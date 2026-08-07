// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/ifne";
const OURS: &str = env!("CARGO_BIN_EXE_ifne");

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

fn base_command<S: AsRef<OsStr>>(program: S, cwd: &Path) -> Command {
    let mut command = Command::new(program);
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
    let mut child = command.spawn().expect("spawn ifne");
    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        std::thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to ifne: {err}"),
        })
    });
    let output = child.wait_with_output().expect("wait for ifne");
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_ifne(
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

fn run_ifne_without_path(program: &str, args: &[&str], stdin: &[u8], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args).env_remove("PATH");
    finish_command(command, stdin)
}

fn run_ifne_from_stdin_path(
    program: &str,
    args: &[&str],
    stdin_path: &Path,
    cwd: &Path,
) -> RunOutput {
    let stdin = File::open(stdin_path).expect("open stdin path");
    let mut command = base_command(program, cwd);
    command.args(args).stdin(Stdio::from(stdin));
    let output = command.output().expect("run ifne with file stdin");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_ifne_with_stdout_closed(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    let mut child = command.spawn().expect("spawn ifne with closed stdout");
    drop(child.stdout.take());
    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        std::thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to ifne: {err}"),
        })
    });
    let output = child.wait_with_output().expect("wait for ifne");
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path, extra_env: &[(&str, &str)]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_ifne(ORACLE, args, stdin, cwd, extra_env);
    let ours = run_ifne(OURS, args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "ifne compatibility mismatch in {name}\n\
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

fn make_large_input(byte: u8) -> Vec<u8> {
    let mut input = Vec::with_capacity(150_000);
    for i in 0..150_000 {
        input.push(byte.wrapping_add((i % 251) as u8));
    }
    input
}

#[test]
fn cli_parsing_usage_and_exec_errors_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("no args", &[], b""),
        ("only -n", &["-n"], b""),
        (
            "unknown option is just command when input is non-empty",
            &["-x", "true"],
            b"x",
        ),
        (
            "unknown option command skipped when input is empty",
            &["-x", "true"],
            b"",
        ),
        ("dashdash is just command", &["--", "true"], b"x"),
        (
            "command not found",
            &["definitely-not-an-ifne-compat-command"],
            b"x",
        ),
        (
            "reverse command not found on empty input",
            &["-n", "definitely-not-an-ifne-compat-command"],
            b"",
        ),
        (
            "reverse option-looking command not found",
            &["-n", "-x"],
            b"",
        ),
    ];

    for (name, args, stdin) in cases {
        assert_compat(name, args, stdin, cwd, &[]);
    }
}

#[test]
fn default_mode_empty_input_skips_child() {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    let args = [
        "sh",
        "-c",
        "printf ran > marker; printf out; printf err >&2; exit 17",
    ];

    let oracle = run_ifne(ORACLE, &args, b"", &oracle_dir, &[]);
    let ours = run_ifne(OURS, &args, b"", &ours_dir, &[]);
    assert_same("default_mode_empty_input_skips_child", &oracle, &ours);
    assert!(!oracle_dir.join("marker").exists(), "oracle child ran");
    assert!(!ours_dir.join("marker").exists(), "ours child ran");
}

#[test]
fn default_mode_nonempty_input_runs_child_and_forwards_all_bytes() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("one byte", b"x".to_vec()),
        ("no trailing newline", b"no trailing newline".to_vec()),
        ("text lines", b"hello\nsecond line\n".to_vec()),
        ("binary", b"\x00\x01\xffnot utf8\nwith\x00nul".to_vec()),
        ("large", make_large_input(0x11)),
    ];

    for (name, input) in cases {
        let temp = tempfile::tempdir().unwrap();
        let oracle_dir = temp.path().join("oracle");
        let ours_dir = temp.path().join("ours");
        std::fs::create_dir_all(&oracle_dir).unwrap();
        std::fs::create_dir_all(&ours_dir).unwrap();
        let args = [
            "sh",
            "-c",
            "cat > seen; printf child-out; printf child-err >&2; exit 23",
        ];

        let oracle = run_ifne(ORACLE, &args, &input, &oracle_dir, &[]);
        let ours = run_ifne(OURS, &args, &input, &ours_dir, &[]);
        assert_same(name, &oracle, &ours);
        assert_file_bytes(&oracle_dir.join("seen"), &input);
        assert_file_bytes(&ours_dir.join("seen"), &input);
    }
}

#[test]
fn reverse_mode_empty_input_runs_child_with_empty_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    let args = [
        "-n",
        "sh",
        "-c",
        "cat > seen; printf reverse-out; printf reverse-err >&2; exit 24",
    ];

    let oracle = run_ifne(ORACLE, &args, b"", &oracle_dir, &[]);
    let ours = run_ifne(OURS, &args, b"", &ours_dir, &[]);
    assert_same(
        "reverse_mode_empty_input_runs_child_with_empty_stdin",
        &oracle,
        &ours,
    );
    assert_file_bytes(&oracle_dir.join("seen"), b"");
    assert_file_bytes(&ours_dir.join("seen"), b"");
}

#[test]
fn reverse_mode_nonempty_input_is_passed_through_and_child_is_skipped() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("one byte", b"x".to_vec()),
        ("text", b"already has data\n".to_vec()),
        ("binary", b"\x00\xffbinary\n".to_vec()),
        ("large", make_large_input(0x80)),
    ];

    for (name, input) in cases {
        let temp = tempfile::tempdir().unwrap();
        let oracle_dir = temp.path().join("oracle");
        let ours_dir = temp.path().join("ours");
        std::fs::create_dir_all(&oracle_dir).unwrap();
        std::fs::create_dir_all(&ours_dir).unwrap();
        let args = ["-n", "sh", "-c", "printf ran > marker; exit 99"];

        let oracle = run_ifne(ORACLE, &args, &input, &oracle_dir, &[]);
        let ours = run_ifne(OURS, &args, &input, &ours_dir, &[]);
        assert_same(name, &oracle, &ours);
        assert!(!oracle_dir.join("marker").exists(), "oracle child ran");
        assert!(!ours_dir.join("marker").exists(), "ours child ran");
    }
}

#[test]
fn child_exit_status_signals_and_streams_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("child exits 0", &["sh", "-c", "cat >/dev/null; exit 0"]),
        ("child exits 1", &["sh", "-c", "cat >/dev/null; exit 1"]),
        ("child exits 42", &["sh", "-c", "cat >/dev/null; exit 42"]),
        ("child exits 127", &["sh", "-c", "cat >/dev/null; exit 127"]),
        (
            "child stdout stderr and failure",
            &[
                "sh",
                "-c",
                "cat >/dev/null; printf out; printf err >&2; exit 7",
            ],
        ),
        (
            "child killed by TERM",
            &["sh", "-c", "cat >/dev/null; kill -TERM $$"],
        ),
        (
            "child killed by PIPE",
            &["sh", "-c", "cat >/dev/null; kill -PIPE $$"],
        ),
        (
            "child writes streams then signal",
            &[
                "sh",
                "-c",
                "cat >/dev/null; printf out; printf err >&2; kill -TERM $$",
            ],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"trigger\n", cwd, &[]);
    }
}

#[test]
fn command_arguments_environment_cwd_and_path_lookup_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    assert_compat(
        "arguments environment and cwd",
        &[
            "sh",
            "-c",
            "printf 'argv0=%s arg1=%s token=%s pwd=%s\\n' \"$0\" \"$1\" \"$IFNE_TOKEN\" \"$(pwd)\"; cat",
            "custom-argv0",
            "-n-looking-arg",
        ],
        b"stdin survives\n",
        cwd,
        &[("IFNE_TOKEN", "from-parent")],
    );

    assert!(Path::new(ORACLE).exists());
    let oracle = run_ifne_without_path(ORACLE, &["true"], b"x", cwd);
    let ours = run_ifne_without_path(OURS, &["true"], b"x", cwd);
    assert_same("unset PATH command lookup", &oracle, &ours);
}

#[test]
fn executable_text_files_and_option_like_program_names_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let script = cwd.join("script-without-shebang");
    std::fs::write(&script, b"printf 'script:%s:' \"$1\"; cat\n").unwrap();
    let dash_n = cwd.join("-n");
    std::fs::write(&dash_n, b"printf dash-n; cat\n").unwrap();
    for path in [&script, &dash_n] {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    let script_path = script.to_str().unwrap();
    assert_compat(
        "executable text file without shebang by path",
        &[script_path, "arg"],
        b"stdin\n",
        cwd,
        &[],
    );
    assert_compat(
        "executable text file without shebang via PATH",
        &["script-without-shebang", "arg"],
        b"stdin\n",
        cwd,
        &[("PATH", ".:/bin:/usr/bin")],
    );
    assert_compat(
        "program named dash-n can be run by explicit path",
        &["./-n"],
        b"stdin\n",
        cwd,
        &[],
    );
}

#[test]
fn stdin_sources_and_read_errors_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let input = cwd.join("input");
    std::fs::write(&input, b"from file\n").unwrap();

    let oracle = run_ifne_from_stdin_path(ORACLE, &["cat"], &input, cwd);
    let ours = run_ifne_from_stdin_path(OURS, &["cat"], &input, cwd);
    assert_same("stdin from regular file", &oracle, &ours);

    let oracle = run_ifne_from_stdin_path(ORACLE, &["cat"], Path::new("/dev/null"), cwd);
    let ours = run_ifne_from_stdin_path(OURS, &["cat"], Path::new("/dev/null"), cwd);
    assert_same("stdin from /dev/null", &oracle, &ours);

    let oracle = run_ifne_from_stdin_path(ORACLE, &["cat"], cwd, cwd);
    let ours = run_ifne_from_stdin_path(OURS, &["cat"], cwd, cwd);
    assert_same("stdin read error from directory fd", &oracle, &ours);
}

#[test]
fn child_closing_stdin_early_matches() {
    let temp = tempfile::tempdir().unwrap();
    let input = make_large_input(0x33);
    assert_compat(
        "child closes stdin early",
        &["true"],
        &input,
        temp.path(),
        &[],
    );
}

#[test]
fn reverse_mode_stdout_closed_while_passing_through_matches() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let input = make_large_input(0x44);
    let oracle = run_ifne_with_stdout_closed(ORACLE, &["-n", "cat"], &input, temp.path());
    let ours = run_ifne_with_stdout_closed(OURS, &["-n", "cat"], &input, temp.path());
    assert_same("reverse mode stdout closed", &oracle, &ours);
}
