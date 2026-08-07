// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/chronic";
const OURS: &str = env!("CARGO_BIN_EXE_chronic");

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
    status: StatusRepr,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

macro_rules! compat_case {
    ($test_name:ident, [$($arg:expr),* $(,)?]) => {
        #[test]
        fn $test_name() {
            let temp = tempfile::tempdir().unwrap();
            assert_compat(stringify!($test_name), &[$($arg),*], b"", temp.path(), &[]);
        }
    };
}

fn base_command<S: AsRef<OsStr>>(program: S, cwd: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn chronic");
    if let Some(mut child_stdin) = child.stdin.take() {
        match child_stdin.write_all(stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to chronic: {err}"),
        }
    }
    let output = child.wait_with_output().expect("wait for chronic");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_chronic(
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

fn run_chronic_without_path(program: &str, args: &[&str], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args).env_remove("PATH");
    finish_command(command, b"")
}

fn run_chronic_with_fd3(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    fd3_path: &Path,
) -> RunOutput {
    let mut command = base_command("/bin/sh", cwd);
    command
        .arg("-c")
        .arg("bin=$1; shift; exec \"$bin\" \"$@\" 3>\"$CHRONIC_FD3_OUT\"")
        .arg("fd3-wrapper")
        .arg(program)
        .args(args)
        .env("CHRONIC_FD3_OUT", fd3_path);
    finish_command(command, stdin)
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path, extra_env: &[(&str, &str)]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_chronic(ORACLE, args, stdin, cwd, extra_env);
    let ours = run_chronic(OURS, args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "chronic compatibility mismatch in {name}\n\
             status: oracle={:?} ours={:?}\n\
             stdout: oracle={} ours={}\n\
             stderr: oracle={} ours={}",
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
    const LIMIT: usize = 256;
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

// CLI parsing.
compat_case!(cli_no_arguments, []);
compat_case!(cli_unknown_option_before_command, ["-x", "true"]);
compat_case!(
    cli_verbose_option,
    ["-v", "sh", "-c", "printf out; printf err >&2; exit 3"]
);
compat_case!(
    cli_stderr_trigger_option,
    ["-e", "sh", "-c", "printf out; printf err >&2; exit 0"]
);
compat_case!(
    cli_combined_ev_options,
    ["-ev", "sh", "-c", "printf out; printf err >&2; exit 0"]
);
compat_case!(
    cli_combined_ve_options,
    ["-ve", "sh", "-c", "printf out; printf err >&2; exit 0"]
);
compat_case!(cli_dashdash_before_command, ["--", "true"]);
compat_case!(cli_baseline_shell_command, ["sh", "-c", "exit 0"]);
compat_case!(cli_verbose_dashdash_before_command, ["-v", "--", "true"]);
compat_case!(cli_triple_dash_unknown_option, ["---", "true"]);
compat_case!(cli_dash_in_option_cluster, ["-v-e", "true"]);
compat_case!(cli_trailing_dash_in_option_cluster, ["-ve-", "true"]);
compat_case!(cli_literal_dash_command, ["-"]);
compat_case!(
    cli_command_not_found,
    ["definitely-not-a-chronic-compat-command"]
);

// Success cases.
compat_case!(success_exit_zero_with_no_output, ["sh", "-c", "exit 0"]);
compat_case!(success_exit_zero_stdout_only, ["sh", "-c", "printf stdout"]);
compat_case!(
    success_exit_zero_stderr_only,
    ["sh", "-c", "printf stderr >&2"]
);
compat_case!(
    success_exit_zero_stdout_and_stderr,
    ["sh", "-c", "printf stdout; printf stderr >&2"]
);
compat_case!(
    success_stderr_trigger_stderr_only,
    ["-e", "sh", "-c", "printf stderr >&2"]
);
compat_case!(
    success_stderr_trigger_stdout_and_stderr,
    ["-e", "sh", "-c", "printf stdout; printf stderr >&2"]
);
compat_case!(
    success_verbose_remains_quiet,
    ["-v", "sh", "-c", "printf stdout; printf stderr >&2"]
);

// Failure cases.
compat_case!(
    failure_exit_1,
    ["sh", "-c", "printf out; printf err >&2; exit 1"]
);
compat_case!(
    failure_exit_2,
    ["sh", "-c", "printf out; printf err >&2; exit 2"]
);
compat_case!(
    failure_exit_42,
    ["sh", "-c", "printf out; printf err >&2; exit 42"]
);
compat_case!(
    failure_exit_127,
    ["sh", "-c", "printf out; printf err >&2; exit 127"]
);
compat_case!(
    failure_stdout_without_trailing_newline,
    ["sh", "-c", "printf stdout-no-newline; exit 5"]
);
compat_case!(
    failure_stderr_without_trailing_newline,
    ["sh", "-c", "printf stderr-no-newline >&2; exit 6"]
);
compat_case!(
    failure_interleaved_stdout_stderr,
    [
        "sh",
        "-c",
        "printf o1; printf e1 >&2; printf o2; printf e2 >&2; exit 9"
    ]
);

// Signal handling.
compat_case!(
    signal_child_killed_by_term,
    ["sh", "-c", "printf out; printf err >&2; kill -TERM $$"]
);
compat_case!(
    signal_child_killed_by_pipe,
    ["sh", "-c", "printf out; printf err >&2; kill -PIPE $$"]
);
compat_case!(
    signal_verbose_child_killed_by_term,
    [
        "-v",
        "sh",
        "-c",
        "printf out; printf err >&2; kill -TERM $$"
    ]
);

// Verbose format.
compat_case!(
    verbose_failing_command,
    ["-v", "sh", "-c", "printf out; printf err >&2; exit 3"]
);
compat_case!(
    verbose_failing_stdout_only,
    ["-v", "sh", "-c", "printf out; exit 4"]
);
compat_case!(
    verbose_failing_stderr_only,
    ["-v", "sh", "-c", "printf err >&2; exit 5"]
);
compat_case!(
    verbose_stderr_trigger,
    ["-ve", "sh", "-c", "printf out; printf err >&2; exit 0"]
);

#[test]
fn stdin_non_empty_is_delivered_to_child() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let text = b"hello on stdin\nsecond line\n";
    std::fs::write(cwd.join("expected-text"), text).unwrap();
    assert_compat(
        "stdin_non_empty_is_delivered_to_child",
        &["sh", "-c", "cmp -s - expected-text"],
        text,
        cwd,
        &[],
    );
}

#[test]
fn stdin_empty_is_delivered_to_child() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "stdin_empty_is_delivered_to_child",
        &["sh", "-c", "test ! -s /dev/stdin"],
        b"",
        temp.path(),
        &[],
    );
}

#[test]
fn stdin_binary_is_delivered_to_child() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let binary = b"\x00\x01\x02\xffbinary\nwith\x00nuls";
    std::fs::write(cwd.join("expected-binary"), binary).unwrap();
    assert_compat(
        "stdin_binary_is_delivered_to_child",
        &["sh", "-c", "cmp -s - expected-binary"],
        binary,
        cwd,
        &[],
    );
}

#[test]
fn stdin_echoed_then_failure_exposes_bytes() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "stdin_echoed_then_failure_exposes_bytes",
        &["sh", "-c", "cat; exit 17"],
        b"visible stdin\n",
        temp.path(),
        &[],
    );
}

#[test]
fn large_outputs_do_not_deadlock_and_match() {
    let temp = tempfile::tempdir().unwrap();
    assert_compat(
        "large_outputs_do_not_deadlock_and_match",
        &[
            "perl",
            "-e",
            "print 'O' x 70000; print STDERR 'E' x 70000; exit 13",
        ],
        b"",
        temp.path(),
        &[],
    );
}

#[test]
fn environment_and_cwd_are_inherited_by_child() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    assert_compat(
        "environment_and_cwd_are_inherited_by_child",
        &[
            "sh",
            "-c",
            "printf 'TOKEN=%s\\nPWD=%s\\n' \"$CHRONIC_COMPAT_TOKEN\" \"$(pwd)\"; exit 8",
        ],
        b"",
        cwd,
        &[("CHRONIC_COMPAT_TOKEN", "token-from-parent")],
    );
}

#[test]
fn unset_path_command_lookup_matches() {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let temp = tempfile::tempdir().unwrap();
    let oracle = run_chronic_without_path(ORACLE, &["true"], temp.path());
    let ours = run_chronic_without_path(OURS, &["true"], temp.path());
    assert_same("unset_path_command_lookup_matches", &oracle, &ours);
}

#[test]
fn executable_text_file_without_shebang_matches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let script = cwd.join("script-without-shebang");
    std::fs::write(&script, b"printf out; printf err >&2; exit 7\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    assert_compat(
        "executable_text_file_without_shebang_by_path",
        &["./script-without-shebang"],
        b"",
        cwd,
        &[],
    );
    assert_compat(
        "executable_text_file_without_shebang_via_path_search",
        &["script-without-shebang"],
        b"",
        cwd,
        &[("PATH", ".:/bin:/usr/bin")],
    );
}

#[test]
fn inherited_fd3_is_preserved_for_child() {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let args = ["sh", "-c", "printf fd3-output >&3 || exit 77"];

    let oracle_fd3 = cwd.join("oracle-fd3");
    let ours_fd3 = cwd.join("ours-fd3");
    let oracle = run_chronic_with_fd3(ORACLE, &args, b"", cwd, &oracle_fd3);
    let ours = run_chronic_with_fd3(OURS, &args, b"", cwd, &ours_fd3);

    assert_same("inherited fd 3 status/stdout/stderr", &oracle, &ours);
    let oracle_fd3 = std::fs::read(&oracle_fd3).unwrap_or_default();
    let ours_fd3 = std::fs::read(&ours_fd3).unwrap_or_default();
    assert_eq!(
        oracle_fd3,
        ours_fd3,
        "fd 3 output mismatch: oracle={} ours={}",
        render_bytes(&oracle_fd3),
        render_bytes(&ours_fd3),
    );
}
