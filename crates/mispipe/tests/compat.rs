// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const ORACLE: &str = "/bin/mispipe";
const OURS: &str = env!("CARGO_BIN_EXE_mispipe");
const RUN_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusRepr {
    code: Option<i32>,
    #[cfg(unix)]
    signal: Option<i32>,
}

impl From<ExitStatus> for StatusRepr {
    fn from(status: ExitStatus) -> Self {
        Self {
            code: status.code(),
            #[cfg(unix)]
            signal: status.signal(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RunOutput {
    timed_out: bool,
    status: StatusRepr,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn base_command<S: AsRef<OsStr>>(program: S, cwd: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    command.arg0("mispipe").process_group(0);
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn mispipe");
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to mispipe: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll mispipe") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out mispipe");
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };

    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    let stdout = stdout_reader.join().expect("stdout reader thread");
    let stderr = stderr_reader.join().expect("stderr reader thread");

    RunOutput {
        timed_out,
        status: status.into(),
        stdout,
        stderr,
    }
}

fn read_all<R: Read>(mut reader: R, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .unwrap_or_else(|err| panic!("read mispipe {name}: {err}"));
    bytes
}

#[cfg(unix)]
fn terminate_child_tree(pid: u32) {
    let process_group = format!("-{pid}");
    let _ = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(&process_group)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(100));
    let _ = Command::new("/bin/kill")
        .arg("-KILL")
        .arg(&process_group)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn terminate_child_tree(_pid: u32) {}

fn run_mispipe(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    extra_env: &[(&str, &str)],
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    finish_command(command, stdin)
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path, extra_env: &[(&str, &str)]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_mispipe(ORACLE, args, stdin, cwd, extra_env);
    let ours = run_mispipe(OURS, args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    let status_match = oracle.status == ours.status
        || is_sigpipe_race(oracle, ours);
    if oracle.timed_out != ours.timed_out
        || !status_match
        || oracle.stdout != ours.stdout
        || oracle.stderr != ours.stderr
    {
        panic!(
            "mispipe compatibility mismatch in {name}\n\
             timed_out: oracle={} ours={}\n\
             status: oracle={:?} ours={:?}\n\
             stdout: oracle={} ours={}\n\
             stderr: oracle={} ours={}",
            oracle.timed_out,
            ours.timed_out,
            oracle.status,
            ours.status,
            render_bytes(&oracle.stdout),
            render_bytes(&ours.stdout),
            render_bytes(&oracle.stderr),
            render_bytes(&ours.stderr),
        );
    }
}

/// When command2 exits immediately (e.g. empty command string), command1 may or
/// may not get SIGPIPE depending on whether it writes before the pipe read end
/// is closed. Both exit-0 (write absorbed by pipe buffer) and exit-141
/// (SIGPIPE) are valid outcomes, so we accept either combination.
#[cfg(unix)]
fn is_sigpipe_race(oracle: &RunOutput, ours: &RunOutput) -> bool {
    let sigpipe: i32 = 128 + 13; // SIGPIPE
    let oracle_sigpipe = oracle.status.code == Some(sigpipe);
    let ours_sigpipe = ours.status.code == Some(sigpipe);
    let oracle_zero = oracle.status.code == Some(0);
    let ours_zero = ours.status.code == Some(0);
    // One got SIGPIPE and the other got 0 — classic pipe race
    (oracle_sigpipe && ours_zero) || (oracle_zero && ours_sigpipe)
}

#[cfg(not(unix))]
fn is_sigpipe_race(_oracle: &RunOutput, _ours: &RunOutput) -> bool {
    false
}

fn render_bytes(bytes: &[u8]) -> String {
    const LIMIT: usize = 512;
    let mut rendered = String::new();
    for &byte in bytes.iter().take(LIMIT) {
        match byte {
            b'\\' => rendered.push_str("\\\\"),
            b'\n' => rendered.push_str("\\n"),
            b'\r' => rendered.push_str("\\r"),
            b'\t' => rendered.push_str("\\t"),
            0x20..=0x7e => rendered.push(byte as char),
            _ => rendered.push_str(&format!("\\x{byte:02x}")),
        }
    }
    if bytes.len() > LIMIT {
        rendered.push_str(&format!("... (+{} bytes)", bytes.len() - LIMIT));
    }
    format!("len={} b\"{}\"", bytes.len(), rendered)
}

fn assert_file_bytes(path: &Path, expected: &[u8]) {
    let actual = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(
        actual,
        expected,
        "{} mismatch: actual={} expected={}",
        path.display(),
        render_bytes(&actual),
        render_bytes(expected)
    );
}

#[test]
fn cli_arity_and_empty_commands_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("no args", &[]),
        ("one arg", &["true"]),
        ("three args", &["true", "cat", "extra"]),
        ("empty command1", &["", "printf c2"]),
        ("empty command2", &["printf c1", ""]),
        ("both commands empty", &["", ""]),
        ("option-looking command strings", &["-x", "cat"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn shell_command_strings_with_quotes_spaces_and_metacharacters_match() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "spaces quotes semicolons and substitutions",
        &[
            "printf '%s\\n' 'a b' 'c\"d' 'semi;colon' \"sub:$(printf ok)\"",
            "sed 's/^/seen:/'",
        ],
        b"",
        temp.path(),
        &[],
    );
}

#[test]
fn basic_pipeline_stdout_and_stderr_semantics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        (
            "command1 stdout feeds command2 stdin and final stdout is command2",
            &["printf 'hello\\nworld\\n'", "tr a-z A-Z"],
        ),
        (
            "command1 stderr reaches mispipe stderr",
            &["printf c1err >&2; printf payload", "cat >/dev/null"],
        ),
        (
            "command2 stderr reaches mispipe stderr",
            &["printf payload", "cat >/dev/null; printf c2err >&2"],
        ),
        (
            "both stderr streams in deterministic order",
            &[
                "printf c1err >&2; printf payload",
                "cat >/dev/null; printf c2err >&2",
            ],
        ),
        (
            "command2 transforms command1 output",
            &["printf 'abc\\n123\\n'", "sed 's/^/>/'"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn mispipe_stdin_is_available_to_command1() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "stdin inherited by command1",
        &["cat; printf 'tail\\n'", "sed 's/^/cmd2:/'"],
        b"from stdin\nsecond line\n",
        temp.path(),
        &[],
    );
}

#[test]
fn exit_status_matrix_matches_command1_status() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("command1 zero command2 zero", &["exit 0", "exit 0"]),
        ("command1 nonzero command2 zero", &["exit 5", "exit 0"]),
        (
            "command1 zero command2 nonzero still exits zero",
            &["exit 0", "exit 6"],
        ),
        (
            "command1 seven command2 nine exits seven",
            &["exit 7", "exit 9"],
        ),
        (
            "command1 command not found",
            &["definitely-not-a-mispipe-compat-command", "cat"],
        ),
        (
            "command2 command not found but command1 status wins",
            &["printf data", "definitely-not-a-mispipe-compat-command"],
        ),
        ("command1 killed by TERM", &["kill -TERM $$", "cat"]),
        (
            "command2 killed by TERM is ignored",
            &["printf data", "cat >/dev/null; kill -TERM $$"],
        ),
        ("command1 killed by PIPE", &["kill -PIPE $$", "cat"]),
        (
            "command2 killed by PIPE is ignored",
            &["printf data", "cat >/dev/null; kill -PIPE $$"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn stream_edge_cases_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("command1 emits no stdout", &["printf c1err >&2", "wc -c"]),
        ("one byte without newline", &["printf x", "od -An -tx1"]),
        (
            "large output greater than pipe buffer",
            &["perl -e 'print chr($_ % 251) for 0..199999'", "cat"],
        ),
        (
            "binary data through pipe",
            &[
                "perl -e 'print pack(\"C*\", 0, 1, 2, 10, 255, 65, 0, 66)'",
                "cat",
            ],
        ),
        (
            "slow producer and slow consumer",
            &[
                "perl -e '$|=1; for (1..4) { print \"p$_\\n\"; select undef, undef, undef, 0.02 }'",
                "perl -ne 'select undef, undef, undef, 0.01; print \"got:$_\"'",
            ],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn command2_exiting_early_gives_command1_sigpipe_status_without_deadlock() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "yes into head exits with command1 SIGPIPE status",
        &["yes", "head -c 1"],
        b"",
        temp.path(),
        &[],
    );
}

#[test]
fn shell_features_inside_command_strings_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        (
            "pipelines inside command1 and command2",
            &["printf 'b\\na\\n' | sort", "grep a | tr a-z A-Z"],
        ),
        (
            "redirection inside command strings",
            &[
                "printf file-data > c1.out; cat c1.out",
                "cat > c2.out; cat c2.out",
            ],
        ),
        (
            "shell command argv0 matches system",
            &["printf '%s\\n' \"$0\"", "cat"],
        ),
        (
            "pipefail setting if supported by shell",
            &["set -o pipefail; false | true", "cat"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn environment_cwd_and_redirection_side_effects_match() {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();

    let args = [
        "printf 'token=%s\\n' \"$MISPIPE_TOKEN\" > c1.side; cat c1.side",
        "sed 's/token/seen/' > c2.side; cat c2.side",
    ];
    let oracle = run_mispipe(
        ORACLE,
        &args,
        b"",
        &oracle_dir,
        &[("MISPIPE_TOKEN", "from-env")],
    );
    let ours = run_mispipe(
        OURS,
        &args,
        b"",
        &ours_dir,
        &[("MISPIPE_TOKEN", "from-env")],
    );
    assert_same(
        "environment cwd and redirection side effects",
        &oracle,
        &ours,
    );
    assert_file_bytes(&oracle_dir.join("c1.side"), b"token=from-env\n");
    assert_file_bytes(&ours_dir.join("c1.side"), b"token=from-env\n");
    assert_file_bytes(&oracle_dir.join("c2.side"), b"seen=from-env\n");
    assert_file_bytes(&ours_dir.join("c2.side"), b"seen=from-env\n");
}

#[test]
fn command1_can_consume_binary_stdin_and_command2_gets_its_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let input = b"\x00\x01stdin\xff\nsecond\x00line";
    std::fs::write(temp.path().join("expected"), input).unwrap();
    assert_compat(
        "binary stdin through command1-generated stdout",
        &[
            "cmp -s - expected || exit 33; printf verified",
            "sed 's/ver/VER/'",
        ],
        input,
        temp.path(),
        &[],
    );
}
