// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/combine";
const OURS: &str = env!("CARGO_BIN_EXE_combine");

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
    let mut child = command.spawn().expect("spawn combine");
    if let Some(mut child_stdin) = child.stdin.take() {
        match child_stdin.write_all(stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to combine: {err}"),
        }
    }
    let output = child.wait_with_output().expect("wait for combine");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_combine(program: &str, args: &[&str], stdin: &[u8], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    finish_command(command, stdin)
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_combine(ORACLE, args, stdin, cwd);
    let ours = run_combine(OURS, args, stdin, cwd);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "combine compatibility mismatch in {name}\n\
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

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn path_string(path: PathBuf) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn cli_arity_and_operation_diagnostics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let a = write_file(cwd, "a", b"a\n");
    let b = write_file(cwd, "b", b"b\n");

    let cases: &[(&str, Vec<&str>)] = &[
        ("no args", vec![]),
        ("one arg", vec![&a]),
        ("two args", vec![&a, "and"]),
        ("four unrelated args", vec![&a, "and", &b, "extra"]),
        ("unknown operation", vec![&a, "bogus", &b]),
        ("unknown operation lowercases", vec![&a, "BOGUS", &b]),
        (
            "unknown operation does not open files",
            vec!["missing1", "bogus", "missing2"],
        ),
        (
            "trailing underscore plus extra arg",
            vec![&a, "and", &b, "_", "extra"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn valid_operations_and_case_insensitivity_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let a = write_file(cwd, "a", b"b\na\na\nc\n");
    let b = write_file(cwd, "b", b"a\nd\na\n");

    let cases: &[(&str, Vec<&str>)] = &[
        ("and", vec![&a, "and", &b]),
        ("not", vec![&a, "not", &b]),
        ("or", vec![&a, "or", &b]),
        ("xor", vec![&a, "xor", &b]),
        ("AND", vec![&a, "AND", &b]),
        ("Not", vec![&a, "Not", &b]),
        ("XoR", vec![&a, "XoR", &b]),
        ("optional trailing underscore", vec![&a, "and", &b, "_"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn underscore_invocation_sugar_matches() {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let a = write_file(cwd, "a", b"a\nb\n");
    let b = write_file(cwd, "b", b"b\nc\n");
    let oracle_link = cwd.join("oracle_");
    let ours_link = cwd.join("ours_");
    symlink(ORACLE, &oracle_link).unwrap();
    symlink(OURS, &ours_link).unwrap();

    let args = [&a.as_str(), "and", &b.as_str(), "_"];
    let oracle = run_combine(oracle_link.to_str().unwrap(), &args, b"", cwd);
    let ours = run_combine(ours_link.to_str().unwrap(), &args, b"", cwd);
    assert_same("underscore_invocation_sugar_matches", &oracle, &ours);
}

#[test]
fn stdin_input_sources_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let file = write_file(cwd, "file", b"a\nb\nb\nd\n");
    let stdin = b"b\nc\nb\n";

    let cases: &[(&str, Vec<&str>)] = &[
        ("dash as file1 and", vec!["-", "and", &file]),
        ("dash as file1 not", vec!["-", "not", &file]),
        ("dash as file1 or", vec!["-", "or", &file]),
        ("dash as file1 xor", vec!["-", "xor", &file]),
        ("dash as file2 and", vec![&file, "and", "-"]),
        ("dash as file2 not", vec![&file, "not", "-"]),
        ("dash as file2 or", vec![&file, "or", "-"]),
        ("dash as file2 xor", vec![&file, "xor", "-"]),
        ("both dash and", vec!["-", "and", "-"]),
        ("both dash not", vec!["-", "not", "-"]),
        ("both dash or", vec!["-", "or", "-"]),
        ("both dash xor", vec!["-", "xor", "-"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, stdin, cwd);
    }
}

#[test]
fn missing_and_unreadable_inputs_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let good = write_file(cwd, "good", b"good\n");
    let missing1 = path_string(cwd.join("missing1"));
    let missing2 = path_string(cwd.join("missing2"));
    let unreadable = cwd.join("unreadable");
    std::fs::write(&unreadable, b"secret\n").unwrap();
    let mut perms = std::fs::metadata(&unreadable).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&unreadable, perms).unwrap();
    let unreadable = path_string(unreadable);

    let cases: &[(&str, Vec<&str>)] = &[
        ("missing file1", vec![&missing1, "or", &good]),
        ("missing file2", vec![&good, "or", &missing2]),
        (
            "missing both and reports file2 first",
            vec![&missing1, "and", &missing2],
        ),
        (
            "missing both not reports file2 first",
            vec![&missing1, "not", &missing2],
        ),
        (
            "missing both or reports file1 first",
            vec![&missing1, "or", &missing2],
        ),
        (
            "missing both xor reports file2 first",
            vec![&missing1, "xor", &missing2],
        ),
        ("unreadable file1", vec![&unreadable, "or", &good]),
        ("unreadable file2", vec![&good, "and", &unreadable]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn empty_files_in_each_position_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let empty = write_file(cwd, "empty", b"");
    let nonempty = write_file(cwd, "nonempty", b"a\nb\n");

    let cases: &[(&str, Vec<&str>)] = &[
        ("empty file1 and", vec![&empty, "and", &nonempty]),
        ("empty file2 and", vec![&nonempty, "and", &empty]),
        ("empty file1 not", vec![&empty, "not", &nonempty]),
        ("empty file2 not", vec![&nonempty, "not", &empty]),
        ("empty file1 or", vec![&empty, "or", &nonempty]),
        ("empty file2 or", vec![&nonempty, "or", &empty]),
        ("empty file1 xor", vec![&empty, "xor", &nonempty]),
        ("empty file2 xor", vec![&nonempty, "xor", &empty]),
        ("both empty xor", vec![&empty, "xor", &empty]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn line_semantics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let long_line = vec![b'x'; 100_000];
    let mut file1 = Vec::new();
    file1.extend_from_slice(b"no-newline-at-eof");
    let file1_no_newline = write_file(cwd, "file1-no-newline", &file1);
    let file2_no_newline = write_file(cwd, "file2-no-newline", b"no-newline-at-eof");

    let weird1 = write_file(
        cwd,
        "weird1",
        b"\n   \n\t\t\n leading\ntrailing \n a b \ncarriage\r\n",
    );
    let weird2 = write_file(cwd, "weird2", b"\n\t\t\ntrailing \ncarriage\r\nabsent\n");

    let long1 = write_file(cwd, "long1", &[long_line.as_slice(), b"\nshort\n"].concat());
    let long2 = write_file(cwd, "long2", &[long_line.as_slice(), b"\nother\n"].concat());

    let binary1 = write_file(cwd, "binary1", b"nul\0line\ninvalid\xff\nshared\xfe\n");
    let binary2 = write_file(cwd, "binary2", b"shared\xfe\ninvalid\xff\nother\0line\n");

    let cases: &[(&str, Vec<&str>)] = &[
        (
            "no trailing newline",
            vec![&file1_no_newline, "and", &file2_no_newline],
        ),
        (
            "empty whitespace tab carriage lines and",
            vec![&weird1, "and", &weird2],
        ),
        (
            "empty whitespace tab carriage lines not",
            vec![&weird1, "not", &weird2],
        ),
        ("very long line", vec![&long1, "xor", &long2]),
        ("binary bytes and", vec![&binary1, "and", &binary2]),
        ("binary bytes xor", vec![&binary1, "xor", &binary2]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn operation_specific_duplicate_semantics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let f1 = write_file(cwd, "f1", b"a\na\nb\nc\nc\nd\n");
    let f2 = write_file(cwd, "f2", b"a\na\nc\ne\ne\n");

    let cases: &[(&str, Vec<&str>)] = &[
        ("and preserves file1 duplicates only", vec![&f1, "and", &f2]),
        (
            "not preserves absent file1 duplicates",
            vec![&f1, "not", &f2],
        ),
        ("or concatenates all duplicates", vec![&f1, "or", &f2]),
        (
            "xor preserves file2 duplicates unique to file2",
            vec![&f1, "xor", &f2],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn non_commutative_ordering_matches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let a = write_file(cwd, "a", b"1\n2\n2\n3\n4\n");
    let b = write_file(cwd, "b", b"4\n3\n3\n2\n5\n5\n");

    let cases: &[(&str, Vec<&str>)] = &[
        ("a and b", vec![&a, "and", &b]),
        ("b and a", vec![&b, "and", &a]),
        ("a not b", vec![&a, "not", &b]),
        ("b not a", vec![&b, "not", &a]),
        ("a xor b duplicate layout", vec![&a, "xor", &b]),
        ("b xor a duplicate layout", vec![&b, "xor", &a]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}
