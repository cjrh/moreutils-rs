// SPDX-License-Identifier: GPL-3.0-or-later

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const ORACLE: &str = "/bin/zrun";
const OURS: &str = env!("CARGO_BIN_EXE_zrun");

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

fn command_exists(name: &str) -> bool {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "PATH=/bin:/usr/bin; command -v {} >/dev/null 2>&1",
            shell_quote(name)
        ))
        .status()
        .expect("run command -v")
        .success()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn run_zrun<P: AsRef<OsStr>>(
    program: P,
    args: &[OsString],
    cwd: &Path,
    tmpdir: &Path,
    extra_env: &[(&str, OsString)],
    path_prefix: Option<&Path>,
) -> RunOutput {
    let path = if let Some(prefix) = path_prefix {
        format!("{}:/bin:/usr/bin", prefix.display())
    } else {
        "/bin:/usr/bin".to_string()
    };
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", path)
        .env("LC_ALL", "C")
        .env("TMPDIR", tmpdir)
        .envs(extra_env.iter().map(|(key, value)| (*key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run zrun");
    RunOutput {
        status: output.status.into(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

fn assert_compat(name: &str, args: &[OsString], cwd: &Path, tmpdir: &Path) {
    assert_compat_env(name, args, cwd, tmpdir, &[], None);
}

fn assert_compat_env(
    name: &str,
    args: &[OsString],
    cwd: &Path,
    tmpdir: &Path,
    extra_env: &[(&str, OsString)],
    path_prefix: Option<&Path>,
) {
    assert!(
        Path::new(ORACLE).exists(),
        "{ORACLE} is required for compatibility tests"
    );
    let oracle = run_zrun(ORACLE, args, cwd, tmpdir, extra_env, path_prefix);
    let ours = run_zrun(OURS, args, cwd, tmpdir, extra_env, path_prefix);
    assert_same(name, &oracle, &ours);
}

fn assert_same(name: &str, oracle: &RunOutput, ours: &RunOutput) {
    if oracle != ours {
        panic!(
            "zrun compatibility mismatch in {name}\n\
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

fn os(s: impl AsRef<OsStr>) -> OsString {
    s.as_ref().to_os_string()
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn write_probe(path: &Path) {
    write_executable(
        path,
        r#"#!/bin/sh
if [ -n "$ZRUN_MARKER" ]; then
	: > "$ZRUN_MARKER"
fi
if [ -n "$ZRUN_ARG_LOG" ]; then
	: > "$ZRUN_ARG_LOG"
fi
i=0
for arg do
	i=$((i + 1))
	case "$arg" in
		"$TMPDIR"/*)
			base=${arg##*/}
			suffix=${base#*-}
			printf 'ARG %d TMP %s\n' "$i" "$suffix"
			;;
		*)
			printf 'ARG %d LIT %s\n' "$i" "$arg"
			;;
	esac
	if [ -f "$arg" ]; then
		printf 'DATA %d ' "$i"
		od -An -tx1 -v "$arg" | tr -d ' \n'
		printf '\n'
	fi
	if [ -n "$ZRUN_ARG_LOG" ]; then
		printf '%s\n' "$arg" >> "$ZRUN_ARG_LOG"
	fi
done
exit "${ZRUN_PROBE_STATUS:-0}"
"#,
    );
}

fn run_to_file(program: &str, args: &[&OsStr], stdout_path: &Path) {
    let stdout = File::create(stdout_path)
        .unwrap_or_else(|err| panic!("create {}: {err}", stdout_path.display()));
    let output = Command::new(program)
        .args(args)
        .env("PATH", "/bin:/usr/bin")
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|err| panic!("run {program}: {err}"));
    assert!(
        output.status.success(),
        "{program} failed with {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn make_compressed(input: &Path, output: &Path, ext: &str) {
    match ext {
        "gz" | "Z" => run_to_file("gzip", &[OsStr::new("-c"), input.as_os_str()], output),
        "bz2" => run_to_file("bzip2", &[OsStr::new("-c"), input.as_os_str()], output),
        "xz" => run_to_file("xz", &[OsStr::new("-c"), input.as_os_str()], output),
        "lzma" => run_to_file("lzma", &[OsStr::new("-c"), input.as_os_str()], output),
        "lzo" => run_to_file("lzop", &[OsStr::new("-c"), input.as_os_str()], output),
        "zst" => run_to_file(
            "zstd",
            &[OsStr::new("-q"), OsStr::new("-c"), input.as_os_str()],
            output,
        ),
        other => panic!("unknown compression extension {other}"),
    }
}

fn compressed_format_available(ext: &str) -> bool {
    match ext {
        "gz" | "Z" => command_exists("gzip"),
        "bz2" => command_exists("bzip2"),
        "xz" => command_exists("xz"),
        "lzma" => command_exists("lzma"),
        "lzo" => command_exists("lzop"),
        "zst" => command_exists("zstd"),
        _ => false,
    }
}

fn assert_logged_temp_paths_removed(log: &Path, tmpdir: &Path) {
    let log = fs::read_to_string(log).unwrap_or_else(|err| panic!("read {}: {err}", log.display()));
    let mut saw_temp = false;
    for line in log.lines() {
        let path = Path::new(line);
        if path.starts_with(tmpdir) {
            saw_temp = true;
            assert!(
                !path.exists(),
                "temporary path {} was not removed",
                path.display()
            );
        }
    }
    assert!(saw_temp, "probe did not receive any paths under TMPDIR");
}

#[test]
fn cli_parsing_status_and_plain_arguments_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("tmp");
    fs::create_dir(&tmpdir).unwrap();
    let probe = cwd.join("probe");
    write_probe(&probe);

    fs::write(cwd.join("plain.txt"), b"plain data\n").unwrap();

    let cases: Vec<(&str, Vec<OsString>)> = vec![
        ("no args", vec![]),
        ("command without file args", vec![os("/bin/true")]),
        ("plain args", vec![os(&probe), os("alpha"), os("plain.txt")]),
        (
            "option-looking plain args",
            vec![os(&probe), os("-n"), os("--"), os("plain.txt")],
        ),
    ];
    for (name, args) in cases {
        assert_compat(name, &args, cwd, &tmpdir);
    }

    for status in [0, 1, 42] {
        assert_compat_env(
            &format!("command exits {status}"),
            &[os(&probe), os("plain.txt")],
            cwd,
            &tmpdir,
            &[("ZRUN_PROBE_STATUS", os(status.to_string()))],
            None,
        );
    }

    assert_compat(
        "command not found",
        &[os("definitely-not-a-zrun-test-command"), os("plain.txt")],
        cwd,
        &tmpdir,
    );

    #[cfg(unix)]
    {
        let noexec = cwd.join("not-executable");
        fs::write(&noexec, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&noexec).unwrap().permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&noexec, permissions).unwrap();
        assert_compat(
            "command is not executable",
            &[os(&noexec), os("plain.txt")],
            cwd,
            &tmpdir,
        );
    }
}

#[test]
fn compressed_formats_and_filename_matching_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("tmp");
    fs::create_dir(&tmpdir).unwrap();
    let probe = cwd.join("probe");
    write_probe(&probe);

    let source = cwd.join("source");
    fs::write(&source, b"first line\nsecond line\0with nul\n").unwrap();
    fs::create_dir(cwd.join("dir with spaces")).unwrap();

    let mut args = vec![os(&probe)];
    let mut compressed_count = 0;
    let supported_names = [
        ("gz", "alpha.gz"),
        ("gz", ".gz"),
        ("Z", "upper.Z"),
        ("bz2", "dir with spaces/name with spaces.bz2"),
        ("xz", "many.dots.name.xz"),
        ("lzma", ".hidden.lzma"),
        ("lzo", "tab\tname.lzo"),
    ];
    for (ext, name) in supported_names {
        if compressed_format_available(ext) {
            let path = cwd.join(name);
            make_compressed(&source, &path, ext);
            args.push(os(name));
            compressed_count += 1;
        }
    }

    #[cfg(unix)]
    if compressed_format_available("gz") {
        let name = OsString::from_vec(b"nonutf-\xff.gz".to_vec());
        let path = cwd.join(PathBuf::from(name.clone()));
        make_compressed(&source, &path, "gz");
        args.push(name);
        compressed_count += 1;
    }

    fs::write(
        cwd.join("not-compressed.gz.txt"),
        b"suffix is not at the end\n",
    )
    .unwrap();
    args.push(os("not-compressed.gz.txt"));

    if compressed_format_available("zst") {
        let zst = cwd.join("unsupported.zst");
        make_compressed(&source, &zst, "zst");
        args.push(os("unsupported.zst"));
    }

    if compressed_format_available("gz") {
        let upper_gz = cwd.join("unsupported.GZ");
        make_compressed(&source, &upper_gz, "gz");
        args.push(os("unsupported.GZ"));
    }

    assert!(
        compressed_count > 0,
        "no compressors available for zrun tests"
    );
    assert_compat("formats and filename matching", &args, cwd, &tmpdir);
}

#[test]
fn multiple_files_repeated_files_tmpdir_and_cleanup_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("custom tmp");
    fs::create_dir(&tmpdir).unwrap();
    let probe = cwd.join("probe");
    write_probe(&probe);

    let source1 = cwd.join("source1");
    let source2 = cwd.join("source2");
    fs::write(&source1, b"one\n").unwrap();
    fs::write(&source2, b"two\n").unwrap();
    fs::write(cwd.join("plain"), b"plain\n").unwrap();

    assert!(compressed_format_available("gz"));
    make_compressed(&source1, &cwd.join("one.gz"), "gz");
    let second_ext = if compressed_format_available("xz") {
        "xz"
    } else {
        "gz"
    };
    make_compressed(&source2, &cwd.join(format!("two.{second_ext}")), second_ext);

    let args = vec![
        os(&probe),
        os("one.gz"),
        os("plain"),
        os(format!("two.{second_ext}")),
        os("one.gz"),
    ];

    let oracle_log = cwd.join("oracle-args.log");
    let ours_log = cwd.join("ours-args.log");
    let oracle = run_zrun(
        ORACLE,
        &args,
        cwd,
        &tmpdir,
        &[("ZRUN_ARG_LOG", os(&oracle_log))],
        None,
    );
    let ours = run_zrun(
        OURS,
        &args,
        cwd,
        &tmpdir,
        &[("ZRUN_ARG_LOG", os(&ours_log))],
        None,
    );
    assert_same("multiple files, repeated files, TMPDIR", &oracle, &ours);
    assert_logged_temp_paths_removed(&oracle_log, &tmpdir);
    assert_logged_temp_paths_removed(&ours_log, &tmpdir);
}

#[test]
fn decompression_failure_prevents_command_and_cleans_up_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("tmp");
    fs::create_dir(&tmpdir).unwrap();
    let probe = cwd.join("probe");
    write_probe(&probe);
    fs::write(cwd.join("bad.gz"), b"not really gzip").unwrap();

    let oracle_marker = cwd.join("oracle-marker");
    let ours_marker = cwd.join("ours-marker");
    let args = vec![os(&probe), os("bad.gz")];
    let oracle = run_zrun(
        ORACLE,
        &args,
        cwd,
        &tmpdir,
        &[("ZRUN_MARKER", os(&oracle_marker))],
        None,
    );
    let ours = run_zrun(
        OURS,
        &args,
        cwd,
        &tmpdir,
        &[("ZRUN_MARKER", os(&ours_marker))],
        None,
    );
    assert_same("invalid compressed input", &oracle, &ours);
    assert!(!oracle_marker.exists(), "oracle unexpectedly ran command");
    assert!(!ours_marker.exists(), "ours unexpectedly ran command");
    assert!(
        fs::read_dir(&tmpdir).unwrap().next().is_none(),
        "temporary directory not empty after decompression failure"
    );
}

#[test]
fn command_signal_diagnostics_match() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("tmp");
    fs::create_dir(&tmpdir).unwrap();

    assert_compat(
        "command killed by signal without compressed args",
        &[os("/bin/sh"), os("-c"), os("kill -TERM $$")],
        cwd,
        &tmpdir,
    );

    if compressed_format_available("gz") {
        fs::write(cwd.join("source"), b"payload\n").unwrap();
        make_compressed(&cwd.join("source"), &cwd.join("payload.gz"), "gz");
        assert_compat(
            "command killed by signal with compressed args",
            &[
                os("/bin/sh"),
                os("-c"),
                os("kill -TERM $$"),
                os("payload.gz"),
            ],
            cwd,
            &tmpdir,
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_invocation_mode_matches() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path();
    let tmpdir = cwd.join("tmp");
    fs::create_dir(&tmpdir).unwrap();
    let probe = cwd.join("probe");
    write_probe(&probe);

    fs::write(cwd.join("source"), b"symlink payload\n").unwrap();
    assert!(compressed_format_available("gz"));
    make_compressed(&cwd.join("source"), &cwd.join("payload.gz"), "gz");

    let oracle_dir = cwd.join("oracle-bin");
    let ours_dir = cwd.join("ours-bin");
    fs::create_dir(&oracle_dir).unwrap();
    fs::create_dir(&ours_dir).unwrap();
    let oracle_link: PathBuf = oracle_dir.join("zprobe");
    let ours_link: PathBuf = ours_dir.join("zprobe");
    symlink(ORACLE, &oracle_link).unwrap();
    symlink(OURS, &ours_link).unwrap();

    let args = vec![os("payload.gz")];
    let oracle = run_zrun(&oracle_link, &args, cwd, &tmpdir, &[], Some(cwd));
    let ours = run_zrun(&ours_link, &args, cwd, &tmpdir, &[], Some(cwd));
    assert_same("symlink zprobe invocation", &oracle, &ours);

    let oracle_no_args = run_zrun(&oracle_link, &[], cwd, &tmpdir, &[], Some(cwd));
    let ours_no_args = run_zrun(&ours_link, &[], cwd, &tmpdir, &[], Some(cwd));
    assert_same("symlink zprobe usage", &oracle_no_args, &ours_no_args);
}
