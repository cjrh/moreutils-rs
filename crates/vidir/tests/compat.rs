// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const ORACLE: &str = "/bin/vidir";
const OURS: &str = env!("CARGO_BIN_EXE_vidir");
const RUN_TIMEOUT: Duration = Duration::from_secs(5);

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
    command.arg0("/bin/vidir").process_group(0);
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

fn run_vidir(
    program: &str,
    args: &[&str],
    cwd: &Path,
    editor: &Path,
    extra_env: &[(&str, &str)],
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args).env("EDITOR", editor);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    finish_command(command, b"")
}

fn run_vidir_with_stdin(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    editor: &Path,
) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args).env("EDITOR", editor);
    finish_command(command, stdin)
}

fn run_vidir_no_editor(program: &str, args: &[&str], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args).stdin(Stdio::null());
    finish_command_no_stdin_writer(command)
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn vidir");
    let pid = child.id();

    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to vidir: {err}"),
        })
    });

    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let (status, timed_out) = wait_with_timeout(&mut child, pid);

    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: stdout_reader.join().expect("stdout reader thread"),
        stderr: stderr_reader.join().expect("stderr reader thread"),
    }
}

fn finish_command_no_stdin_writer(mut command: Command) -> RunOutput {
    let mut child = command.spawn().expect("spawn vidir");
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));
    let (status, timed_out) = wait_with_timeout(&mut child, pid);
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: stdout_reader.join().expect("stdout reader thread"),
        stderr: stderr_reader.join().expect("stderr reader thread"),
    }
}

fn wait_with_timeout(child: &mut std::process::Child, pid: u32) -> (ExitStatus, bool) {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll vidir") {
            return (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            return (child.wait().expect("wait for timed-out vidir"), true);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_all<R: Read>(mut reader: R, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .unwrap_or_else(|err| panic!("read vidir {name}: {err}"));
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

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "vidir compatibility mismatch in {name}\n\
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

fn assert_listing_numbers_equivalent(oracle: &[u8], ours: &[u8]) {
    fn normalize(bytes: &[u8]) -> Vec<(usize, String)> {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(|line| {
                let (number, path) = line.split_once('\t').expect("item number and path");
                (
                    number.parse().expect("numeric item number"),
                    path.to_owned(),
                )
            })
            .collect()
    }

    assert_eq!(normalize(oracle), normalize(ours));
}

fn make_editor(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn run_pair(
    temp: &Path,
    args: &[&str],
    editor_body: &str,
    setup: impl Fn(&Path),
) -> (RunOutput, RunOutput, PathBuf, PathBuf) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let editor = make_editor(temp, "editor.sh", editor_body);
    let oracle_dir = temp.join("oracle");
    let ours_dir = temp.join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    setup(&oracle_dir);
    setup(&ours_dir);
    let oracle = run_vidir(ORACLE, args, &oracle_dir, &editor, &[]);
    let ours = run_vidir(OURS, args, &ours_dir, &editor, &[]);
    (oracle, ours, oracle_dir, ours_dir)
}

fn assert_snapshots_equal(name: &str, oracle_dir: &Path, ours_dir: &Path) {
    let oracle = snapshot(oracle_dir);
    let ours = snapshot(ours_dir);
    assert_eq!(oracle, ours, "filesystem snapshot mismatch in {name}");
}

fn snapshot(root: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    collect_snapshot(root, root, &mut entries);
    entries.sort();
    entries
}

fn collect_snapshot(root: &Path, dir: &Path, entries: &mut Vec<String>) {
    let mut children: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    children.sort();
    for path in children {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.file_type().is_symlink() {
            entries.push(format!(
                "L {rel} -> {}",
                fs::read_link(&path).unwrap().display()
            ));
        } else if metadata.is_dir() {
            entries.push(format!("D {rel}"));
            collect_snapshot(root, &path, entries);
        } else if metadata.is_file() {
            entries.push(format!(
                "F {rel} {}",
                render_bytes(&fs::read(&path).unwrap())
            ));
        } else {
            entries.push(format!("? {rel}"));
        }
    }
}

#[test]
fn listing_format_sorting_hidden_files_and_visual_precedence_match() {
    let temp = tempfile::tempdir().unwrap();
    let visual = make_editor(
        temp.path(),
        "visual.sh",
        "test \"$1\" = ARG || exit 7\ncp \"$2\" \"$RECORD\"\nexit 0",
    );
    let editor = make_editor(temp.path(), "editor.sh", "exit 99");
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("b"), b"").unwrap();
        fs::write(dir.join("a"), b"").unwrap();
        fs::write(dir.join(".hidden"), b"").unwrap();
        fs::create_dir(dir.join("dir")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a", dir.join("link")).unwrap();
    }

    let oracle_record = temp.path().join("oracle.record");
    let ours_record = temp.path().join("ours.record");
    let visual_value = format!("{} ARG", visual.display());
    let oracle = run_vidir(
        ORACLE,
        &[],
        &oracle_dir,
        &editor,
        &[
            ("VISUAL", visual_value.as_str()),
            ("RECORD", oracle_record.to_str().unwrap()),
        ],
    );
    let ours = run_vidir(
        OURS,
        &[],
        &ours_dir,
        &editor,
        &[
            ("VISUAL", visual_value.as_str()),
            ("RECORD", ours_record.to_str().unwrap()),
        ],
    );
    assert_same("listing format and VISUAL precedence", &oracle, &ours);
    let oracle_listing = fs::read(&oracle_record).unwrap();
    let ours_listing = fs::read(&ours_record).unwrap();
    // Ubuntu's older vidir leaves numbers unpadded; newer releases pad them.
    // Number/path pairing is the portable upstream contract.
    assert_listing_numbers_equivalent(&oracle_listing, &ours_listing);

    #[cfg(unix)]
    let expected = "0001\t./.hidden\n0002\t./a\n0003\t./b\n0004\t./dir\n0005\t./link\n";
    #[cfg(not(unix))]
    let expected = "0001\t./.hidden\n0002\t./a\n0003\t./b\n0004\t./dir\n";
    assert_eq!(String::from_utf8(ours_listing).unwrap(), expected);
}

#[test]
fn explicit_dirs_files_missing_paths_and_dashdash_paths_match() {
    let temp = tempfile::tempdir().unwrap();
    let record_oracle = temp.path().join("oracle.record");
    let record_ours = temp.path().join("ours.record");
    let body = "cp \"$1\" \"$RECORD\"";
    let editor = make_editor(temp.path(), "record.sh", body);
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    for dir in [&oracle_dir, &ours_dir] {
        fs::create_dir(dir.join("d")).unwrap();
        fs::write(dir.join("d/b"), b"").unwrap();
        fs::write(dir.join("d/a"), b"").unwrap();
        fs::write(dir.join("z"), b"").unwrap();
        fs::write(dir.join("-foo"), b"").unwrap();
    }

    let args = &["d", "z", "missing", "--", "-foo"];
    let oracle = run_vidir(
        ORACLE,
        args,
        &oracle_dir,
        &editor,
        &[("RECORD", record_oracle.to_str().unwrap())],
    );
    let ours = run_vidir(
        OURS,
        args,
        &ours_dir,
        &editor,
        &[("RECORD", record_ours.to_str().unwrap())],
    );
    assert_same("explicit args and --", &oracle, &ours);
    let oracle_listing = fs::read(&record_oracle).unwrap();
    let ours_listing = fs::read(&record_ours).unwrap();
    assert_listing_numbers_equivalent(&oracle_listing, &ours_listing);
    assert_eq!(
        String::from_utf8(ours_listing).unwrap(),
        "0001\td/a\n0002\td/b\n0003\tz\n0004\tmissing\n0005\t-foo\n"
    );
}

#[test]
fn renames_deletes_parent_creation_spaces_and_symlinks_match() {
    let temp = tempfile::tempdir().unwrap();
    let body = "cat > \"$1\" <<'EOF'\n0001\t./new dir/sub/a file\n0004\t./renamed link\n0005\t./target\nEOF";
    let (oracle, ours, oracle_dir, ours_dir) = run_pair(temp.path(), &[], body, |dir| {
        fs::write(dir.join("a"), b"A").unwrap();
        fs::write(dir.join("b"), b"B").unwrap();
        fs::create_dir(dir.join("dir")).unwrap();
        fs::write(dir.join("target"), b"T").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("target", dir.join("link")).unwrap();
        #[cfg(not(unix))]
        fs::write(dir.join("link"), b"L").unwrap();
    });
    assert_same("rename delete create parents symlink", &oracle, &ours);
    assert_snapshots_equal(
        "rename delete create parents symlink",
        &oracle_dir,
        &ours_dir,
    );
    assert_eq!(
        fs::read(oracle_dir.join("new dir/sub/a file")).unwrap(),
        b"A"
    );
    assert!(!oracle_dir.join("b").exists());
    assert!(!oracle_dir.join("dir").exists());
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(oracle_dir.join("renamed link")).unwrap(),
        Path::new("target")
    );
}

#[test]
fn verbose_swaps_and_existing_target_backups_match() {
    let temp = tempfile::tempdir().unwrap();
    let body = "cat > \"$1\" <<'EOF'\n0001\t./b\n0002\t./a\n0003\t./b~\nEOF";
    let (oracle, ours, oracle_dir, ours_dir) = run_pair(temp.path(), &["-v"], body, |dir| {
        fs::write(dir.join("a"), b"A").unwrap();
        fs::write(dir.join("b"), b"B").unwrap();
        fs::write(dir.join("b~"), b"T").unwrap();
    });
    assert_same("verbose swap", &oracle, &ours);
    assert_snapshots_equal("verbose swap", &oracle_dir, &ours_dir);
    assert_eq!(fs::read(oracle_dir.join("a")).unwrap(), b"B");
    assert_eq!(fs::read(oracle_dir.join("b")).unwrap(), b"A");
    assert_eq!(fs::read(oracle_dir.join("b~")).unwrap(), b"T");
}

#[test]
fn deletions_empty_names_blank_lines_and_non_empty_dir_errors_match() {
    let temp = tempfile::tempdir().unwrap();
    let body = "cat > \"$1\" <<'EOF'\n\n   \n0001\t./a\n0002\t\nEOF";
    let (oracle, ours, oracle_dir, ours_dir) = run_pair(temp.path(), &[], body, |dir| {
        fs::write(dir.join("a"), b"A").unwrap();
        fs::create_dir(dir.join("d")).unwrap();
        fs::write(dir.join("d/x"), b"X").unwrap();
    });
    assert_same("non-empty directory deletion", &oracle, &ours);
    assert_snapshots_equal("non-empty directory deletion", &oracle_dir, &ours_dir);
    assert!(oracle_dir.join("d/x").exists());
}

#[cfg(unix)]
#[test]
fn hard_links_get_distinct_item_numbers_match() {
    let temp = tempfile::tempdir().unwrap();
    let record_oracle = temp.path().join("oracle.record");
    let record_ours = temp.path().join("ours.record");
    let editor = make_editor(temp.path(), "record-hardlinks.sh", "cp \"$1\" \"$RECORD\"");
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    for dir in [&oracle_dir, &ours_dir] {
        fs::write(dir.join("a"), b"A").unwrap();
        fs::hard_link(dir.join("a"), dir.join("z")).unwrap();
    }
    let oracle = run_vidir(
        ORACLE,
        &[],
        &oracle_dir,
        &editor,
        &[("RECORD", record_oracle.to_str().unwrap())],
    );
    let ours = run_vidir(
        OURS,
        &[],
        &ours_dir,
        &editor,
        &[("RECORD", record_ours.to_str().unwrap())],
    );
    assert_same("hard links", &oracle, &ours);
    let oracle_listing = fs::read(&record_oracle).unwrap();
    let ours_listing = fs::read(&record_ours).unwrap();
    assert_listing_numbers_equivalent(&oracle_listing, &ours_listing);
    assert_eq!(
        String::from_utf8(ours_listing).unwrap(),
        "0001\t./a\n0002\t./z\n"
    );
}

#[test]
fn malformed_duplicate_unknown_options_editor_failures_and_control_names_match() {
    let temp = tempfile::tempdir().unwrap();

    let bad_line = "cat > \"$1\" <<'EOF'\nnot a vidir line\nEOF";
    let (oracle, ours, oracle_dir, ours_dir) = run_pair(temp.path(), &[], bad_line, |dir| {
        fs::write(dir.join("a"), b"A").unwrap();
    });
    assert_same("malformed editor line", &oracle, &ours);
    assert_snapshots_equal("malformed editor line", &oracle_dir, &ours_dir);

    let dup_temp = tempfile::tempdir().unwrap();
    let duplicate = "cat > \"$1\" <<'EOF'\n0001\t./a\n0001\t./a\nEOF";
    let (oracle, ours, oracle_dir, ours_dir) = run_pair(dup_temp.path(), &[], duplicate, |dir| {
        fs::write(dir.join("a"), b"A").unwrap();
        fs::write(dir.join("b"), b"B").unwrap();
    });
    assert_same("duplicate item number", &oracle, &ours);
    assert_snapshots_equal("duplicate item number", &oracle_dir, &ours_dir);

    let stdin_temp = tempfile::tempdir().unwrap();
    let editor = make_editor(stdin_temp.path(), "stdin-noop.sh", "exit 0");
    let oracle_dir = stdin_temp.path().join("oracle");
    let ours_dir = stdin_temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    fs::write(oracle_dir.join("a"), b"A").unwrap();
    fs::write(ours_dir.join("a"), b"A").unwrap();
    let oracle = run_vidir_with_stdin(ORACLE, &["-"], b"a\n", &oracle_dir, &editor);
    let ours = run_vidir_with_stdin(OURS, &["-"], b"a\n", &ours_dir, &editor);
    assert_same("stdin dash without controlling tty", &oracle, &ours);
    assert_snapshots_equal("stdin dash without controlling tty", &oracle_dir, &ours_dir);

    let opt_temp = tempfile::tempdir().unwrap();
    let oracle_dir = opt_temp.path().join("oracle");
    let ours_dir = opt_temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    let oracle = run_vidir_no_editor(ORACLE, &["-x"], &oracle_dir);
    let ours = run_vidir_no_editor(OURS, &["-x"], &ours_dir);
    assert_same("unknown option", &oracle, &ours);

    let fail_temp = tempfile::tempdir().unwrap();
    let failing_editor = make_editor(fail_temp.path(), "fail.sh", "exit 42");
    let oracle_dir = fail_temp.path().join("oracle");
    let ours_dir = fail_temp.path().join("ours");
    fs::create_dir_all(&oracle_dir).unwrap();
    fs::create_dir_all(&ours_dir).unwrap();
    fs::write(oracle_dir.join("a"), b"A").unwrap();
    fs::write(ours_dir.join("a"), b"A").unwrap();
    let oracle = run_vidir(ORACLE, &[], &oracle_dir, &failing_editor, &[]);
    let ours = run_vidir(OURS, &[], &ours_dir, &failing_editor, &[]);
    assert_same("failing editor", &oracle, &ours);
    assert_snapshots_equal("failing editor", &oracle_dir, &ours_dir);

    #[cfg(unix)]
    {
        let ctrl_temp = tempfile::tempdir().unwrap();
        let editor = make_editor(ctrl_temp.path(), "noop.sh", "exit 0");
        let oracle_dir = ctrl_temp.path().join("oracle");
        let ours_dir = ctrl_temp.path().join("ours");
        fs::create_dir_all(&oracle_dir).unwrap();
        fs::create_dir_all(&ours_dir).unwrap();
        fs::write(oracle_dir.join("a\tb"), b"").unwrap();
        fs::write(ours_dir.join("a\tb"), b"").unwrap();
        let oracle = run_vidir(ORACLE, &[], &oracle_dir, &editor, &[]);
        let ours = run_vidir(OURS, &[], &ours_dir, &editor, &[]);
        assert_same("control character filename", &oracle, &ours);
        assert_snapshots_equal("control character filename", &oracle_dir, &ours_dir);
    }
}
