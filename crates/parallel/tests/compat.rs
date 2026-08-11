// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const OURS: &str = env!("CARGO_BIN_EXE_parallel");
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

fn oracle_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MOREUTILS_PARALLEL_ORACLE") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    // GNU parallel diverts moreutils' binary to `parallel.moreutils` on Debian
    // and Ubuntu. Prefer it so compatibility tests never select GNU parallel.
    for path in [
        "/bin/parallel.moreutils",
        "/usr/bin/parallel.moreutils",
        "/bin/parallel",
        "/usr/bin/parallel",
    ] {
        let path = PathBuf::from(path);
        if path.exists() && !is_gnu_parallel(&path) {
            return Some(path);
        }
    }
    None
}

fn is_gnu_parallel(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).starts_with("GNU parallel"))
        .unwrap_or(false)
}

fn require_oracle() -> Option<PathBuf> {
    let oracle = oracle_path();
    if oracle.is_none() {
        eprintln!(
            "skipping parallel compatibility test: set MOREUTILS_PARALLEL_ORACLE or install moreutils' parallel"
        );
    }
    oracle
}

fn base_command<S: AsRef<OsStr>>(program: S, cwd: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    command.arg0("parallel").process_group(0);
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
    let mut child = command.spawn().expect("spawn parallel");
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to parallel: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll parallel") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out parallel");
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
        .unwrap_or_else(|err| panic!("read parallel {name}: {err}"));
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

fn run_parallel(
    program: &Path,
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
    let Some(oracle) = require_oracle() else {
        return;
    };
    let oracle = run_parallel(&oracle, args, stdin, cwd, extra_env);
    let ours = run_parallel(Path::new(OURS), args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "parallel compatibility mismatch in {name}\n\
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

fn assert_same_unordered_stdout_lines(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    let oracle_lines = sorted_lines(&oracle.stdout);
    let ours_lines = sorted_lines(&ours.stdout);
    if oracle.timed_out != ours.timed_out
        || oracle.status != ours.status
        || oracle.stderr != ours.stderr
        || oracle_lines != ours_lines
    {
        panic!(
            "parallel compatibility mismatch in {name}\n\
             timed_out: oracle={} ours={}\n\
             status: oracle={:?} ours={:?}\n\
             stdout lines: oracle={:?} ours={:?}\n\
             stderr: oracle={} ours={}",
            oracle.timed_out,
            ours.timed_out,
            oracle.status,
            ours.status,
            oracle_lines
                .iter()
                .map(|line| render_bytes(line))
                .collect::<Vec<_>>(),
            ours_lines
                .iter()
                .map(|line| render_bytes(line))
                .collect::<Vec<_>>(),
            render_bytes(&oracle.stderr),
            render_bytes(&ours.stderr),
        );
    }
}

fn sorted_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = bytes
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| line.to_vec())
        .collect();
    lines.sort();
    lines
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

fn read_counts(path: &Path) -> Vec<usize> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    text.lines()
        .map(|line| line.parse::<usize>().unwrap())
        .collect()
}

#[test]
fn cli_parsing_and_option_diagnostics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("no args", &[]),
        ("missing separator silently runs no jobs", &["echo", "a"]),
        ("separator with no commands", &["--"]),
        ("unknown option", &["-x", "--", "true"]),
        ("short help", &["-h"]),
        ("long-looking help is invalid short option", &["--help"]),
        ("dashdash-looking bad option", &["--bad"]),
        ("j missing", &["-j"]),
        ("j non-numeric", &["-j", "x", "echo", "--", "a"]),
        ("j attached", &["-j2", "echo", "--", "a"]),
        ("j zero sequential", &["-j", "0", "echo", "--", "a"]),
        (
            "j negative attached unlimited",
            &["-j-1", "echo", "--", "a"],
        ),
        ("l missing", &["-l"]),
        ("l non-numeric", &["-l", "x", "echo", "--", "a"]),
        ("l attached high load", &["-l999", "echo", "--", "a"]),
        ("n missing", &["-n"]),
        ("n non-numeric", &["-n", "x", "echo", "--", "a"]),
        ("n zero", &["-n", "0", "echo", "--", "a"]),
        ("n attached", &["-j1", "-n2", "echo", "--", "a", "b", "c"]),
        (
            "i and n incompatible",
            &["-i", "-n", "2", "echo", "{}", "--", "a"],
        ),
        ("clustered i j", &["-ij2", "echo", "{}", "--", "a"]),
        ("n without command", &["-n", "2", "--", "echo a"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn command_plus_arguments_mode_matches_sequentially() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        (
            "echo appends one arg",
            &["-j1", "echo", "--", "a", "b", "c"],
        ),
        (
            "arguments are literal bytes not shell syntax",
            &[
                "-j1", "printf", "<%s>\\n", "--", "a b", "*", "\"q\"", "x\ty", "",
            ],
        ),
        (
            "argument appended after fixed command arguments",
            &[
                "-j1",
                "sh",
                "-c",
                "printf '%s:%s\\n' \"$0\" \"$1\"",
                "fixed",
                "--",
                "a",
                "b",
            ],
        ),
        (
            "n two final partial group",
            &[
                "-j1",
                "-n",
                "2",
                "printf",
                "<%s><%s>\\n",
                "--",
                "a",
                "b",
                "c",
            ],
        ),
        (
            "n three final partial group",
            &[
                "-j1",
                "-n3",
                "printf",
                "%s,%s,%s\\n",
                "--",
                "a",
                "b",
                "c",
                "d",
                "e",
            ],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn replacement_mode_matches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        (
            "replace token in one command word",
            &["-j1", "-i", "echo", "pre{}post", "--", "a", "b"],
        ),
        (
            "replace multiple words and occurrences",
            &[
                "-j1",
                "-i",
                "printf",
                "<%s>:%s\\n",
                "{}{}",
                "X{}Y",
                "--",
                "a",
                "b c",
                "x/y",
                "a&b",
            ],
        ),
        (
            "no replacement token still runs once per argument",
            &["-j1", "-i", "echo", "fixed", "--", "a", "b"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn independent_shell_commands_match_sequentially() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    // Upstream parallel forwards output through helper processes and terminates
    // them immediately after the final job exits. Keep output-producing jobs
    // alive briefly so their bytes are drained before that cleanup race.
    let cases: &[(&str, &[&str])] = &[
        (
            "simple independent shell commands",
            &["-j1", "--", "printf A; sleep 0.2", "printf B; sleep 0.2"],
        ),
        (
            "variables pipelines redirections and empty command",
            &[
                "-j1",
                "--",
                "X=ok; printf \"$X\\n\"; sleep 0.2",
                "printf 'b\\na\\n' | sort; sleep 0.2",
                "printf side > file; cat file; sleep 0.2",
                "",
            ],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn exit_status_aggregation_matches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("all zero", &["-j1", "--", "exit 0", "true"]),
        ("one exits one", &["-j1", "--", "exit 1"]),
        (
            "multiple failures are bitwise or",
            &["--", "exit 1", "exit 2", "exit 4"],
        ),
        (
            "signal contributes upstream failure value",
            &["--", "kill -TERM $$"],
        ),
        (
            "command mode failures are bitwise or",
            &["-j1", "sh", "-c", "exit \"$1\"", "job", "--", "1", "2", "4"],
        ),
        (
            "exec failure in command mode",
            &["notfound-parallel-compat", "--", "a", "b"],
        ),
        (
            "command not found in shell command mode",
            &["--", "notfound-parallel-compat"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn concurrent_command_output_matches_as_unordered_lines() {
    let Some(oracle) = require_oracle() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let args = [
        "-j0",
        "sh",
        "-c",
        "printf 'out:%s\\n' \"$1\"; printf 'err:%s\\n' \"$1\" >&2",
        "job",
        "--",
        "a",
        "b",
        "c",
        "d",
        "e",
    ];
    let oracle = run_parallel(&oracle, &args, b"", cwd, &[]);
    let ours = run_parallel(Path::new(OURS), &args, b"", cwd, &[]);
    let oracle_err = sorted_lines(&oracle.stderr);
    let ours_err = sorted_lines(&ours.stderr);
    assert_eq!(oracle_err, ours_err, "stderr lines differ");
    let oracle_without_stderr = RunOutput {
        stderr: Vec::new(),
        ..oracle
    };
    let ours_without_stderr = RunOutput {
        stderr: Vec::new(),
        ..ours
    };
    assert_same_unordered_stdout_lines(
        "concurrent stdout/stderr unordered lines",
        &oracle_without_stderr,
        &ours_without_stderr,
    );
}

#[test]
fn maxjobs_limits_concurrency_and_zero_allows_more() {
    let Some(oracle) = require_oracle() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();

    let script = r#"
        use Fcntl qw(:flock);
        my $name = $ARGV[0];
        mkdir "active";
        open my $lock, ">>", "lock" or die $!;
        flock $lock, LOCK_EX;
        open my $active, ">", "active/$name" or die $!;
        close $active;
        opendir my $dh, "active" or die $!;
        my @active = grep { !/^\./ } readdir $dh;
        open my $counts, ">>", "counts" or die $!;
        print $counts scalar(@active), "\n";
        close $counts;
        flock $lock, LOCK_UN;
        select undef, undef, undef, 0.15;
        flock $lock, LOCK_EX;
        unlink "active/$name";
        flock $lock, LOCK_UN;
    "#;

    for (name, args, expected_max, expected_at_least) in [
        (
            "j1",
            vec!["-j1", "perl", "-e", script, "--", "a", "b", "c"],
            1,
            1,
        ),
        (
            "j2",
            vec!["-j2", "perl", "-e", script, "--", "a", "b", "c", "d"],
            2,
            2,
        ),
        (
            "j0",
            vec!["-j0", "perl", "-e", script, "--", "a", "b", "c", "d"],
            1,
            1,
        ),
        (
            "jneg",
            vec!["-j-1", "perl", "-e", script, "--", "a", "b", "c", "d"],
            4,
            2,
        ),
    ] {
        let oracle_case_dir = oracle_dir.join(name);
        let ours_case_dir = ours_dir.join(name);
        std::fs::create_dir_all(&oracle_case_dir).unwrap();
        std::fs::create_dir_all(&ours_case_dir).unwrap();
        let oracle_output = run_parallel(&oracle, &args, b"", &oracle_case_dir, &[]);
        let ours_output = run_parallel(Path::new(OURS), &args, b"", &ours_case_dir, &[]);
        assert_same(name, &oracle_output, &ours_output);

        for (which, dir) in [("oracle", &oracle_case_dir), ("ours", &ours_case_dir)] {
            let counts = read_counts(&dir.join("counts"));
            assert_eq!(counts.len(), args.len() - 5, "{which} {name} job count");
            let max = counts.iter().copied().max().unwrap_or(0);
            assert!(
                max <= expected_max,
                "{which} {name} exceeded max concurrency: counts={counts:?}"
            );
            assert!(
                max >= expected_at_least,
                "{which} {name} did not reach expected concurrency: counts={counts:?}"
            );
        }
    }
}

#[test]
fn child_stdin_is_inherited_by_command_jobs() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "sequential jobs consume parent stdin",
        &[
            "-j1",
            "sh",
            "-c",
            "read x || x=EOF; printf '%s:%s\\n' \"$1\" \"$x\"",
            "job",
            "--",
            "a",
            "b",
        ],
        b"line1\nline2\n",
        temp.path(),
        &[],
    );
}

#[test]
fn high_load_limit_starts_jobs_normally() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "high maxload runs immediately",
        &["-l999", "echo", "--", "a"],
        b"",
        temp.path(),
        &[],
    );
}
