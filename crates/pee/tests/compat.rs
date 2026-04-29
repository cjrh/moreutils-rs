// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const ORACLE: &str = "/bin/pee";
const OURS: &str = env!("CARGO_BIN_EXE_pee");
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
    command.arg0("pee").process_group(0);
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
    let mut child = command.spawn().expect("spawn pee");
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to pee: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll pee") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out pee");
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
        .unwrap_or_else(|err| panic!("read pee {name}: {err}"));
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

fn run_pee(
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
    let oracle = run_pee(ORACLE, args, stdin, cwd, extra_env);
    let ours = run_pee(OURS, args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "pee compatibility mismatch in {name}\n\
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

fn make_large_input() -> Vec<u8> {
    let mut input = Vec::with_capacity(200_000);
    for i in 0..200_000 {
        input.push((i % 251) as u8);
    }
    input
}

#[test]
fn cli_parsing_options_and_command_errors_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("no args empty stdin", &[], b""),
        ("no args consumes stdin", &[], b"ignored input\n"),
        ("one true command", &["true"], b"abc"),
        ("one cat command", &["cat"], b"abc"),
        ("multiple commands without output", &["true", "true", ":"], b"abc"),
        ("empty command string", &[""], b"abc"),
        ("ignore sigpipe option", &["--ignore-sigpipe", "cat"], b"abc"),
        ("no ignore sigpipe option without broken pipe", &["--no-ignore-sigpipe", "cat"], b"abc"),
        ("ignore write errors option", &["--ignore-write-errors", "cat"], b"abc"),
        ("no ignore write errors option without broken pipe", &["--no-ignore-write-errors", "cat"], b"abc"),
        ("option after first command is a command", &["cat", "--ignore-sigpipe"], b"x"),
        ("unknown option is a command", &["--definitely-not-a-pee-option", "cat"], b"x"),
        ("command not found", &["definitely-not-a-pee-compat-command"], b"x"),
    ];

    for (name, args, stdin) in cases {
        assert_compat(name, args, stdin, cwd, &[]);
    }
}

#[test]
fn fan_out_to_every_command_for_text_binary_empty_and_large_input_matches() {
    assert!(Path::new(ORACLE).exists());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("one byte", b"x".to_vec()),
        ("text lines", b"hello\nsecond line\nthird".to_vec()),
        ("binary", b"\x00\x01\xffnot utf8\nwith\x00nul".to_vec()),
        ("large", make_large_input()),
    ];

    for (name, input) in cases {
        let temp = tempfile::tempdir().unwrap();
        let oracle_dir = temp.path().join(format!("oracle-{name}"));
        let ours_dir = temp.path().join(format!("ours-{name}"));
        std::fs::create_dir_all(&oracle_dir).unwrap();
        std::fs::create_dir_all(&ours_dir).unwrap();
        let args = ["cat > c1", "cat > c2", "cat > c3"];

        let oracle = run_pee(ORACLE, &args, &input, &oracle_dir, &[]);
        let ours = run_pee(OURS, &args, &input, &ours_dir, &[]);
        assert_same(name, &oracle, &ours);
        for file in ["c1", "c2", "c3"] {
            assert_file_bytes(&oracle_dir.join(file), &input);
            assert_file_bytes(&ours_dir.join(file), &input);
        }
    }
}

#[test]
fn stdout_stderr_and_no_implicit_echo_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("no implicit stdout without cat", &["true"], b"input that is not echoed\n"),
        ("explicit cat echoes stdin once", &["cat"], b"input that is echoed\n"),
        (
            "single command stdout and stderr after consuming stdin",
            &["cat >/dev/null; printf out; printf err >&2"],
            b"payload\n",
        ),
        (
            "multiple commands deterministic stdout order",
            &[
                "cat >/dev/null; printf 'one\\n'",
                "cat >/dev/null; sleep 0.05; printf 'two\\n'",
                "cat >/dev/null; sleep 0.10; printf 'three\\n'",
            ],
            b"payload\n",
        ),
        (
            "multiple commands deterministic stderr order",
            &[
                "cat >/dev/null; printf 'err-one\\n' >&2",
                "cat >/dev/null; sleep 0.05; printf 'err-two\\n' >&2",
                "cat >/dev/null; sleep 0.10; printf 'err-three\\n' >&2",
            ],
            b"payload\n",
        ),
        (
            "slow command output",
            &["perl -e '$|=1; while (read STDIN, my $b, 4096) {} for (1..3) { print qq(chunk$_\\n); select undef, undef, undef, 0.02 }'"],
            b"payload\n",
        ),
    ];

    for (name, args, stdin) in cases {
        assert_compat(name, args, stdin, cwd, &[]);
    }
}

#[test]
fn exit_status_aggregation_and_child_signals_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("all zero", &["exit 0", "true"]),
        ("one exits one", &["exit 1", "true"]),
        ("multiple statuses are bitwise or", &["exit 1", "exit 2", "exit 4"]),
        ("late command not found", &["true", "definitely-not-a-pee-compat-command"]),
        ("child killed by TERM becomes failure", &["kill -TERM $$"]),
        ("child killed by KILL becomes failure", &["kill -KILL $$"]),
        ("default child SIGPIPE is ignored", &["kill -PIPE $$"]),
        ("explicit ignore child SIGPIPE is ignored", &["--ignore-sigpipe", "kill -PIPE $$"]),
        ("no-ignore child SIGPIPE becomes failure", &["--no-ignore-sigpipe", "kill -PIPE $$"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"trigger\n", cwd, &[]);
    }
}

#[test]
fn sigpipe_and_write_error_options_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let input = make_large_input();
    let cases: &[(&str, &[&str])] = &[
        ("default ignores one early consumer", &["true", "cat"]),
        ("default fails when all consumers close", &["true", "true"]),
        ("explicit ignore write errors", &["--ignore-write-errors", "true", "true"]),
        ("no ignore write errors reports diagnostic", &["--no-ignore-write-errors", "true"]),
        (
            "no ignore write errors stops before later consumers",
            &["--no-ignore-write-errors", "true", "cat"],
        ),
        ("no ignore sigpipe dies from SIGPIPE", &["--no-ignore-sigpipe", "true"]),
        (
            "last sigpipe option wins with ignore",
            &["--no-ignore-sigpipe", "--ignore-sigpipe", "true"],
        ),
        (
            "last sigpipe option wins with no-ignore",
            &["--ignore-sigpipe", "--no-ignore-sigpipe", "true"],
        ),
        (
            "last write-error option wins with ignore",
            &["--no-ignore-write-errors", "--ignore-write-errors", "true"],
        ),
        (
            "last write-error option wins with no-ignore",
            &["--ignore-write-errors", "--no-ignore-write-errors", "true"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, &input, cwd, &[]);
    }
}

#[test]
fn shell_command_semantics_environment_cwd_and_redirections_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    let input = b"shell stdin\nsecond line\n";
    let args = [
        "cat > 'sp ace'; printf 'token=%s lines=%s\\n' \"$PEE_TOKEN\" \"$(wc -l < 'sp ace')\"",
        "printf 'b\\na\\n' | sort > sorted; cat >/dev/null; sleep 0.05; printf 'sorted=%s\\n' \"$(tr -d '\\n' < sorted)\"",
        "cat >/dev/null; printf '%s\\n' 'quoted arg with spaces' 'semi;colon' 'star:*' > quoted.out",
    ];

    let oracle = run_pee(
        ORACLE,
        &args,
        input,
        &oracle_dir,
        &[("PEE_TOKEN", "from-env")],
    );
    let ours = run_pee(
        OURS,
        &args,
        input,
        &ours_dir,
        &[("PEE_TOKEN", "from-env")],
    );
    assert_same("shell command semantics", &oracle, &ours);
    for dir in [&oracle_dir, &ours_dir] {
        assert_file_bytes(&dir.join("sp ace"), input);
        assert_file_bytes(&dir.join("sorted"), b"a\nb\n");
        assert_file_bytes(
            &dir.join("quoted.out"),
            b"quoted arg with spaces\nsemi;colon\nstar:*\n",
        );
    }
}
