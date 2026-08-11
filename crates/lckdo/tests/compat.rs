// SPDX-License-Identifier: GPL-2.0-only

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/lckdo";
const OURS: &str = env!("CARGO_BIN_EXE_lckdo");

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

#[derive(Debug)]
struct TimedOutput {
    output: RunOutput,
    elapsed: Duration,
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
    let mut child = command.spawn().expect("spawn lckdo");
    let writer = child.stdin.take().map(|mut child_stdin| {
        let stdin = stdin.to_vec();
        std::thread::spawn(move || match child_stdin.write_all(&stdin) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {}
            Err(err) => panic!("write stdin to lckdo: {err}"),
        })
    });
    let output = child.wait_with_output().expect("wait for lckdo");
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn run_lckdo(
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

fn run_lckdo_timed(
    program: &str,
    args: &[&str],
    stdin: &[u8],
    cwd: &Path,
    extra_env: &[(&str, &str)],
) -> TimedOutput {
    let start = Instant::now();
    let output = run_lckdo(program, args, stdin, cwd, extra_env);
    TimedOutput {
        output,
        elapsed: start.elapsed(),
    }
}

fn spawn_lckdo(program: &str, args: &[&str], cwd: &Path) -> Child {
    let mut command = base_command(program, cwd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.spawn().expect("spawn lckdo holder")
}

fn assert_compat(name: &str, args: &[&str], stdin: &[u8], cwd: &Path, extra_env: &[(&str, &str)]) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_lckdo(ORACLE, args, stdin, cwd, extra_env);
    let ours = run_lckdo(OURS, args, stdin, cwd, extra_env);
    assert_same(name, &oracle, &ours);
}

fn assert_compat_isolated(name: &str, args: &[&str]) {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    let oracle = run_lckdo(ORACLE, args, b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, args, b"", &ours_dir, &[]);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "lckdo compatibility mismatch in {name}\n\
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

fn assert_same_normalized_lock_pid(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    let oracle_norm = normalize_lock_pid(&oracle.stdout);
    let ours_norm = normalize_lock_pid(&ours.stdout);
    if oracle.status != ours.status || oracle.stderr != ours.stderr || oracle_norm != ours_norm {
        panic!(
            "lckdo compatibility mismatch in {name}\n\
             status: oracle={:?} ours={:?}\n\
             stdout: oracle={} ours={}\n\
             normalized stdout: oracle={} ours={}\n\
             stderr: oracle={} ours={}",
            oracle.status,
            ours.status,
            render_bytes(&oracle.stdout),
            render_bytes(&ours.stdout),
            render_bytes(oracle_norm.as_bytes()),
            render_bytes(ours_norm.as_bytes()),
            render_bytes(&oracle.stderr),
            render_bytes(&ours.stderr),
        );
    }
}

fn normalize_lock_pid(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.bytes().all(|b| b.is_ascii_digit() || b == b'\n')
        && text.bytes().any(|b| b.is_ascii_digit())
    {
        return "<pid>\n".to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_ref();
    while let Some(pos) = rest.find("process ") {
        out.push_str(&rest[..pos + "process ".len()]);
        let after = &rest[pos + "process ".len()..];
        let digit_count = after.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_count == 0 {
            rest = after;
        } else {
            out.push_str("<pid>");
            rest = &after[digit_count..];
        }
    }
    out.push_str(rest);
    out
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

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", path.display());
}

fn reap_holder(child: Child) {
    let output = child.wait_with_output().expect("wait holder");
    assert!(
        output.status.success(),
        "holder failed: status={:?} stdout={} stderr={}",
        StatusRepr::from(output.status),
        render_bytes(&output.stdout),
        render_bytes(&output.stderr),
    );
}

fn skip_if_root() -> bool {
    nix::unistd::geteuid().is_root()
}

#[test]
fn cli_parsing_and_option_combinations_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        ("no args prints help", &[]),
        ("lockfile but no program", &["lock"]),
        ("unknown option", &["-z", "lock", "true"]),
        (
            "long option is invalid short dash",
            &["--help", "lock", "true"],
        ),
        ("dashdash ends options", &["--", "lock", "true"]),
        ("cluster wait exclusive", &["-wx", "lock", "true"]),
        (
            "cluster shared exclusive last wins",
            &["-sx", "lock", "true"],
        ),
        (
            "cluster exclusive shared last wins",
            &["-xs", "lock", "true"],
        ),
        ("cluster quiet wait", &["-qw", "lock", "true"]),
        ("missing wait argument", &["-W"]),
        ("invalid wait nonnumeric", &["-W", "nope", "lock", "true"]),
        ("invalid wait zero", &["-W", "0", "lock", "true"]),
        ("invalid wait negative", &["-W", "-1", "lock", "true"]),
        ("invalid wait fractional", &["-W", "0.1", "lock", "true"]),
        ("inline wait", &["-W1", "lock", "true"]),
        ("wait accepts trailing junk", &["-W", "1q", "lock", "true"]),
        ("missing fd argument", &["-E"]),
        ("invalid fd negative", &["-E", "-1", "lock", "true"]),
        ("invalid fd stderr", &["-E", "2", "lock", "true"]),
        (
            "invalid fd leading zero stderr",
            &["-E", "02", "lock", "true"],
        ),
        ("fd nonnumeric becomes stdin", &["-E", "q", "lock", "true"]),
        (
            "fd three historical bad descriptor",
            &["-E3", "lock", "true"],
        ),
        ("inline fd four", &["-E4", "lock", "true"]),
        ("test mode no program", &["-t", "missing"]),
        ("quiet test mode no program", &["-qt", "missing"]),
        ("test implies no create", &["-tn", "missing"]),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
        let _ = std::fs::remove_file(cwd.join("lock"));
    }
}

#[test]
fn basic_locking_child_status_streams_arguments_and_environment_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str], &[(&str, &str)])] = &[
        ("child exits zero", &["lock", "sh", "-c", "exit 0"], &[]),
        ("child exits one", &["lock", "sh", "-c", "exit 1"], &[]),
        (
            "child exits forty two",
            &["lock", "sh", "-c", "exit 42"],
            &[],
        ),
        ("child exits 127", &["lock", "sh", "-c", "exit 127"], &[]),
        (
            "stdout stderr and argv",
            &[
                "lock",
                "sh",
                "-c",
                "printf 'argv0=%s arg1=%s token=%s' \"$0\" \"$1\" \"$LCKDO_TOKEN\"; printf err >&2; exit 7",
                "custom-argv0",
                "argument",
            ],
            &[("LCKDO_TOKEN", "from-env")],
        ),
    ];

    for (name, args, env) in cases {
        assert_compat(name, args, b"stdin is inherited\n", cwd, env);
        assert!(cwd.join("lock").exists(), "lock file should exist");
        let _ = std::fs::remove_file(cwd.join("lock"));
    }
}

#[test]
fn lockfile_creation_reuse_and_symlink_behaviour_match() {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();

    let oracle = run_lckdo(ORACLE, &["lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["lock", "true"], b"", &ours_dir, &[]);
    assert_same("new lockfile is created", &oracle, &ours);
    assert!(oracle_dir.join("lock").exists());
    assert!(ours_dir.join("lock").exists());

    let oracle = run_lckdo(ORACLE, &["lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["lock", "true"], b"", &ours_dir, &[]);
    assert_same("existing lockfile is reused", &oracle, &ours);

    std::fs::write(oracle_dir.join("target"), b"").unwrap();
    std::fs::write(ours_dir.join("target"), b"").unwrap();
    symlink("target", oracle_dir.join("link")).unwrap();
    symlink("target", ours_dir.join("link")).unwrap();
    let oracle = run_lckdo(ORACLE, &["link", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["link", "true"], b"", &ours_dir, &[]);
    assert_same("symlink lockfile", &oracle, &ours);
}

#[test]
fn command_execution_errors_scripts_and_signals_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let script = cwd.join("script-without-shebang");
    std::fs::write(
        &script,
        b"printf script-out; printf script-err >&2; exit 13\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let cases: &[(&str, &[&str], &[(&str, &str)])] = &[
        (
            "command not found",
            &["lock", "definitely-not-a-lckdo-compat-command"],
            &[],
        ),
        (
            "script without shebang by explicit path",
            &["lock", "./script-without-shebang"],
            &[],
        ),
        (
            "script without shebang via path",
            &["lock", "script-without-shebang"],
            &[("PATH", ".:/bin:/usr/bin")],
        ),
        (
            "child killed by term",
            &["lock", "sh", "-c", "kill -TERM $$"],
            &[],
        ),
        (
            "child writes streams then broken pipe signal",
            &[
                "lock",
                "sh",
                "-c",
                "printf out; printf err >&2; kill -PIPE $$",
            ],
            &[],
        ),
    ];

    for (name, args, env) in cases {
        assert_compat(name, args, b"", cwd, env);
        let _ = std::fs::remove_file(cwd.join("lock"));
    }
}

#[test]
fn file_open_errors_and_permissions_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    std::fs::create_dir(cwd.join("dir")).unwrap();
    assert_compat(
        "nonexistent directory",
        &["no/such/lock", "true"],
        b"",
        cwd,
        &[],
    );
    assert_compat(
        "no create missing",
        &["-n", "missing", "true"],
        b"",
        cwd,
        &[],
    );
    assert_compat("lockfile is directory", &["dir", "true"], b"", cwd, &[]);
    assert_compat("test mode directory", &["-t", "dir"], b"", cwd, &[]);

    if skip_if_root() {
        return;
    }

    std::fs::write(cwd.join("readonly"), b"").unwrap();
    std::fs::write(cwd.join("writeonly"), b"").unwrap();
    std::fs::write(cwd.join("none"), b"").unwrap();
    std::fs::set_permissions(cwd.join("readonly"), std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(
        cwd.join("writeonly"),
        std::fs::Permissions::from_mode(0o222),
    )
    .unwrap();
    std::fs::set_permissions(cwd.join("none"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let cases: &[(&str, &[&str])] = &[
        ("exclusive readonly", &["readonly", "true"]),
        ("shared readonly", &["-s", "readonly", "true"]),
        ("test readonly", &["-t", "readonly"]),
        ("exclusive writeonly", &["writeonly", "true"]),
        ("shared writeonly", &["-s", "writeonly", "true"]),
        ("test writeonly", &["-t", "writeonly"]),
        ("exclusive no permissions", &["none", "true"]),
        ("shared no permissions", &["-s", "none", "true"]),
        ("test no permissions", &["-t", "none"]),
    ];
    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
    }
}

#[test]
fn contention_immediate_quiet_wait_and_timeout_match() {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();

    let oracle_holder = spawn_lckdo(
        ORACLE,
        &["lock", "sh", "-c", "printf ready > oracle-ready; sleep 3"],
        &oracle_dir,
    );
    let ours_holder = spawn_lckdo(
        OURS,
        &["lock", "sh", "-c", "printf ready > ours-ready; sleep 3"],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("oracle-ready"));
    wait_for_file(&ours_dir.join("ours-ready"));

    let immediate_oracle = run_lckdo(ORACLE, &["lock", "true"], b"", &oracle_dir, &[]);
    let immediate_ours = run_lckdo(OURS, &["lock", "true"], b"", &ours_dir, &[]);
    assert_same("immediate contention", &immediate_oracle, &immediate_ours);

    let quiet_oracle = run_lckdo(ORACLE, &["-q", "lock", "true"], b"", &oracle_dir, &[]);
    let quiet_ours = run_lckdo(OURS, &["-q", "lock", "true"], b"", &ours_dir, &[]);
    assert_same("quiet contention", &quiet_oracle, &quiet_ours);

    let timeout_oracle =
        run_lckdo_timed(ORACLE, &["-W", "1", "lock", "true"], b"", &oracle_dir, &[]);
    let timeout_ours = run_lckdo_timed(OURS, &["-W", "1", "lock", "true"], b"", &ours_dir, &[]);
    assert_same(
        "timed contention",
        &timeout_oracle.output,
        &timeout_ours.output,
    );
    assert!(timeout_oracle.elapsed >= Duration::from_millis(900));
    assert!(timeout_ours.elapsed >= Duration::from_millis(900));

    reap_holder(oracle_holder);
    reap_holder(ours_holder);

    let oracle_holder = spawn_lckdo(
        ORACLE,
        &["lock", "sh", "-c", "printf ready > oracle-ready2; sleep 1"],
        &oracle_dir,
    );
    wait_for_file(&oracle_dir.join("oracle-ready2"));
    let wait_oracle = run_lckdo_timed(
        ORACLE,
        &["-w", "lock", "sh", "-c", "printf waited"],
        b"",
        &oracle_dir,
        &[],
    );
    reap_holder(oracle_holder);

    let ours_holder = spawn_lckdo(
        OURS,
        &["lock", "sh", "-c", "printf ready > ours-ready2; sleep 1"],
        &ours_dir,
    );
    wait_for_file(&ours_dir.join("ours-ready2"));
    let wait_ours = run_lckdo_timed(
        OURS,
        &["-w", "lock", "sh", "-c", "printf waited"],
        b"",
        &ours_dir,
        &[],
    );
    reap_holder(ours_holder);

    assert_same("wait until release", &wait_oracle.output, &wait_ours.output);
    assert!(wait_oracle.elapsed >= Duration::from_millis(700));
    assert!(wait_ours.elapsed >= Duration::from_millis(700));
}

#[test]
fn shared_and_exclusive_lock_modes_match() {
    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();

    let oracle_shared = spawn_lckdo(
        ORACLE,
        &[
            "-s",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-shared; sleep 2",
        ],
        &oracle_dir,
    );
    let ours_shared = spawn_lckdo(
        OURS,
        &[
            "-s",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-shared; sleep 2",
        ],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("ready-shared"));
    wait_for_file(&ours_dir.join("ready-shared"));

    let oracle = run_lckdo(
        ORACLE,
        &["-s", "lock", "sh", "-c", "printf shared"],
        b"",
        &oracle_dir,
        &[],
    );
    let ours = run_lckdo(
        OURS,
        &["-s", "lock", "sh", "-c", "printf shared"],
        b"",
        &ours_dir,
        &[],
    );
    assert_same("shared locks coexist", &oracle, &ours);

    let oracle = run_lckdo(ORACLE, &["lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["lock", "true"], b"", &ours_dir, &[]);
    assert_same("shared blocks exclusive", &oracle, &ours);
    reap_holder(oracle_shared);
    reap_holder(ours_shared);

    let oracle_exclusive = spawn_lckdo(
        ORACLE,
        &[
            "-x",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-exclusive; sleep 2",
        ],
        &oracle_dir,
    );
    let ours_exclusive = spawn_lckdo(
        OURS,
        &[
            "-x",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-exclusive; sleep 2",
        ],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("ready-exclusive"));
    wait_for_file(&ours_dir.join("ready-exclusive"));

    let oracle = run_lckdo(ORACLE, &["-s", "lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-s", "lock", "true"], b"", &ours_dir, &[]);
    assert_same("exclusive blocks shared", &oracle, &ours);

    let oracle = run_lckdo(ORACLE, &["-x", "lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-x", "lock", "true"], b"", &ours_dir, &[]);
    assert_same("explicit exclusive blocks exclusive", &oracle, &ours);
    reap_holder(oracle_exclusive);
    reap_holder(ours_exclusive);
}

#[test]
fn test_mode_available_held_shared_exclusive_and_quiet_match() {
    assert_compat_isolated("test nonexistent", &["-t", "missing"]);
    assert_compat_isolated("quiet test nonexistent", &["-qt", "missing"]);

    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    std::fs::write(oracle_dir.join("lock"), b"").unwrap();
    std::fs::write(ours_dir.join("lock"), b"").unwrap();

    let oracle = run_lckdo(ORACLE, &["-t", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-t", "lock"], b"", &ours_dir, &[]);
    assert_same("test available", &oracle, &ours);

    let oracle_holder = spawn_lckdo(
        ORACLE,
        &[
            "lock",
            "sh",
            "-c",
            "printf ready > ready-exclusive; sleep 2",
        ],
        &oracle_dir,
    );
    let ours_holder = spawn_lckdo(
        OURS,
        &[
            "lock",
            "sh",
            "-c",
            "printf ready > ready-exclusive; sleep 2",
        ],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("ready-exclusive"));
    wait_for_file(&ours_dir.join("ready-exclusive"));
    let oracle = run_lckdo(ORACLE, &["-t", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-t", "lock"], b"", &ours_dir, &[]);
    assert_same_normalized_lock_pid("test held exclusive", &oracle, &ours);
    let oracle = run_lckdo(ORACLE, &["-qt", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-qt", "lock"], b"", &ours_dir, &[]);
    assert_same_normalized_lock_pid("quiet test held exclusive", &oracle, &ours);
    let oracle = run_lckdo(ORACLE, &["-st", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-st", "lock"], b"", &ours_dir, &[]);
    assert_same_normalized_lock_pid("shared test blocked by exclusive", &oracle, &ours);
    reap_holder(oracle_holder);
    reap_holder(ours_holder);

    let oracle_holder = spawn_lckdo(
        ORACLE,
        &[
            "-s",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-shared; sleep 2",
        ],
        &oracle_dir,
    );
    let ours_holder = spawn_lckdo(
        OURS,
        &[
            "-s",
            "lock",
            "sh",
            "-c",
            "printf ready > ready-shared; sleep 2",
        ],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("ready-shared"));
    wait_for_file(&ours_dir.join("ready-shared"));
    let oracle = run_lckdo(ORACLE, &["-st", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-st", "lock"], b"", &ours_dir, &[]);
    assert_same("shared test ignores shared holder", &oracle, &ours);
    let oracle = run_lckdo(ORACLE, &["-xt", "lock"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["-xt", "lock"], b"", &ours_dir, &[]);
    assert_same_normalized_lock_pid("exclusive test sees shared holder", &oracle, &ours);
    reap_holder(oracle_holder);
    reap_holder(ours_holder);
}

#[test]
fn exec_modes_status_fd_and_lock_lifetime_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let cases: &[(&str, &[&str])] = &[
        (
            "exec direct status and streams",
            &[
                "-e",
                "lock",
                "sh",
                "-c",
                "printf out; printf err >&2; exit 42",
            ],
        ),
        (
            "exec direct command not found",
            &["-e", "lock", "definitely-not-a-lckdo-compat-command"],
        ),
        (
            "exec direct keeps default fd",
            &[
                "-e",
                "lock",
                "sh",
                "-c",
                "test -e /proc/$$/fd/3; printf fd3=$?",
            ],
        ),
        (
            "exec direct keeps selected fd",
            &[
                "-E",
                "4",
                "lock",
                "sh",
                "-c",
                "test -e /proc/$$/fd/4; printf fd4=$?",
            ],
        ),
        (
            "exec direct signal",
            &["-e", "lock", "sh", "-c", "kill -TERM $$"],
        ),
    ];

    for (name, args) in cases {
        assert_compat(name, args, b"", cwd, &[]);
        let _ = std::fs::remove_file(cwd.join("lock"));
    }

    let temp = tempfile::tempdir().unwrap();
    let oracle_dir = temp.path().join("oracle");
    let ours_dir = temp.path().join("ours");
    std::fs::create_dir_all(&oracle_dir).unwrap();
    std::fs::create_dir_all(&ours_dir).unwrap();
    let oracle_holder = spawn_lckdo(
        ORACLE,
        &[
            "-E",
            "4",
            "lock",
            "sh",
            "-c",
            "printf ready > ready; sleep 2",
        ],
        &oracle_dir,
    );
    let ours_holder = spawn_lckdo(
        OURS,
        &[
            "-E",
            "4",
            "lock",
            "sh",
            "-c",
            "printf ready > ready; sleep 2",
        ],
        &ours_dir,
    );
    wait_for_file(&oracle_dir.join("ready"));
    wait_for_file(&ours_dir.join("ready"));

    let oracle = run_lckdo(ORACLE, &["lock", "true"], b"", &oracle_dir, &[]);
    let ours = run_lckdo(OURS, &["lock", "true"], b"", &ours_dir, &[]);
    assert_same("exec mode keeps lock held", &oracle, &ours);
    reap_holder(oracle_holder);
    reap_holder(ours_holder);
}
