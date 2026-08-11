// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/ifdata";
const OURS: &str = env!("CARGO_BIN_EXE_ifdata");
const LOOPBACK: &str = "lo";
const MISSING_IFACE: &str = "no_such_if_xyz";

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

fn base_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/bin:/usr/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish_command(mut command: Command, stdin: &[u8]) -> RunOutput {
    let mut child = command.spawn().expect("spawn ifdata");
    if let Some(mut child_stdin) = child.stdin.take() {
        match child_stdin.write_all(stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to ifdata: {err}"),
        }
    }
    let output = child.wait_with_output().expect("wait for ifdata");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_ifdata(program: &str, args: &[&str]) -> RunOutput {
    let mut command = base_command(program);
    command.args(args);
    finish_command(command, b"")
}

fn assert_compat(name: &str, args: &[&str]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_ifdata(ORACLE, args);
    let ours = run_ifdata(OURS, args);
    assert_same(name, &oracle, &ours);
}

fn assert_missing_stats_error(name: &str, args: &[&str]) {
    let ours = run_ifdata(OURS, args);
    assert_eq!(ours.status.code, Some(1), "{name}: status");
    assert!(
        ours.stdout.is_empty(),
        "{name}: stdout={}",
        render_bytes(&ours.stdout)
    );
    assert_eq!(
        ours.stderr,
        format!("Error getting statistics for {MISSING_IFACE}\n").into_bytes(),
        "{name}: stderr"
    );
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "ifdata compatibility mismatch in {name}\n\
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

fn stdout_words(output: &RunOutput) -> Vec<&str> {
    std::str::from_utf8(&output.stdout)
        .expect("ifdata stdout is utf-8")
        .split_whitespace()
        .collect()
}

fn assert_unsigned_fields(name: &str, output: &RunOutput, fields: usize) {
    assert_eq!(output.status.code, Some(0), "{name}: status");
    assert!(
        output.stderr.is_empty(),
        "{name}: stderr={}",
        render_bytes(&output.stderr)
    );
    let words = stdout_words(output);
    assert_eq!(
        words.len(),
        fields,
        "{name}: stdout={}",
        render_bytes(&output.stdout)
    );
    for word in words {
        word.parse::<u64>()
            .unwrap_or_else(|_| panic!("{name}: {word:?} is not an unsigned integer"));
    }
}

fn assert_stat_shape_compat(name: &str, args: &[&str], fields: usize) {
    assert!(Path::new(ORACLE).exists());
    let oracle = run_ifdata(ORACLE, args);
    let ours = run_ifdata(OURS, args);
    assert_eq!(oracle.status, ours.status, "{name}: status mismatch");
    assert_eq!(oracle.stderr, ours.stderr, "{name}: stderr mismatch");
    assert_unsigned_fields(&format!("oracle {name}"), &oracle, fields);
    assert_unsigned_fields(&format!("ours {name}"), &ours, fields);
}

fn assert_rate_compat(name: &str, args: &[&str]) {
    assert!(Path::new(ORACLE).exists());
    let oracle_start = Instant::now();
    let oracle = run_ifdata(ORACLE, args);
    let oracle_elapsed = oracle_start.elapsed();
    let ours_start = Instant::now();
    let ours = run_ifdata(OURS, args);
    let ours_elapsed = ours_start.elapsed();

    assert_eq!(oracle.status, ours.status, "{name}: status mismatch");
    assert_eq!(oracle.stderr, ours.stderr, "{name}: stderr mismatch");
    assert_unsigned_fields(&format!("oracle {name}"), &oracle, 1);
    assert_unsigned_fields(&format!("ours {name}"), &ours, 1);
    assert!(
        oracle_elapsed >= Duration::from_millis(800) && ours_elapsed >= Duration::from_millis(800),
        "{name}: rate commands should wait about one second: oracle={oracle_elapsed:?} ours={ours_elapsed:?}"
    );
}

fn non_loopback_interfaces() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut names: Vec<_> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != LOOPBACK)
        .collect();
    names.sort();
    names
}

fn first_non_loopback_with_hardware_address() -> Option<String> {
    non_loopback_interfaces().into_iter().find(|name| {
        let address =
            std::fs::read_to_string(Path::new("/sys/class/net").join(name).join("address"))
                .unwrap_or_default();
        let address = address.trim();
        !address.is_empty() && address != "00:00:00:00:00:00"
    })
}

fn first_non_loopback_with_ipv4() -> Option<String> {
    non_loopback_interfaces().into_iter().find(|name| {
        let output = run_ifdata(ORACLE, &["-pa", name]);
        output.status.code == Some(0)
            && std::str::from_utf8(&output.stdout)
                .ok()
                .is_some_and(|stdout| stdout.trim().parse::<Ipv4Addr>().is_ok())
    })
}

fn first_non_loopback_without_ipv4() -> Option<String> {
    non_loopback_interfaces().into_iter().find(|name| {
        let output = run_ifdata(ORACLE, &["-pa", name]);
        output.status.code == Some(0) && output.stdout == b"NON-IP\n"
    })
}

#[cfg(target_os = "linux")]
fn assert_namespace_scenario(name: &str, unshare_args: &[&str], script: &str, expected: &[u8]) {
    if !Path::new("/bin/unshare").exists() && !Path::new("/usr/bin/unshare").exists() {
        eprintln!("skipping {name}: unshare is unavailable");
        return;
    }

    let mut command = base_command("unshare");
    command
        .env("OURS", OURS)
        .args(unshare_args)
        .args(["sh", "-c", script]);
    let output = finish_command(command, b"");

    if !output.stdout.starts_with(b"READY\n") {
        eprintln!(
            "skipping {name}: isolated network setup failed: status={:?}, stdout={}, stderr={}",
            output.status,
            render_bytes(&output.stdout),
            render_bytes(&output.stderr)
        );
        return;
    }

    assert_eq!(output.status.code, Some(0), "{name}: status");
    assert_eq!(output.stdout, expected, "{name}: stdout");
    assert!(
        output.stderr.is_empty(),
        "{name}: stderr={}",
        render_bytes(&output.stderr)
    );
}

#[test]
fn cli_parsing_and_usage_match() {
    let cases: &[(&str, &[&str])] = &[
        ("no args", &[]),
        ("only option", &["-e"]),
        ("interface but no option", &[LOOPBACK]),
        ("missing interface as lone name", &[MISSING_IFACE]),
        ("unknown option", &["-x", LOOPBACK]),
        ("help", &["-h"]),
        ("help with interface", &["-h", LOOPBACK]),
        ("long option rejected", &["--help", LOOPBACK]),
        ("too many non-options", &[LOOPBACK, "extra"]),
        ("option and too many args", &["-p", LOOPBACK, "extra"]),
        ("combined-looking invalid option", &["-pelo"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }
}

#[test]
fn multiple_options_are_executed_in_order() {
    let cases: &[(&str, &[&str])] = &[
        ("p then e", &["-p", "-e", LOOPBACK]),
        ("e then p", &["-e", "-p", LOOPBACK]),
        ("pe then p missing", &["-pe", "-p", MISSING_IFACE]),
        ("p then pe missing", &["-p", "-pe", MISSING_IFACE]),
        ("address then netmask", &["-pa", "-pn", LOOPBACK]),
        ("netmask then address", &["-pn", "-pa", LOOPBACK]),
        ("stats then stat field", &["-si", "-sip", LOOPBACK]),
        ("stat field then stats", &["-sip", "-si", LOOPBACK]),
    ];

    for (name, args) in cases {
        if name.contains("stats") || name.contains("stat field") {
            // Counters can change between runs, so compare status/stderr and numeric shape.
            continue;
        }
        assert_compat(name, args);
    }
    assert_stat_shape_compat("stats then stat field", &["-si", "-sip", LOOPBACK], 9);
    assert_stat_shape_compat("stat field then stats", &["-sip", "-si", LOOPBACK], 9);
}

#[test]
fn interface_existence_matches() {
    let cases: &[(&str, &[&str])] = &[
        ("existing e", &["-e", LOOPBACK]),
        ("existing pe", &["-pe", LOOPBACK]),
        ("missing e", &["-e", MISSING_IFACE]),
        ("missing pe", &["-pe", MISSING_IFACE]),
        ("missing p", &["-p", MISSING_IFACE]),
        ("missing pa", &["-pa", MISSING_IFACE]),
        ("missing pn", &["-pn", MISSING_IFACE]),
        ("missing pN", &["-pN", MISSING_IFACE]),
        ("missing pb", &["-pb", MISSING_IFACE]),
        ("missing pm", &["-pm", MISSING_IFACE]),
        ("missing ph", &["-ph", MISSING_IFACE]),
        ("missing pf", &["-pf", MISSING_IFACE]),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }

    // Upstream ifdata varies by distro here: Fedora aborts with a stack-smash
    // diagnostic, while Ubuntu reports a regular error. Require our stable,
    // non-crashing error rather than compare a platform-specific defect.
    for (name, args) in [
        ("missing si", &["-si", MISSING_IFACE][..]),
        ("missing so", &["-so", MISSING_IFACE][..]),
        ("missing bips", &["-bips", MISSING_IFACE][..]),
    ] {
        assert_missing_stats_error(name, args);
    }
}

#[test]
fn ipv4_and_non_ip_configuration_match() {
    let cases: &[(&str, &[&str])] = &[
        ("whole config", &["-p", LOOPBACK]),
        ("address", &["-pa", LOOPBACK]),
        ("netmask", &["-pn", LOOPBACK]),
        ("network", &["-pN", LOOPBACK]),
        ("broadcast", &["-pb", LOOPBACK]),
        ("mtu", &["-pm", LOOPBACK]),
    ];

    for (name, args) in cases {
        assert_compat(name, args);
    }

    if let Some(iface) = first_non_loopback_with_ipv4() {
        let cases: &[(&str, &[&str])] = &[
            ("non-loopback whole config", &["-p", &iface]),
            ("non-loopback address", &["-pa", &iface]),
            ("non-loopback netmask", &["-pn", &iface]),
            ("non-loopback network", &["-pN", &iface]),
            ("non-loopback broadcast", &["-pb", &iface]),
            ("non-loopback mtu", &["-pm", &iface]),
        ];
        for (name, args) in cases {
            assert_compat(name, args);
        }
    }

    if let Some(iface) = first_non_loopback_without_ipv4() {
        let cases: &[(&str, &[&str])] = &[
            ("non-IP whole config", &["-p", &iface]),
            ("non-IP address", &["-pa", &iface]),
            ("non-IP netmask", &["-pn", &iface]),
            ("non-IP network", &["-pN", &iface]),
            ("non-IP broadcast", &["-pb", &iface]),
            ("non-IP mtu", &["-pm", &iface]),
        ];
        for (name, args) in cases {
            assert_compat(name, args);
        }
    }
}

#[test]
fn flags_and_hardware_address_match() {
    assert_compat("loopback no hardware address", &["-ph", LOOPBACK]);
    assert_compat("loopback flags", &["-pf", LOOPBACK]);

    if let Some(iface) = first_non_loopback_with_hardware_address() {
        assert_compat(
            "hardware address on ethernet-like interface",
            &["-ph", &iface],
        );
        assert_compat("flags on ethernet-like interface", &["-pf", &iface]);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn broadcastless_addresses_and_labeled_aliases_work() {
    let script = r#"
set -eu
mount --make-rprivate / 2>/dev/null || true
mount -t sysfs sysfs /sys
ip link add ifdata0 type veth peer name ifdata1
ip link set ifdata0 address 02:00:00:00:00:01 mtu 1400 up
ip link set ifdata1 up
ip addr add 192.0.2.1/24 dev ifdata0
printf 'READY\n'
"$OURS" -pb ifdata0
ip addr add 192.0.2.2/24 dev ifdata0 label ifdata0:alias
"$OURS" -pb ifdata0:alias
"$OURS" -pm ifdata0:alias
"$OURS" -ph ifdata0:alias
"$OURS" -p ifdata0:alias
"#;
    assert_namespace_scenario(
        "broadcastless addresses and labeled aliases",
        &["--user", "--map-root-user", "--net", "--mount"],
        script,
        b"READY\n0.0.0.0\n0.0.0.0\n1400\n02:00:00:00:00:01\n192.0.2.2 255.255.255.0 0.0.0.0 1400\n",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn interface_queries_use_the_current_network_namespace() {
    let script = r#"
set -eu
ip link add ifdatans0 type veth peer name ifdatans1
ip link set ifdatans0 address 02:00:00:00:00:02 mtu 1401 up
ip link set ifdatans1 up
ip addr add 192.0.2.10/24 dev ifdatans0
printf 'READY\n'
"$OURS" -pm ifdatans0
"$OURS" -ph ifdatans0
"$OURS" -p ifdatans0
"#;
    assert_namespace_scenario(
        "current network namespace",
        &["--user", "--map-root-user", "--net"],
        script,
        b"READY\n1401\n02:00:00:00:00:02\n192.0.2.10 255.255.255.0 0.0.0.0 1401\n",
    );
}

#[test]
fn loopback_statistics_shape_matches() {
    let full = [("input stats", "-si"), ("output stats", "-so")];
    for (name, opt) in full {
        assert_stat_shape_compat(name, &[opt, LOOPBACK], 8);
    }

    let fields = [
        ("input packets", "-sip"),
        ("input bytes", "-sib"),
        ("input errors", "-sie"),
        ("input drops", "-sid"),
        ("input fifo", "-sif"),
        ("input compressed", "-sic"),
        ("input multicast", "-sim"),
        ("output packets", "-sop"),
        ("output bytes", "-sob"),
        ("output errors", "-soe"),
        ("output drops", "-sod"),
        ("output fifo", "-sof"),
        ("output collisions", "-sox"),
        ("output carrier", "-soc"),
        ("output multicast", "-som"),
    ];
    for (name, opt) in fields {
        assert_stat_shape_compat(name, &[opt, LOOPBACK], 1);
    }
}

#[test]
fn loopback_rate_options_match_shape_and_timing() {
    assert_rate_compat("incoming bytes per second", &["-bips", LOOPBACK]);
    assert_rate_compat("outgoing bytes per second", &["-bops", LOOPBACK]);
}
