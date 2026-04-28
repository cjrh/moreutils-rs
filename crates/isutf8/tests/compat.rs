// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/isutf8";
const OURS: &str = env!("CARGO_BIN_EXE_isutf8");

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
    let mut child = command.spawn().expect("spawn isutf8");
    if let Some(mut child_stdin) = child.stdin.take() {
        match child_stdin.write_all(stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to isutf8: {err}"),
        }
    }
    let output = child.wait_with_output().expect("wait for isutf8");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_isutf8(program: &str, args: &[&str], stdin: &[u8], cwd: &Path) -> RunOutput {
    let mut command = base_command(program, cwd);
    command.args(args);
    finish_command(command, stdin)
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_isutf8(ORACLE, args, stdin, cwd);
    let ours = run_isutf8(OURS, args, stdin, cwd);
    assert_same(name, &oracle, &ours);
}

#[cfg(unix)]
fn assert_compat_same_argv(name: &str, args: &[&str], stdin: &[u8]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    symlink(ORACLE, oracle_dir.join("isutf8")).unwrap();
    symlink(OURS, ours_dir.join("isutf8")).unwrap();

    let oracle = run_isutf8("./isutf8", args, stdin, &oracle_dir);
    let ours = run_isutf8("./isutf8", args, stdin, &ours_dir);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "isutf8 compatibility mismatch in {name}\n\
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

fn write_file(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(dir.join(name), bytes).unwrap();
}

#[test]
fn cli_parsing_help_and_option_errors_match() {
    let cases: &[(&str, &[&str])] = &[
        ("short help", &["-h"]),
        ("long help", &["--help"]),
        ("unknown short option", &["-x"]),
        ("unknown in short option cluster", &["-qx"]),
        ("unknown long option", &["--bad"]),
        ("unknown long option with argument", &["--bad=arg"]),
        ("long option disallows argument", &["--quiet=arg"]),
        ("abbreviated long option disallows argument", &["--l=arg"]),
        ("help in short option cluster", &["-qh"]),
    ];

    for (name, args) in cases {
        assert_compat_same_argv(name, args, b"");
    }
}

#[test]
fn stdin_valid_and_invalid_inputs_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("empty stdin", &[], b""),
        ("ascii stdin", &[], b"ascii\n"),
        ("multibyte stdin", &[], "café 👩\u{200d}💻\n".as_bytes()),
        ("invalid stdin default", &[], b"abc\xff"),
        ("invalid stdin quiet", &["-q"], b"abc\xff"),
        ("invalid stdin list", &["-l"], b"abc\xff"),
        ("valid stdin invert", &["-i"], b"valid"),
        ("invalid stdin invert", &["-i"], b"abc\xff"),
        ("invalid stdin verbose", &["-v"], b"abc\xffdef"),
        ("invalid stdin char zero", &[], b"\xff"),
        ("invalid stdin after newline", &[], b"a\nb\xff"),
    ];

    for (name, args, stdin) in cases {
        assert_compat(name, args, stdin, cwd);
    }
}

#[test]
fn valid_utf8_files_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    write_file(cwd, "empty", b"");
    write_file(cwd, "ascii", b"hello\nworld\n");
    write_file(cwd, "multibyte", "café\ncombining: e\u{301}\n".as_bytes());
    write_file(cwd, "emoji", "emoji: 😀 🦀 👩\u{200d}💻\n".as_bytes());
    write_file(cwd, "max4", "\u{10ffff}\n".as_bytes());

    let cases: &[(&str, &[&str])] = &[
        ("empty file", &["empty"]),
        ("ascii file", &["ascii"]),
        ("multibyte file", &["multibyte"]),
        ("emoji file", &["emoji"]),
        ("valid four-byte max", &["max4"]),
        (
            "multiple valid files",
            &["empty", "ascii", "multibyte", "emoji", "max4"],
        ),
        (
            "invert lists valid files",
            &["-i", "empty", "ascii", "multibyte"],
        ),
        (
            "invert list cluster lists valid files",
            &["-il", "empty", "ascii", "multibyte"],
        ),
        (
            "quiet invert suppresses valid list",
            &["-iq", "empty", "ascii"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn invalid_utf8_patterns_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let patterns: &[(&str, &[u8])] = &[
        ("lone_continuation", b"\x80"),
        ("invalid_ff", b"\xff"),
        ("truncated_two_byte", b"\xc2"),
        ("bad_second_two_byte", b"\xc2A"),
        ("truncated_e0_one", b"\xe0"),
        ("truncated_e0_two", b"\xe0\xa0"),
        ("bad_e0_second_overlong", b"\xe0\x80\x80"),
        ("bad_e0_third", b"\xe0\xa0A"),
        ("bad_e1_second", b"\xe1A\x80"),
        ("bad_e1_third", b"\xe1\x80A"),
        ("surrogate", b"\xed\xa0\x80"),
        ("bad_ed_third", b"\xed\x80A"),
        ("truncated_ef", b"\xef\x80"),
        ("bad_ef_second", b"\xefA\x80"),
        ("truncated_f0", b"\xf0\x90\x80"),
        ("overlong_four_byte", b"\xf0\x80\x80\x80"),
        ("bad_f0_third", b"\xf0\x90A\x80"),
        ("bad_f0_fourth", b"\xf0\x90\x80A"),
        ("truncated_f1", b"\xf1\x80"),
        ("bad_f1_second", b"\xf1A\x80\x80"),
        ("bad_f1_third", b"\xf1\x80A\x80"),
        ("bad_f1_fourth", b"\xf1\x80\x80A"),
        ("above_unicode_range", b"\xf4\x90\x80\x80"),
        ("bad_f4_third", b"\xf4\x80A\x80"),
        ("bad_f4_fourth", b"\xf4\x80\x80A"),
        ("invalid_leading_f5", b"\xf5\x80\x80\x80"),
        ("late_invalid", b"valid line\nnext line\xff"),
        ("invalid_without_final_newline", b"ok\nlast\xff"),
        ("crlf_before_invalid", b"first\r\nsecond\xff"),
        ("multibyte_before_invalid", "éé".as_bytes()),
    ];

    for (name, bytes) in patterns {
        let data = if *name == "multibyte_before_invalid" {
            let mut data = bytes.to_vec();
            data.push(0xff);
            data
        } else {
            bytes.to_vec()
        };
        write_file(cwd, name, &data);
        assert_compat(name, &[*name], b"", cwd);
    }
}

#[test]
fn output_modes_multiple_files_and_verbose_format_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    write_file(cwd, "valid", b"valid\n");
    write_file(cwd, "invalid", b"abc\xffdef");
    write_file(cwd, "long", b"0123456789abcdefghijklmnop\xffqrstuvwxyz");
    write_file(cwd, "other_invalid", b"\xe0\x80\x80");

    let cases: &[(&str, &[&str])] = &[
        ("mixed valid then invalid", &["valid", "invalid"]),
        ("mixed invalid then valid", &["invalid", "valid"]),
        (
            "multiple invalid files",
            &["invalid", "other_invalid", "valid"],
        ),
        ("quiet", &["-q", "valid", "invalid"]),
        ("list", &["-l", "valid", "invalid", "other_invalid"]),
        ("long quiet", &["--quiet", "valid", "invalid"]),
        ("long list", &["--list", "valid", "invalid"]),
        ("long list-only", &["--list-only", "valid", "invalid"]),
        ("abbreviated long list", &["--l", "valid", "invalid"]),
        ("invert", &["-i", "valid", "invalid"]),
        ("invert list", &["-il", "valid", "invalid"]),
        (
            "long invert list",
            &["--invert", "--list", "valid", "invalid"],
        ),
        ("quiet invert list", &["-qil", "valid", "invalid"]),
        ("verbose", &["-v", "invalid"]),
        ("verbose centered context", &["-v", "long"]),
        ("list dominates verbose", &["-lv", "invalid"]),
        ("invert suppresses invalid verbose", &["-iv", "invalid"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn option_permutation_and_dashdash_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    write_file(cwd, "valid", b"valid\n");
    write_file(cwd, "invalid", b"\xff");
    write_file(cwd, "-q", b"\xff");

    let cases: &[(&str, &[&str])] = &[
        ("option after invalid filename", &["invalid", "-q"]),
        ("list after invalid filename", &["invalid", "-l"]),
        ("invert after valid filename", &["valid", "-i"]),
        ("long option after filename", &["invalid", "--quiet"]),
        ("dashdash stops option parsing", &["--", "-q"]),
        ("quiet before dashdash", &["-q", "--", "invalid"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }
}

#[test]
fn file_errors_and_directories_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    write_file(cwd, "valid", b"valid\n");
    let unreadable = cwd.join("unreadable");
    write_file(cwd, "unreadable", b"valid\n");
    let mut perms = std::fs::metadata(&unreadable).unwrap().permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&unreadable, perms).unwrap();

    let cases: &[(&str, &[&str])] = &[
        ("missing file", &["missing"]),
        ("quiet does not suppress open error", &["-q", "missing"]),
        ("missing among valid files", &["valid", "missing"]),
        ("directory argument", &["."]),
        ("directory among files", &["valid", "."]),
        ("unreadable file", &["unreadable"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd);
    }

    let mut perms = std::fs::metadata(&unreadable).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&unreadable, perms).unwrap();
}
