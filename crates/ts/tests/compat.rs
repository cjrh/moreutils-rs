// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::OsStr;
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

const ORACLE: &str = "/bin/ts";
const OURS: &str = env!("CARGO_BIN_EXE_ts");
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

fn base_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut command = Command::new(program);
    #[cfg(unix)]
    command.arg0("ts").process_group(0);
    command
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_ts(program: &str, args: &[&str], stdin: &[u8], envs: &[(&str, &str)]) -> RunOutput {
    let mut command = base_command(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    finish_command(command, stdin)
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn ts");
    let pid = child.id();
    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to ts: {err}"),
        })
    });
    let stdout = child.stdout.take().expect("piped stdout");
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));

    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll ts") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait for timed-out ts");
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
        stdout: stdout_reader.join().expect("stdout reader thread"),
        stderr: stderr_reader.join().expect("stderr reader thread"),
    }
}

fn read_all<R: Read>(mut reader: R, name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .unwrap_or_else(|err| panic!("read ts {name}: {err}"));
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

fn assert_oracle_available() {
    assert!(Path::new(ORACLE).exists(), "{ORACLE} is required");
}

fn assert_exact(name: &str, args: &[&str], stdin: &[u8]) {
    assert_exact_env(name, args, stdin, &[]);
}

fn assert_exact_env(name: &str, args: &[&str], stdin: &[u8], envs: &[(&str, &str)]) {
    assert_oracle_available();
    let oracle = run_ts(ORACLE, args, stdin, envs);
    let ours = run_ts(OURS, args, stdin, envs);
    if oracle != ours {
        panic!(
            "ts mismatch in {name}\nstatus oracle={:?} ours={:?}\ntimeout oracle={} ours={}\nstdout oracle={} ours={}\nstderr oracle={} ours={}",
            oracle.status,
            ours.status,
            oracle.timed_out,
            ours.timed_out,
            render_bytes(&oracle.stdout),
            render_bytes(&ours.stdout),
            render_bytes(&oracle.stderr),
            render_bytes(&ours.stderr)
        );
    }
}

fn assert_status_stderr(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    assert!(!oracle.timed_out && !ours.timed_out, "{name} timed out");
    assert_eq!(oracle.status, ours.status, "{name} status");
    assert_eq!(oracle.stderr, ours.stderr, "{name} stderr");
}

fn render_bytes(bytes: &[u8]) -> String {
    const LIMIT: usize = 512;
    let mut s = String::new();
    for &byte in bytes.iter().take(LIMIT) {
        match byte {
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(byte as char),
            _ => s.push_str(&format!("\\x{byte:02x}")),
        }
    }
    if bytes.len() > LIMIT {
        s.push_str(&format!("... (+{} bytes)", bytes.len() - LIMIT));
    }
    format!("len={} b\"{}\"", bytes.len(), s)
}

fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&b| b == b'\n').collect()
}

fn strip_suffix<'a>(line: &'a [u8], suffix: &[u8], name: &str) -> &'a [u8] {
    assert!(
        line.ends_with(suffix),
        "{name}: {} does not end with {}",
        render_bytes(line),
        render_bytes(suffix)
    );
    &line[..line.len() - suffix.len()]
}

fn assert_hms(bytes: &[u8], name: &str) {
    assert_eq!(bytes.len(), 8, "{name}");
    assert_eq!(bytes[2], b':', "{name}");
    assert_eq!(bytes[5], b':', "{name}");
    for &idx in &[0, 1, 3, 4, 6, 7] {
        assert!(
            bytes[idx].is_ascii_digit(),
            "{name}: {}",
            render_bytes(bytes)
        );
    }
}

fn assert_default_stamp(bytes: &[u8], name: &str) {
    assert_eq!(bytes.len(), 15, "{name}: {}", render_bytes(bytes));
    assert!(
        bytes[0..3].iter().all(|b| b.is_ascii_alphabetic()),
        "{name}"
    );
    assert_eq!(bytes[3], b' ', "{name}");
    assert!(bytes[4].is_ascii_digit() || bytes[4] == b' ', "{name}");
    assert!(bytes[5].is_ascii_digit(), "{name}");
    assert_eq!(bytes[6], b' ', "{name}");
    assert_hms(&bytes[7..], name);
}

fn assert_seconds_micro(bytes: &[u8], name: &str) {
    assert_eq!(bytes.len(), 9, "{name}: {}", render_bytes(bytes));
    assert!(
        bytes[0].is_ascii_digit() && bytes[1].is_ascii_digit(),
        "{name}"
    );
    assert_eq!(bytes[2], b'.', "{name}");
    assert!(bytes[3..].iter().all(|b| b.is_ascii_digit()), "{name}");
}

fn assert_time_micro(bytes: &[u8], name: &str) {
    assert_eq!(bytes.len(), 15, "{name}: {}", render_bytes(bytes));
    assert_hms(&bytes[..8], name);
    assert_eq!(bytes[8], b'.', "{name}");
    assert!(bytes[9..].iter().all(|b| b.is_ascii_digit()), "{name}");
}

fn parse_f64_stamp(line: &[u8], suffix: &[u8], name: &str) -> f64 {
    let stamp = strip_suffix(line, suffix, name);
    std::str::from_utf8(stamp).unwrap().parse::<f64>().unwrap()
}

#[test]
fn cli_parsing_matches_getopt_long() {
    let cases: &[(&str, &[&str])] = &[
        ("unknown short", &["-x"]),
        ("unknown long", &["--bad"]),
        ("no short clustering", &["-is"]),
        ("no combined options", &["-ri"]),
        ("option value rejected", &["-r=1"]),
        ("long single-letter value rejected", &["--i=1"]),
        ("dash format needs dashdash", &["-%Y"]),
        ("options are permuted", &["literal", "-x"]),
        ("long aliases are not accepted", &["--relative"]),
        ("unknown is lowercased", &["-X"]),
    ];
    for (name, args) in cases {
        assert_exact(name, args, b"input\n");
    }
    assert_exact("empty stdin", &[], b"");
    assert_exact("literal dash format", &["-"], b"input\n");
    assert_exact(
        "format begins dash after dashdash",
        &["--", "-foo"],
        b"input\n",
    );
    assert_exact(
        "uppercase option accepted",
        &["-R", "literal"],
        b"not a timestamp\n",
    );
    assert_exact("long single letter accepted", &["--i"], b"x\n");
}

#[test]
fn stdin_bytes_and_line_endings_match() {
    let mut long = vec![b'a'; 150_000];
    long.push(b'\n');
    let cases: &[(&str, &[u8])] = &[
        ("empty", b""),
        ("with newline", b"one\n"),
        ("without newline", b"one"),
        ("multiple", b"one\ntwo\nthree"),
        ("invalid utf8", b"\xff\x80ok\n"),
        ("nul byte", b"a\0b\n"),
        ("carriage returns", b"a\r\nb\r"),
        ("empty lines", b"\n\n"),
    ];
    for (name, stdin) in cases {
        assert_exact(name, &["prefix"], stdin);
    }
    assert_exact("long line", &["prefix"], &long);
}

#[test]
fn absolute_dynamic_formats_have_compatible_shape() {
    assert_oracle_available();
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("default", &[], b"alpha\nbeta\n"),
        ("date", &["%Y-%m-%d"], b"alpha\n"),
        ("epoch", &["%s"], b"alpha\n"),
        ("timezone", &["%z"], b"alpha\n"),
        ("literal percent", &["%%"], b"alpha\n"),
    ];
    for (name, args, stdin) in cases {
        let oracle = run_ts(ORACLE, args, stdin, &[]);
        let ours = run_ts(OURS, args, stdin, &[]);
        assert_status_stderr(name, &oracle, &ours);
        let oracle_lines = lines(&oracle.stdout);
        let ours_lines = lines(&ours.stdout);
        assert_eq!(oracle_lines.len(), ours_lines.len(), "{name}");
        for (idx, (oracle_line, ours_line)) in
            oracle_lines.iter().zip(ours_lines.iter()).enumerate()
        {
            let suffix = if idx == 0 {
                b" alpha\n" as &[u8]
            } else {
                b" beta\n" as &[u8]
            };
            let oracle_stamp = strip_suffix(oracle_line, suffix, name);
            let ours_stamp = strip_suffix(ours_line, suffix, name);
            match *name {
                "default" => {
                    assert_default_stamp(oracle_stamp, "oracle default");
                    assert_default_stamp(ours_stamp, "ours default");
                }
                "date" => {
                    assert_eq!(oracle_stamp.len(), 10);
                    assert_eq!(ours_stamp.len(), 10);
                    assert_eq!(oracle_stamp[4], b'-');
                    assert_eq!(ours_stamp[4], b'-');
                }
                "epoch" => {
                    let o: i64 = std::str::from_utf8(oracle_stamp).unwrap().parse().unwrap();
                    let u: i64 = std::str::from_utf8(ours_stamp).unwrap().parse().unwrap();
                    assert!((o - u).abs() <= 2, "epoch mismatch {o} {u}");
                }
                _ => assert_eq!(oracle_stamp, ours_stamp, "{name}"),
            }
        }
    }
}

#[test]
fn timezone_environment_matches() {
    assert_exact_env("UTC offset", &["%z"], b"x\n", &[("TZ", "UTC")]);
    assert_exact_env("POSIX offset", &["%z"], b"x\n", &[("TZ", "GMT+5")]);
}

#[test]
fn subsecond_extensions_match_shape() {
    assert_oracle_available();
    let args = &["%.S %.s %.T %% %.S"];
    let oracle = run_ts(ORACLE, args, b"payload\n", &[]);
    let ours = run_ts(OURS, args, b"payload\n", &[]);
    assert_status_stderr("subseconds", &oracle, &ours);
    for (label, output) in [("oracle", &oracle.stdout), ("ours", &ours.stdout)] {
        let stamp = strip_suffix(&lines(output)[0], b" payload\n", label);
        let fields: Vec<&[u8]> = stamp.split(|&b| b == b' ').collect();
        assert_eq!(fields.len(), 5, "{label}: {}", render_bytes(stamp));
        assert_seconds_micro(fields[0], label);
        assert!(fields[1].contains(&b'.'), "{label}: epoch.micro");
        assert_time_micro(fields[2], label);
        assert_eq!(fields[3], b"%");
        assert_seconds_micro(fields[4], label);
    }
}

#[test]
fn incremental_since_start_and_monotonic_ranges() {
    assert_timed_mode("incremental", &["-i", "%.s"]);
    assert_timed_mode("since start", &["-s", "%.s"]);
    assert_timed_mode("monotonic incremental", &["-m", "-i", "%.s"]);
    assert_timed_mode("monotonic since", &["-m", "-s", "%.s"]);
    assert_timed_mode("both flags means incremental", &["-i", "-s", "%.s"]);
    assert_timed_mode("reverse both flags means incremental", &["-s", "-i", "%.s"]);
    assert_exact("incremental default", &["-i"], b"x\n");
    assert_exact("since-start default", &["-s"], b"x\n");
    assert_exact_env(
        "incremental ignores TZ",
        &["-i"],
        b"x\n",
        &[("TZ", "GMT+12")],
    );
    assert_exact_env(
        "since-start ignores TZ",
        &["-s"],
        b"x\n",
        &[("TZ", "GMT+12")],
    );
}

fn assert_timed_mode(name: &str, args: &[&str]) {
    assert_oracle_available();
    for (label, output) in [
        ("oracle", run_scripted(ORACLE, args)),
        ("ours", run_scripted(OURS, args)),
    ] {
        assert!(!output.timed_out, "{name} {label} timeout");
        assert_eq!(output.status.code, Some(0), "{name} {label} status");
        assert_eq!(output.stderr, b"", "{name} {label} stderr");
        let out_lines = lines(&output.stdout);
        assert_eq!(
            out_lines.len(),
            3,
            "{name} {label}: {}",
            render_bytes(&output.stdout)
        );
        let first = parse_f64_stamp(out_lines[0], b" a\n", name);
        let second = parse_f64_stamp(out_lines[1], b" b\n", name);
        let third = parse_f64_stamp(out_lines[2], b" c\n", name);
        assert!((0.0..0.2).contains(&first), "{name} {label} first {first}");
        assert!(
            (0.04..0.8).contains(&second),
            "{name} {label} second {second}"
        );
        assert!((0.04..0.9).contains(&third), "{name} {label} third {third}");
    }
}

fn run_scripted(program: &str, args: &[&str]) -> RunOutput {
    let mut command = base_command(program);
    command.args(args);
    let mut child = command.spawn().expect("spawn scripted ts");
    let pid = child.id();
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        stdin.write_all(b"a\n").unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(90));
        stdin.write_all(b"b\n").unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(110));
        stdin.write_all(b"c\n").unwrap();
    });
    let stdout = child.stdout.take().unwrap();
    let stdout_reader = thread::spawn(move || read_all(stdout, "stdout"));
    let stderr = child.stderr.take().unwrap();
    let stderr_reader = thread::spawn(move || read_all(stderr, "stderr"));
    let deadline = Instant::now() + RUN_TIMEOUT;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll scripted ts") {
            break (status, false);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(pid);
            let status = child.wait().expect("wait scripted ts");
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    writer.join().expect("scripted writer");
    RunOutput {
        timed_out,
        status: status.into(),
        stdout: stdout_reader.join().unwrap(),
        stderr: stderr_reader.join().unwrap(),
    }
}

#[test]
fn output_is_line_buffered() {
    assert_line_buffered(ORACLE);
    assert_line_buffered(OURS);
}

fn assert_line_buffered(program: &str) {
    assert_oracle_available();
    let mut command = base_command(program);
    command.arg("prefix");
    let mut child = command.spawn().expect("spawn line-buffer ts");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reader = io::BufReader::new(stdout);
        let mut line = Vec::new();
        let result = reader.read_until(b'\n', &mut line).map(|_| line);
        let _ = tx.send(result);
    });
    stdin.write_all(b"first\n").unwrap();
    stdin.flush().unwrap();
    let line = rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| panic!("{program} did not flush before EOF"))
        .expect("read first output line");
    assert_eq!(line, b"prefix first\n");
    drop(stdin);
    let _ = child.wait();
    reader.join().unwrap();
}

#[test]
fn relative_conversion_with_format_matches() {
    let input = b"Jan  2 03:04:05 syslog\n\
2020-01-02T03:04:05.123Z iso\n\
2020:01:02T03:04:05.123Z iso-colon\n\
Thu, 16 Jun 1994 07:29:35 GMT rfc\n\
Thu 16 Jun 1994 07:29:35 GMT rfc-no-comma\n\
16 Jun 94 07:29:35 +0200 offset\n\
21 Dec 17:05 daymonth\n\
21 Dec 17:05 +0200 daymonth-tz\n\
Mon Jan 02 03:04 lastlog\n\
no timestamp here\n\
2020-01-02T03:04:05.123Z first and 2021-02-03T04:05:06.789Z second\n";
    assert_exact("relative formatted", &["-r", "%Y-%m-%dT%H:%M:%S%z"], input);
    assert_exact("relative epoch", &["-r", "%s"], input);
    assert_exact(
        "relative subsecond tokens are literal",
        &["-r", "%.S %.s %.T"],
        b"2020-01-02T03:04:05.123Z x\n",
    );
    assert_exact_env(
        "relative TZ env",
        &["-r", "%Y-%m-%dT%H:%M:%S%z"],
        b"2020-01-02T03:04:05.123Z zulu\n21 Dec 17:05 local\n",
        &[("TZ", "America/New_York")],
    );
}

#[test]
fn relative_conversion_without_format_has_compatible_shape() {
    assert_oracle_available();
    let input = b"1970-01-01T00:00:00.000Z old\n2999-01-01T00:00:00.000Z future\nno timestamp\n";
    let oracle = run_ts(ORACLE, &["-r"], input, &[]);
    let ours = run_ts(OURS, &["-r"], input, &[]);
    assert_status_stderr("relative no format", &oracle, &ours);
    for (label, output) in [("oracle", &oracle.stdout), ("ours", &ours.stdout)] {
        let text = String::from_utf8_lossy(output);
        let mut it = text.lines();
        let old = it.next().unwrap();
        let future = it.next().unwrap();
        let unchanged = it.next().unwrap();
        assert!(old.ends_with(" ago old"), "{label}: {old}");
        assert!(future.ends_with(" from now future"), "{label}: {future}");
        assert_eq!(unchanged, "no timestamp");
    }
}

#[test]
fn relative_invalid_and_no_match_inputs() {
    assert_exact("relative no matches", &["-r"], b"plain\n\xff\x80\nplain2");
    assert_exact(
        "relative invalid non-matches",
        &["-r", "%Y"],
        b"not a timestamp\n2020-99-99 99:99:99 bad\n99 Jax nope bad\n",
    );
    assert_exact(
        "relative lowercase z not matched",
        &["-r", "%Y"],
        b"2020-01-02T03:04:05.123z x\n",
    );
}

#[test]
fn extra_operands_are_input_files() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a.txt");
    let b = temp.path().join("b.txt");
    std::fs::write(&a, b"a\n").unwrap();
    std::fs::write(&b, b"2020-01-02T03:04:05.123Z b").unwrap();
    let a = a.to_string_lossy().into_owned();
    let b = b.to_string_lossy().into_owned();
    assert_exact("file operand", &["prefix", &a], b"ignored stdin\n");
    assert_exact("file order", &["prefix", &a, &b], b"ignored stdin\n");
    assert_exact("dash file operand", &["prefix", "-"], b"stdin\n");
    assert_exact(
        "relative file order",
        &["-r", "%Y", &b, "-"],
        b"2021-02-03T04:05:06.789Z stdin\n",
    );
}

#[test]
fn missing_extra_file_reports_error_but_exits_zero() {
    assert_oracle_available();
    let ours = run_ts(
        OURS,
        &["prefix", "/definitely/missing/ts-file"],
        b"ignored\n",
        &[],
    );
    assert_eq!(ours.status.code, Some(0));
    assert_eq!(ours.stdout, b"");
    assert!(
        String::from_utf8_lossy(&ours.stderr).contains("Can't open /definitely/missing/ts-file")
    );
}

#[test]
fn option_after_dashdash_becomes_operand() {
    assert_exact(
        "dashdash gives option-looking format",
        &["--", "-x"],
        b"in\n",
    );
    let ours = run_ts(OURS, &["prefix", "--", "-x"], b"ignored\n", &[]);
    assert_eq!(ours.status.code, Some(0));
    assert!(String::from_utf8_lossy(&ours.stderr).contains("Can't open -x"));
}

#[test]
fn no_newline_and_binary_are_preserved_with_dynamic_stamps() {
    assert_oracle_available();
    for (args, stdin, suffix) in [
        (
            &["%s"][..],
            b"unterminated" as &[u8],
            b" unterminated" as &[u8],
        ),
        (
            &["%s"][..],
            b"\xff\x00binary\n" as &[u8],
            b" \xff\x00binary\n" as &[u8],
        ),
        (&["-i", "%.s"][..], b"x" as &[u8], b" x" as &[u8]),
    ] {
        let oracle = run_ts(ORACLE, args, stdin, &[]);
        let ours = run_ts(OURS, args, stdin, &[]);
        assert_status_stderr("dynamic preserve", &oracle, &ours);
        assert!(oracle.stdout.ends_with(suffix));
        assert!(ours.stdout.ends_with(suffix));
        assert_eq!(oracle.stdout.ends_with(b"\n"), stdin.ends_with(b"\n"));
        assert_eq!(ours.stdout.ends_with(b"\n"), stdin.ends_with(b"\n"));
    }
}

#[test]
fn monotonic_absolute_mode_accepts_formats() {
    assert_oracle_available();
    let oracle = run_ts(ORACLE, &["-m", "%Y-%m-%d %.S"], b"x\n", &[]);
    let ours = run_ts(OURS, &["-m", "%Y-%m-%d %.S"], b"x\n", &[]);
    assert_status_stderr("monotonic absolute", &oracle, &ours);
    for (label, output) in [("oracle", &oracle.stdout), ("ours", &ours.stdout)] {
        let stamp = strip_suffix(&lines(output)[0], b" x\n", label);
        let fields: Vec<&[u8]> = stamp.split(|&b| b == b' ').collect();
        assert_eq!(fields.len(), 2, "{label}");
        assert_eq!(fields[0].len(), 10, "{label}");
        assert_seconds_micro(fields[1], label);
    }
}

#[test]
fn relative_options_do_not_override_relative_mode() {
    assert_exact(
        "relative with -i",
        &["-r", "-i", "%Y"],
        b"2020-01-02T03:04:05.123Z x\n",
    );
    assert_exact(
        "relative with -s",
        &["-r", "-s", "%Y"],
        b"2020-01-02T03:04:05.123Z x\n",
    );
    assert_exact(
        "relative with -m",
        &["-r", "-m", "%Y"],
        b"2020-01-02T03:04:05.123Z x\n",
    );
    assert_exact(
        "all flags relative no match",
        &["-r", "-i", "-s", "-m", "prefix"],
        b"plain\n",
    );
}

#[test]
fn literal_and_empty_formats_match() {
    assert_exact("empty format", &[""], b"x\n\n");
    assert_exact(
        "relative empty format",
        &["-r", ""],
        b"2020-01-02T03:04:05.123Z x\nplain\n",
    );
    assert_exact("tab format", &["A\tB"], b"x\n");
    assert_exact("newline format", &["A\nB"], b"x\n");
    assert_exact("space format", &[" "], b"x\n");
    assert_exact("escaped percent", &["%% %%"], b"x\n");
    assert_exact(
        "relative escaped percent",
        &["-r", "%% %%.S"],
        b"2020-01-02T03:04:05.123Z x\n",
    );
}

#[test]
fn relative_word_boundaries_and_replacement_scope() {
    assert_exact(
        "word boundaries",
        &["-r", "%Y"],
        b"x2020-01-02T03:04:05.123Z not-boundary\n[2020-01-02T03:04:05.123Z] boundary\n",
    );
    assert_exact(
        "replace substring only",
        &["-r", "%Y"],
        b"before 2020-01-02T03:04:05.123Z after\n",
    );
}

#[test]
fn relative_two_digit_years_and_timezones() {
    assert_exact(
        "two digit years",
        &["-r", "%Y"],
        b"16 Jun 69 07:29:35 x\n16 Jun 70 07:29:35 y\n",
    );
    assert_exact(
        "GMT abbreviation",
        &["-r", "%z %H"],
        b"16 Jun 1994 07:29:35 GMT x\n",
    );
    assert_exact_env(
        "numeric offset",
        &["-r", "%H:%M"],
        b"16 Jun 1994 07:29:35 +0200 x\n",
        &[("TZ", "UTC")],
    );
    assert_exact_env(
        "local timezone",
        &["-r", "%s"],
        b"16 Jun 1994 07:29:35 local\n",
        &[("TZ", "UTC")],
    );
}

#[test]
fn future_yearless_relative_dates_roll_to_previous_year() {
    assert_exact(
        "syslog rollover",
        &["-r", "%Y"],
        b"Dec 31 23:59:59 future-ish\n",
    );
    assert_exact(
        "lastlog rollover",
        &["-r", "%Y"],
        b"Sun Dec 31 23:59 lastlog\n",
    );
}

#[test]
fn status_and_stderr_success_cases() {
    for args in [
        &["prefix"][..],
        &["-r", "%Y"][..],
        &["-m"][..],
        &["-s"][..],
        &["-i"][..],
    ] {
        let ours = run_ts(OURS, args, b"plain\n", &[]);
        assert_eq!(ours.status.code, Some(0));
        assert_eq!(ours.stderr, b"");
        assert!(!ours.timed_out);
    }
    let usage = run_ts(OURS, &["-x"], b"", &[]);
    assert_eq!(usage.status.code, Some(255));
}
