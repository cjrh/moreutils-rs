// SPDX-License-Identifier: GPL-2.0-only

#![cfg(unix)]

use nix::pty::{OpenptyResult, openpty};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const ORACLE: &str = "/bin/vipe";
const OURS: &str = env!("CARGO_BIN_EXE_vipe");
const RUN_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusRepr {
    code: Option<i32>,
    signal: Option<i32>,
}

impl From<ExitStatus> for StatusRepr {
    fn from(status: ExitStatus) -> Self {
        Self {
            code: status.code(),
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
    tty: Vec<u8>,
}

fn base_command<S: AsRef<OsStr>>(program: S, cwd: &Path) -> (Command, OwnedFd, OwnedFd) {
    let OpenptyResult { master, slave } = openpty(None, None).expect("openpty");
    let slave_fd = slave.as_raw_fd();
    let mut command = Command::new(program);
    // SAFETY: The closure runs between fork and exec and invokes only the
    // async-signal-safe `setsid` and TIOCSCTTY operations on a live slave FD.
    // `slave` remains owned by the parent until after `spawn` completes.
    unsafe {
        command.arg0("/bin/vipe").pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (command, master, slave)
}

fn run_vipe(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    extra_env: &[(&str, &str)],
) -> RunOutput {
    let (mut command, master, slave) = base_command(program, cwd);
    command.args(args);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    finish_command(command, master, slave, stdin)
}

fn run_vipe_with_stdout_closed(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    extra_env: &[(&str, &str)],
) -> RunOutput {
    let (mut command, master, slave) = base_command(program, cwd);
    command.args(args);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    finish_command_with_stdout_closed(command, master, slave, stdin)
}

fn finish_command(
    mut command: Command,
    master: OwnedFd,
    slave: OwnedFd,
    stdin: &[u8],
) -> RunOutput {
    let mut child = command.spawn().expect("spawn vipe");
    drop(slave);
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to vipe: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));
    let tty_reader = thread::spawn(move || read_pty(master));

    let (status, timed_out) = wait_with_timeout(&mut child, pid);

    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: stdout_reader.join().expect("stdout reader thread"),
        stderr: stderr_reader.join().expect("stderr reader thread"),
        tty: tty_reader.join().expect("tty reader thread"),
    }
}

fn finish_command_with_stdout_closed(
    mut command: Command,
    master: OwnedFd,
    slave: OwnedFd,
    stdin: &[u8],
) -> RunOutput {
    let mut child = command.spawn().expect("spawn vipe");
    drop(slave);
    let pid = child.id();
    drop(child.stdout.take());

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to vipe: {err}"),
        })
    });
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));
    let tty_reader = thread::spawn(move || read_pty(master));

    let (status, timed_out) = wait_with_timeout(&mut child, pid);
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: Vec::new(),
        stderr: stderr_reader.join().expect("stderr reader thread"),
        tty: tty_reader.join().expect("tty reader thread"),
    }
}

fn wait_with_timeout(child: &mut std::process::Child, pid: u32) -> (ExitStatus, bool) {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll vipe") {
            return (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            return (child.wait().expect("wait for timed-out vipe"), true);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_all<R: Read>(mut reader: R, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .unwrap_or_else(|err| panic!("read vipe {name}: {err}"));
    bytes
}

fn read_pty(master: OwnedFd) -> Vec<u8> {
    let mut file = File::from(master);
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(err) if err.raw_os_error() == Some(libc::EIO) => break,
            Err(err) => panic!("read vipe tty: {err}"),
        }
    }
    bytes
}

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

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "vipe compatibility mismatch in {name}\n\
             timed_out: oracle={} ours={}\n\
             status: oracle={:?} ours={:?}\n\
             stdout: oracle={} ours={}\n\
             stderr: oracle={} ours={}\n\
             tty: oracle={} ours={}",
            oracle.timed_out,
            ours.timed_out,
            oracle.status,
            ours.status,
            render_bytes(&oracle.stdout),
            render_bytes(&ours.stdout),
            render_bytes(&oracle.stderr),
            render_bytes(&ours.stderr),
            render_bytes(&oracle.tty),
            render_bytes(&ours.tty),
        );
    }
}

fn assert_same_except_path_stderr(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    assert_eq!(
        oracle.timed_out, ours.timed_out,
        "timed_out mismatch in {name}"
    );
    assert_eq!(oracle.status, ours.status, "status mismatch in {name}");
    assert_eq!(oracle.stdout, ours.stdout, "stdout mismatch in {name}");
    assert_eq!(oracle.tty, ours.tty, "tty mismatch in {name}");
    assert_eq!(
        normalize_missing_temp_stderr(&oracle.stderr),
        normalize_missing_temp_stderr(&ours.stderr),
        "stderr mismatch in {name}\noracle={}\nours={}",
        render_bytes(&oracle.stderr),
        render_bytes(&ours.stderr)
    );
}

fn normalize_missing_temp_stderr(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if let Some(start) = text.find("cannot read ") {
        if let Some(rest) = text[start + "cannot read ".len()..].find(": No such file or directory")
        {
            let before = &text[..start + "cannot read ".len()];
            let after = &text[start + "cannot read ".len() + rest..];
            return format!("{before}<temp>{after}");
        }
    }
    text.into_owned()
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

fn make_editor(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn make_large_input() -> Vec<u8> {
    let mut input = Vec::with_capacity(200_000);
    for i in 0..200_000 {
        input.push((i % 251) as u8);
    }
    input
}

#[test]
fn cli_parsing_suffix_extra_args_and_diagnostics_match() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();

    for (name, args) in [
        ("unknown long option", vec!["--help"]),
        ("unknown short option", vec!["-x"]),
        ("missing suffix argument", vec!["--suffix"]),
        ("bad option after suffix", vec!["--suffix=csv", "--bad"]),
    ] {
        let oracle = run_vipe(ORACLE, &args, b"ignored", cwd, &[]);
        let ours = run_vipe(OURS, &args, b"ignored", cwd, &[]);
        assert_same(name, &oracle, &ours);
    }

    let editor = make_editor(
        cwd,
        "suffix-editor.sh",
        "case \"$1\" in *.csv) printf csv > \"$1\" ;; *.rs) printf rs > \"$1\" ;; *) printf plain > \"$1\" ;; esac",
    );
    let editor_value = editor.to_str().unwrap();
    for (name, args, expected) in [
        (
            "long suffix with ignored extra arg",
            vec!["--suffix", "csv", "extra"],
            b"csv".to_vec(),
        ),
        (
            "abbreviated single dash suffix",
            vec!["-S", ".rs"],
            b"rs".to_vec(),
        ),
        (
            "dashdash stops option parsing",
            vec!["--", "--suffix", "csv"],
            b"plain".to_vec(),
        ),
    ] {
        let oracle = run_vipe(ORACLE, &args, b"input", cwd, &[("EDITOR", editor_value)]);
        let ours = run_vipe(OURS, &args, b"input", cwd, &[("EDITOR", editor_value)]);
        assert_same(name, &oracle, &ours);
        assert_eq!(oracle.stdout, expected, "oracle stdout in {name}");
        assert_eq!(ours.stdout, expected, "ours stdout in {name}");
    }
}

#[test]
fn stdin_data_editor_modifications_and_unchanged_files_match() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
    let temp = tempfile::tempdir().unwrap();
    let edited = temp.path().join("edited.bin");
    let edited_bytes = b"edited\0bytes\xff\nwithout assumption";
    fs::write(&edited, edited_bytes).unwrap();
    let editor = make_editor(
        temp.path(),
        "binary-editor.sh",
        "cp \"$1\" \"$RECORD\"\ncat \"$EDITED\" > \"$1\"",
    );
    let large = make_large_input();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty stdin", Vec::new()),
        ("one byte", b"x".to_vec()),
        ("text without trailing newline", b"hello\nworld".to_vec()),
        ("multiple lines", b"a\nb\nc\n".to_vec()),
        ("binary invalid utf8", b"\x00\x01\xffnot utf8\n".to_vec()),
        ("large stdin", large),
    ];

    for (name, input) in cases {
        let oracle_record = temp.path().join(format!("oracle-{name}.record"));
        let ours_record = temp.path().join(format!("ours-{name}.record"));
        let common_env = [
            ("EDITOR", editor.to_str().unwrap()),
            ("EDITED", edited.to_str().unwrap()),
        ];
        let oracle = run_vipe(
            ORACLE,
            &[],
            &input,
            temp.path(),
            &[
                common_env[0],
                common_env[1],
                ("RECORD", oracle_record.to_str().unwrap()),
            ],
        );
        let ours = run_vipe(
            OURS,
            &[],
            &input,
            temp.path(),
            &[
                common_env[0],
                common_env[1],
                ("RECORD", ours_record.to_str().unwrap()),
            ],
        );
        assert_same(name, &oracle, &ours);
        assert_eq!(
            oracle.stdout, edited_bytes,
            "oracle edited output in {name}"
        );
        assert_eq!(
            fs::read(&oracle_record).unwrap(),
            input,
            "oracle editor input in {name}"
        );
        assert_eq!(
            fs::read(&ours_record).unwrap(),
            input,
            "ours editor input in {name}"
        );
    }

    let unchanged = make_editor(temp.path(), "unchanged-editor.sh", "cp \"$1\" \"$RECORD\"");
    let input = b"left unchanged\n";
    let oracle_record = temp.path().join("oracle-unchanged.record");
    let ours_record = temp.path().join("ours-unchanged.record");
    let oracle = run_vipe(
        ORACLE,
        &[],
        input,
        temp.path(),
        &[
            ("EDITOR", unchanged.to_str().unwrap()),
            ("RECORD", oracle_record.to_str().unwrap()),
        ],
    );
    let ours = run_vipe(
        OURS,
        &[],
        input,
        temp.path(),
        &[
            ("EDITOR", unchanged.to_str().unwrap()),
            ("RECORD", ours_record.to_str().unwrap()),
        ],
    );
    assert_same("editor leaves file unchanged", &oracle, &ours);
    assert_eq!(oracle.stdout, input);
    assert_eq!(fs::read(&oracle_record).unwrap(), input);
    assert_eq!(fs::read(&ours_record).unwrap(), input);
}

#[test]
fn editor_selection_argument_splitting_visual_precedence_and_failures_match() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
    let temp = tempfile::tempdir().unwrap();
    let editor = make_editor(
        temp.path(),
        "editor-with-args.sh",
        "test \"$1\" = --mode || exit 11\ntest \"$2\" = pipe || exit 12\nprintf editor > \"$3\"",
    );
    let editor_value = format!("{} --mode pipe", editor.display());
    let oracle = run_vipe(
        ORACLE,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", editor_value.as_str())],
    );
    let ours = run_vipe(
        OURS,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", editor_value.as_str())],
    );
    assert_same("EDITOR split into argv", &oracle, &ours);
    assert_eq!(oracle.stdout, b"editor");

    let visual = make_editor(
        temp.path(),
        "visual-with-args.sh",
        "test \"$1\" = --visual || exit 13\nprintf visual > \"$2\"",
    );
    let failing_editor = make_editor(temp.path(), "must-not-run.sh", "exit 99");
    let visual_value = format!("{} --visual", visual.display());
    let oracle = run_vipe(
        ORACLE,
        &[],
        b"input",
        temp.path(),
        &[
            ("EDITOR", failing_editor.to_str().unwrap()),
            ("VISUAL", visual_value.as_str()),
        ],
    );
    let ours = run_vipe(
        OURS,
        &[],
        b"input",
        temp.path(),
        &[
            ("EDITOR", failing_editor.to_str().unwrap()),
            ("VISUAL", visual_value.as_str()),
        ],
    );
    assert_same("VISUAL takes precedence", &oracle, &ours);
    assert_eq!(oracle.stdout, b"visual");

    let fail = make_editor(temp.path(), "fail.sh", "exit 42");
    let oracle = run_vipe(
        ORACLE,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", fail.to_str().unwrap())],
    );
    let ours = run_vipe(
        OURS,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", fail.to_str().unwrap())],
    );
    assert_same("failing editor", &oracle, &ours);
    assert_eq!(oracle.stdout, b"");

    let oracle = run_vipe(
        ORACLE,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", "/no/such/vipe-editor")],
    );
    let ours = run_vipe(
        OURS,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", "/no/such/vipe-editor")],
    );
    assert_same("missing editor executable", &oracle, &ours);
    assert_eq!(oracle.stdout, b"");
}

#[test]
fn tempfile_tmpdir_permissions_suffix_and_cleanup_match() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
    let temp = tempfile::tempdir().unwrap();
    let oracle_tmp = temp.path().join("oracle-tmp");
    let ours_tmp = temp.path().join("ours-tmp");
    fs::create_dir_all(&oracle_tmp).unwrap();
    fs::create_dir_all(&ours_tmp).unwrap();
    let oracle_record = temp.path().join("oracle-temp.record");
    let ours_record = temp.path().join("ours-temp.record");
    let editor = make_editor(
        temp.path(),
        "temp-props.sh",
        "printf '%s\n' \"$1\" > \"$RECORD\"\nstat -c '%a' \"$1\" >> \"$RECORD\"\nprintf done > \"$1\"",
    );

    let oracle = run_vipe(
        ORACLE,
        &["--suffix=txt"],
        b"input",
        temp.path(),
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("RECORD", oracle_record.to_str().unwrap()),
            ("TMPDIR", oracle_tmp.to_str().unwrap()),
        ],
    );
    let ours = run_vipe(
        OURS,
        &["--suffix=txt"],
        b"input",
        temp.path(),
        &[
            ("EDITOR", editor.to_str().unwrap()),
            ("RECORD", ours_record.to_str().unwrap()),
            ("TMPDIR", ours_tmp.to_str().unwrap()),
        ],
    );
    assert_same("TMPDIR permissions suffix cleanup", &oracle, &ours);
    assert_eq!(oracle.stdout, b"done");

    for (record, tmpdir) in [(oracle_record, oracle_tmp), (ours_record, ours_tmp)] {
        let text = String::from_utf8(fs::read(&record).unwrap()).unwrap();
        let mut lines = text.lines();
        let temp_path = PathBuf::from(lines.next().unwrap());
        let mode = lines.next().unwrap();
        assert!(
            temp_path.starts_with(&tmpdir),
            "{} should be under {}",
            temp_path.display(),
            tmpdir.display()
        );
        assert!(
            temp_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".txt"),
            "{} should carry suffix",
            temp_path.display()
        );
        assert_eq!(mode, "600", "temp mode for {}", temp_path.display());
        assert!(
            !temp_path.exists(),
            "temp file should be removed after vipe exits"
        );
    }
}

#[test]
fn editor_deletes_temp_and_broken_stdout_pipe_match() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
    let temp = tempfile::tempdir().unwrap();
    let deleter = make_editor(temp.path(), "delete-temp.sh", "rm \"$1\"");
    let oracle = run_vipe(
        ORACLE,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", deleter.to_str().unwrap())],
    );
    let ours = run_vipe(
        OURS,
        &[],
        b"input",
        temp.path(),
        &[("EDITOR", deleter.to_str().unwrap())],
    );
    assert_same_except_path_stderr("editor deletes temp file", &oracle, &ours);

    let passthrough = make_editor(temp.path(), "passthrough.sh", "exit 0");
    let large = make_large_input();
    let oracle = run_vipe_with_stdout_closed(
        ORACLE,
        &[],
        &large,
        temp.path(),
        &[("EDITOR", passthrough.to_str().unwrap())],
    );
    let ours = run_vipe_with_stdout_closed(
        OURS,
        &[],
        &large,
        temp.path(),
        &[("EDITOR", passthrough.to_str().unwrap())],
    );
    assert_same("broken stdout pipe", &oracle, &ours);
}
