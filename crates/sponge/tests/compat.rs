// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const ORACLE: &str = "/bin/sponge";
const OURS: &str = env!("CARGO_BIN_EXE_sponge");
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
    command.arg0("sponge").process_group(0);
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

#[cfg(unix)]
fn set_child_umask(command: &mut Command, mask: libc::mode_t) {
    unsafe {
        command.pre_exec(move || {
            libc::umask(mask);
            Ok(())
        });
    }
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn sponge");
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to sponge: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll sponge") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out sponge");
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
        .unwrap_or_else(|err| panic!("read sponge {name}: {err}"));
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

fn run_sponge(program: &str, args: &[&str], stdin: &[u8], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    finish_command(command, stdin)
}

#[cfg(unix)]
fn run_sponge_with_umask(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    mask: libc::mode_t,
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    set_child_umask(&mut command, mask);
    finish_command(command, stdin)
}

fn run_sponge_from_stdin_path(
    program: &str,
    args: &[&str],
    stdin_path: &Path,
    cwd: &Path,
) -> RunOutput {
    let stdin = File::open(stdin_path).expect("open stdin path");
    let mut command = base_command(program, cwd);
    command.args(args).stdin(Stdio::from(stdin));
    finish_command_no_stdin_writer(command)
}

fn finish_command_no_stdin_writer(mut command: Command) -> RunOutput {
    let mut child = command.spawn().expect("spawn sponge");
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll sponge") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out sponge");
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };

    RunOutput {
        timed_out,
        status: status.into(),
        stdout: stdout_reader.join().expect("stdout reader thread"),
        stderr: stderr_reader.join().expect("stderr reader thread"),
    }
}

fn run_sponge_with_stdout_closed(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    let mut child = command.spawn().expect("spawn sponge with closed stdout");
    let pid = child.id();
    drop(child.stdout.take());

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to sponge: {err}"),
        })
    });

    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));
    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll sponge") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out sponge");
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: Vec::new(),
        stderr: stderr_reader.join().expect("stderr reader thread"),
    }
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_sponge(ORACLE, args, stdin, cwd);
    let ours = run_sponge(OURS, args, stdin, cwd);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "sponge compatibility mismatch in {name}\n\
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
    let actual = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
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
fn stdout_mode_and_cli_diagnostics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let large = make_large_input();
    let cases: Vec<(&str, Vec<&str>, Vec<u8>)> = vec![
        ("no args empty stdin", vec![], Vec::new()),
        ("no args one byte", vec![], b"x".to_vec()),
        (
            "no args text no trailing newline",
            vec![],
            b"hello\nworld".to_vec(),
        ),
        ("no args binary", vec![], b"\x00\x01\xffnot utf8\n".to_vec()),
        ("no args large", vec![], large),
        (
            "append option without file still writes stdout",
            vec!["-a"],
            b"abc".to_vec(),
        ),
        ("unknown option without file", vec!["-x"], b"abc".to_vec()),
        (
            "clustered unknown option without file",
            vec!["-zx"],
            b"abc".to_vec(),
        ),
    ];

    for (name, args, stdin) in cases {
        assert_compat(name, &args, &stdin, cwd);
    }
}

#[test]
fn help_and_getopt_style_parsing_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("plain help", &["-h"]),
        ("help after append in same cluster", &["-ah"]),
        ("help before append in same cluster", &["-ha"]),
        ("long-help is getopt characters", &["--help"]),
        ("dashdash without filename", &["--"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"ignored", cwd);
    }
}

#[test]
fn filename_selection_option_permutation_and_extra_args_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();

    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("first"), b"OLD").unwrap();
        fs::write(dir.join("second"), b"SECOND").unwrap();
        fs::write(dir.join("after"), b"OLD").unwrap();
    }

    let oracle = run_sponge(ORACLE, &["first", "second"], b"one", &oracle_dir);
    let ours = run_sponge(OURS, &["first", "second"], b"one", &ours_dir);
    assert_same("extra output files are ignored", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("first"), b"one");
    assert_file_bytes(&ours_dir.join("first"), b"one");
    assert_file_bytes(&oracle_dir.join("second"), b"SECOND");
    assert_file_bytes(&ours_dir.join("second"), b"SECOND");

    let oracle = run_sponge(ORACLE, &["after", "-a"], b"+", &oracle_dir);
    let ours = run_sponge(OURS, &["after", "-a"], b"+", &ours_dir);
    assert_same("option after filename is still parsed", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("after"), b"OLD+");
    assert_file_bytes(&ours_dir.join("after"), b"OLD+");

    let oracle = run_sponge(ORACLE, &["-x", "unknown-file"], b"payload", &oracle_dir);
    let ours = run_sponge(OURS, &["-x", "unknown-file"], b"payload", &ours_dir);
    assert_same(
        "unknown option diagnostic while still using filename",
        &oracle,
        &ours,
    );
    assert_file_bytes(&oracle_dir.join("unknown-file"), b"payload");
    assert_file_bytes(&ours_dir.join("unknown-file"), b"payload");

    let oracle = run_sponge(ORACLE, &["--", "-a"], b"literal", &oracle_dir);
    let ours = run_sponge(OURS, &["--", "-a"], b"literal", &ours_dir);
    assert_same("dashdash makes -a a filename", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("-a"), b"literal");
    assert_file_bytes(&ours_dir.join("-a"), b"literal");
}

#[test]
fn regular_file_overwrite_modes_and_in_place_input_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("one byte", b"x".to_vec()),
        ("text", b"hello\nwithout trailing".to_vec()),
        ("binary", b"\x00\x01\xffwith\x00nul".to_vec()),
        ("large", make_large_input()),
    ];

    for (name, input) in cases {
        let oracle_file = format!("oracle-{name}");
        let ours_file = format!("ours-{name}");
        let oracle = run_sponge(ORACLE, &[&oracle_file], &input, &oracle_dir);
        let ours = run_sponge(OURS, &[&ours_file], &input, &ours_dir);
        assert_same(name, &oracle, &ours);
        assert_file_bytes(&oracle_dir.join(oracle_file), &input);
        assert_file_bytes(&ours_dir.join(ours_file), &input);
    }

    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("existing"), b"old").unwrap();
        #[cfg(unix)]
        fs::set_permissions(dir.join("existing"), fs::Permissions::from_mode(0o640)).unwrap();
    }
    #[cfg(unix)]
    let oracle_inode_before = fs::metadata(oracle_dir.join("existing")).unwrap().ino();
    #[cfg(unix)]
    let ours_inode_before = fs::metadata(ours_dir.join("existing")).unwrap().ino();
    let oracle = run_sponge(ORACLE, &["existing"], b"new", &oracle_dir);
    let ours = run_sponge(OURS, &["existing"], b"new", &ours_dir);
    assert_same("existing regular file", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("existing"), b"new");
    assert_file_bytes(&ours_dir.join("existing"), b"new");
    #[cfg(unix)]
    {
        let oracle_meta = fs::metadata(oracle_dir.join("existing")).unwrap();
        let ours_meta = fs::metadata(ours_dir.join("existing")).unwrap();
        assert_eq!(oracle_meta.permissions().mode() & 0o777, 0o640);
        assert_eq!(ours_meta.permissions().mode() & 0o777, 0o640);
        assert_ne!(
            oracle_meta.ino(),
            oracle_inode_before,
            "oracle inode changed"
        );
        assert_ne!(ours_meta.ino(), ours_inode_before, "ours inode changed");
    }

    #[cfg(unix)]
    {
        let oracle = run_sponge_with_umask(ORACLE, &["umask-new"], b"u", &oracle_dir, 0o077);
        let ours = run_sponge_with_umask(OURS, &["umask-new"], b"u", &ours_dir, 0o077);
        assert_same("new file respects umask", &oracle, &ours);
        assert_eq!(
            fs::metadata(oracle_dir.join("umask-new"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(ours_dir.join("umask-new"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    fs::write(oracle_dir.join("same"), b"same file input\n").unwrap();
    fs::write(ours_dir.join("same"), b"same file input\n").unwrap();
    let oracle =
        run_sponge_from_stdin_path(ORACLE, &["same"], &oracle_dir.join("same"), &oracle_dir);
    let ours = run_sponge_from_stdin_path(OURS, &["same"], &ours_dir.join("same"), &ours_dir);
    assert_same("stdin from same file as output", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("same"), b"same file input\n");
    assert_file_bytes(&ours_dir.join("same"), b"same file input\n");
}

#[test]
fn append_mode_regular_files_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();

    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("append"), b"OLD").unwrap();
        fs::write(dir.join("empty"), b"KEEP").unwrap();
        fs::write(dir.join("self"), b"AB").unwrap();
        #[cfg(unix)]
        fs::set_permissions(dir.join("append"), fs::Permissions::from_mode(0o600)).unwrap();
    }

    let oracle = run_sponge(ORACLE, &["-a", "append"], b"+NEW", &oracle_dir);
    let ours = run_sponge(OURS, &["-a", "append"], b"+NEW", &ours_dir);
    assert_same("append existing", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("append"), b"OLD+NEW");
    assert_file_bytes(&ours_dir.join("append"), b"OLD+NEW");
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(oracle_dir.join("append"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(ours_dir.join("append"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    let oracle_empty_inode = fs::metadata(oracle_dir.join("empty")).unwrap().ino();
    #[cfg(unix)]
    let ours_empty_inode = fs::metadata(ours_dir.join("empty")).unwrap().ino();
    let oracle = run_sponge(ORACLE, &["-a", "empty"], b"", &oracle_dir);
    let ours = run_sponge(OURS, &["-a", "empty"], b"", &ours_dir);
    assert_same("append empty stdin", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("empty"), b"KEEP");
    assert_file_bytes(&ours_dir.join("empty"), b"KEEP");
    #[cfg(unix)]
    {
        assert_ne!(
            fs::metadata(oracle_dir.join("empty")).unwrap().ino(),
            oracle_empty_inode
        );
        assert_ne!(
            fs::metadata(ours_dir.join("empty")).unwrap().ino(),
            ours_empty_inode
        );
    }

    let oracle = run_sponge(ORACLE, &["-a", "created"], b"created", &oracle_dir);
    let ours = run_sponge(OURS, &["-a", "created"], b"created", &ours_dir);
    assert_same("append creates file", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("created"), b"created");
    assert_file_bytes(&ours_dir.join("created"), b"created");

    let oracle = run_sponge_from_stdin_path(
        ORACLE,
        &["-a", "self"],
        &oracle_dir.join("self"),
        &oracle_dir,
    );
    let ours = run_sponge_from_stdin_path(OURS, &["-a", "self"], &ours_dir.join("self"), &ours_dir);
    assert_same("append stdin from same file", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("self"), b"ABAB");
    assert_file_bytes(&ours_dir.join("self"), b"ABAB");
}

#[test]
fn error_paths_and_stdin_read_errors_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(oracle_dir.join("adir")).unwrap();
    fs::create_dir_all(ours_dir.join("adir")).unwrap();

    let cases: &[(&str, &[&str])] = &[
        ("missing parent", &["missing/child"]),
        ("directory output", &["adir"]),
        ("append missing parent", &["-a", "missing/child"]),
        ("append directory output", &["-a", "adir"]),
    ];
    for (name, args) in cases {
        let oracle = run_sponge(ORACLE, args, b"data", &oracle_dir);
        let ours = run_sponge(OURS, args, b"data", &ours_dir);
        assert_same(name, &oracle, &ours);
    }

    let oracle = run_sponge_from_stdin_path(ORACLE, &["out"], &oracle_dir, &oracle_dir);
    let ours = run_sponge_from_stdin_path(OURS, &["out"], &ours_dir, &ours_dir);
    assert_same("stdin read error from directory", &oracle, &ours);
    assert!(!oracle_dir.join("out").exists());
    assert!(!ours_dir.join("out").exists());

    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        let no_write_oracle = oracle_dir.join("no-write");
        let no_write_ours = ours_dir.join("no-write");
        fs::create_dir_all(&no_write_oracle).unwrap();
        fs::create_dir_all(&no_write_ours).unwrap();
        fs::set_permissions(&no_write_oracle, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&no_write_ours, fs::Permissions::from_mode(0o555)).unwrap();
        let oracle = run_sponge(ORACLE, &["child"], b"data", &no_write_oracle);
        let ours = run_sponge(OURS, &["child"], b"data", &no_write_ours);
        fs::set_permissions(&no_write_oracle, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&no_write_ours, fs::Permissions::from_mode(0o755)).unwrap();
        assert_same("permission denied", &oracle, &ours);
    }
}

#[cfg(unix)]
#[test]
fn symlinks_special_files_and_fifos_match() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();

    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("target"), b"OLD").unwrap();
        fs::set_permissions(dir.join("target"), fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink("target", dir.join("link")).unwrap();
    }
    let oracle_inode = fs::metadata(oracle_dir.join("target")).unwrap().ino();
    let ours_inode = fs::metadata(ours_dir.join("target")).unwrap().ino();
    let oracle = run_sponge(ORACLE, &["link"], b"new", &oracle_dir);
    let ours = run_sponge(OURS, &["link"], b"new", &ours_dir);
    assert_same("symlink overwrite", &oracle, &ours);
    assert_eq!(
        fs::read_link(oracle_dir.join("link")).unwrap(),
        Path::new("target")
    );
    assert_eq!(
        fs::read_link(ours_dir.join("link")).unwrap(),
        Path::new("target")
    );
    assert_file_bytes(&oracle_dir.join("target"), b"new");
    assert_file_bytes(&ours_dir.join("target"), b"new");
    assert_eq!(
        fs::metadata(oracle_dir.join("target")).unwrap().ino(),
        oracle_inode
    );
    assert_eq!(
        fs::metadata(ours_dir.join("target")).unwrap().ino(),
        ours_inode
    );

    fs::write(oracle_dir.join("target"), b"OLD").unwrap();
    fs::write(ours_dir.join("target"), b"OLD").unwrap();
    let oracle = run_sponge(ORACLE, &["-a", "link"], b"X", &oracle_dir);
    let ours = run_sponge(OURS, &["-a", "link"], b"X", &ours_dir);
    assert_same("symlink append truncates through link", &oracle, &ours);
    assert_file_bytes(&oracle_dir.join("target"), b"X");
    assert_file_bytes(&ours_dir.join("target"), b"X");

    let oracle = run_sponge(ORACLE, &["/dev/null"], b"devnull", &oracle_dir);
    let ours = run_sponge(OURS, &["/dev/null"], b"devnull", &ours_dir);
    assert_same("/dev/null output", &oracle, &ours);

    let oracle_fifo = run_fifo_case(ORACLE, &oracle_dir, b"fifo bytes\n");
    let ours_fifo = run_fifo_case(OURS, &ours_dir, b"fifo bytes\n");
    assert_same("fifo output", &oracle_fifo.0, &ours_fifo.0);
    assert_eq!(oracle_fifo.1, b"fifo bytes\n");
    assert_eq!(ours_fifo.1, b"fifo bytes\n");
    assert!(
        fs::symlink_metadata(oracle_dir.join("fifo"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert!(
        fs::symlink_metadata(ours_dir.join("fifo"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[cfg(unix)]
fn run_fifo_case(program: &str, cwd: &Path, input: &[u8]) -> (RunOutput, Vec<u8>) {
    let fifo = cwd.join("fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo {}", fifo.display());

    let reader_path = fifo.clone();
    let reader = thread::spawn(move || {
        let mut file = File::open(&reader_path).expect("open fifo reader");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read fifo");
        bytes
    });
    let output = run_sponge(program, &["fifo"], input, cwd);
    let bytes = reader.join().expect("fifo reader thread");
    (output, bytes)
}

#[cfg(unix)]
#[test]
fn broken_stdout_pipe_status_matches() {
    assert!(Path::new(ORACLE).exists());
    let temp = tempfile::tempdir().unwrap();
    let input = make_large_input();
    let oracle = run_sponge_with_stdout_closed(ORACLE, &[], &input, temp.path());
    let ours = run_sponge_with_stdout_closed(OURS, &[], &input, temp.path());
    assert_same("stdout closed", &oracle, &ours);
}
