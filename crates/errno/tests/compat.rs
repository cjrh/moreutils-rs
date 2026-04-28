// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/errno";
const OURS: &str = env!("CARGO_BIN_EXE_errno");

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

fn base_command<S: AsRef<OsStr>>(program: S, lc_all: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", lc_all)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn errno");
    if let Some(mut child_stdin) = child.stdin.take() {
        match child_stdin.write_all(stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to errno: {err}"),
        }
    }
    let output = child.wait_with_output().expect("wait for errno");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_errno(program: &str, args: &[&str], lc_all: &str) -> RunOutput {
    let mut command = base_command(program, lc_all);
    command.args(args);
    finish_command(command, b"")
}

fn assert_compat(name: &str, args: &[&str]) {
    assert_compat_locale(name, args, "C");
}

fn assert_compat_locale(name: &str, args: &[&str], lc_all: &str) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_errno(ORACLE, args, lc_all);
    let ours = run_errno(OURS, args, lc_all);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "errno compatibility mismatch in {name}\n\
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

fn locale_available(locale: &str) -> bool {
    let output = Command::new("locale")
        .arg("-a")
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .output()
        .expect("locale -a");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line == locale)
}

#[test]
fn cli_parsing_and_options_match() {
    let cases: &[(&str, &[&str])] = &[
        ("no args", &[]),
        ("help short", &["-h"]),
        ("help long", &["--help"]),
        ("unknown short option", &["-x"]),
        ("unknown long option", &["--bad"]),
        ("unknown short plus lookup", &["-x", "ENOENT"]),
        ("unknown long plus lookup", &["--bad", "ENOENT"]),
        ("negative number parsed as option", &["-2"]),
        ("dashdash stops options", &["--", "-2"]),
        ("option after operand is permuted", &["ENOENT", "-l"]),
        ("list before operand", &["-l", "ENOENT"]),
        ("cluster list then search", &["-ls", "no"]),
        ("cluster search then list", &["-sl", "no"]),
        ("cluster search with invalid trailing chars", &["-sno"]),
        ("search then later list", &["-s", "no", "-l"]),
        ("list then later search", &["-l", "-s", "no"]),
        ("long option disallows argument", &["--search=no"]),
        ("long list disallows argument", &["--list=foo"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }
}

#[test]
fn lookup_by_name_matches() {
    let cases: &[(&str, &[&str])] = &[
        (
            "common names",
            &["ENOENT", "EACCES", "EPERM", "EINVAL", "EPIPE"],
        ),
        (
            "linux-specific names",
            &["EHWPOISON", "ERFKILL", "ENOKEY", "EOWNERDEAD"],
        ),
        ("aliases by name", &["EWOULDBLOCK", "EDEADLOCK", "ENOTSUP"]),
        ("lowercase name", &["enoent"]),
        ("mixed case name", &["EnOeNt"]),
        ("unknown errno-style name", &["EFOO"]),
        ("unknown lowercase errno-style name", &["efoo"]),
        ("unknown non-errno word", &["foo"]),
        (
            "literal unusual names",
            &["EINVALx", "noent", "+ENOENT", ""],
        ),
        (
            "mixed successes and failures",
            &["ENOENT", "foo", "EACCES", "EFOO"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }
}

#[test]
fn lookup_by_number_matches() {
    let cases: &[(&str, &[&str])] = &[
        ("common numbers", &["1", "2", "13", "22", "32"]),
        (
            "alias numbers choose first table entry",
            &["11", "35", "95"],
        ),
        ("linux-specific numbers", &["132", "133"]),
        ("zero", &["0"]),
        ("negative after dashdash", &["--", "-1"]),
        ("large number", &["999999"]),
        ("very large number", &["999999999999999999999999"]),
        ("i32 boundary numbers", &["2147483647", "2147483648"]),
        ("leading zero decimal", &["02"]),
        ("hex-like parses decimal prefix zero", &["0x2"]),
        ("plus-prefixed is not numeric", &["+2"]),
        ("numeric prefix ignores suffix", &["2x", "02x"]),
        ("multiple numeric lookups", &["2", "3"]),
        (
            "mixed numeric successes and failures",
            &["0", "ENOENT", "999999", "13"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }
}

#[test]
fn listing_matches_exactly() {
    assert_compat("short list", &["-l"]);
    assert_compat("long list", &["--list"]);
}

#[test]
fn search_matches() {
    let cases: &[(&str, &[&str])] = &[
        ("short search no words", &["-s"]),
        ("long search no words", &["--search"]),
        ("single word case insensitive lower", &["-s", "file"]),
        ("single word case insensitive upper", &["-s", "FILE"]),
        ("multiple words all must match", &["-s", "not", "permitted"]),
        ("multiple words no match", &["-s", "not", "xyz"]),
        ("search does not include errno names", &["-s", "ENOENT"]),
        ("regex metacharacter is literal dot", &["-s", "."]),
        ("regex metacharacter is literal bracket", &["-s", "["]),
        ("alias descriptions both match", &["-s", "deadlock"]),
        ("resource search", &["-s", "Resource"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }
}

#[test]
fn search_all_locales_matches() {
    assert_compat("short search all locales", &["-S", "no", "such"]);
    assert_compat(
        "long search all locales",
        &["--search-all-locales", "deadlock"],
    );
}

#[test]
fn search_all_locales_without_words_matches() {
    assert_compat("search all locales no words", &["-S"]);
}

#[test]
fn locale_specific_output_matches_when_locale_exists() {
    assert_compat_locale("C locale baseline", &["ENOENT", "-s", "file"], "C");
    if locale_available("en_US.utf8") {
        assert_compat_locale("en_US.utf8 locale", &["ENOENT", "-s", "file"], "en_US.utf8");
    }
    if locale_available("fr_FR.utf8") {
        assert_compat_locale(
            "fr_FR.utf8 locale",
            &["ENOENT", "-s", "fichier"],
            "fr_FR.utf8",
        );
    }
}
