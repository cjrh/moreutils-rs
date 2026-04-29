// SPDX-License-Identifier: GPL-3.0-or-later

use cjrh_moreutils_common::exit_with_status;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::Builder;

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

fn decompressor(ext: &str) -> &'static [&'static str] {
    match ext {
        "bz2" => &["bzip2", "-d", "-c"],
        "xz" => &["xz", "-d", "-c"],
        "lzo" => &["lzop", "-d", "-c"],
        "lzma" => &["lzma", "-d", "-c"],
        _ => &["gzip", "-d", "-c"],
    }
}

#[cfg(unix)]
fn compressed_arg(path: &OsStr) -> Option<(&'static str, OsString)> {
    let bytes = path.as_bytes();
    for (ext, ext_bytes) in [
        ("gz", b"gz".as_slice()),
        ("Z", b"Z".as_slice()),
        ("bz2", b"bz2".as_slice()),
        ("xz", b"xz".as_slice()),
        ("lzo", b"lzo".as_slice()),
        ("lzma", b"lzma".as_slice()),
    ] {
        let suffix_len = ext_bytes.len() + 1;
        if bytes.len() >= suffix_len
            && bytes[bytes.len() - suffix_len] == b'.'
            && &bytes[bytes.len() - ext_bytes.len()..] == ext_bytes
        {
            let basename_start = bytes[..bytes.len() - suffix_len]
                .iter()
                .rposition(|&byte| byte == b'/')
                .map_or(0, |index| index + 1);
            let mut suffix = Vec::with_capacity(bytes.len() - suffix_len - basename_start + 1);
            suffix.push(b'-');
            suffix.extend_from_slice(&bytes[basename_start..bytes.len() - suffix_len]);
            return Some((ext, OsString::from_vec(suffix)));
        }
    }
    None
}

#[cfg(not(unix))]
fn compressed_arg(path: &OsStr) -> Option<(&'static str, OsString)> {
    let path = path.to_str()?;
    for ext in ["gz", "Z", "bz2", "xz", "lzo", "lzma"] {
        if path.ends_with(&format!(".{ext}")) {
            let stem = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            return Some((ext, format!("-{stem}").into()));
        }
    }
    None
}

#[cfg(unix)]
fn linked_program(argv0: &OsStr) -> Option<OsString> {
    let base = Path::new(argv0).file_name()?.as_bytes();
    let stripped = base.strip_prefix(b"z")?;
    if stripped.is_empty() || stripped == b"run" {
        None
    } else {
        Some(OsString::from_vec(stripped.to_vec()))
    }
}

#[cfg(not(unix))]
fn linked_program(argv0: &OsStr) -> Option<OsString> {
    let base = Path::new(argv0).file_name()?.to_str()?;
    let stripped = base.strip_prefix('z')?;
    if stripped.is_empty() || stripped == "run" {
        None
    } else {
        Some(stripped.into())
    }
}

fn display_os(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn perl_exec_error(err: &io::Error) -> (&'static str, i32) {
    match err.raw_os_error() {
        Some(2) => ("No such file or directory", 2),
        Some(13) => ("Permission denied", 13),
        _ => ("No such file or directory", 2),
    }
}

fn main() {
    let argv0 = env::args_os().next().unwrap_or_else(|| "zrun".into());
    let mut args: Vec<OsString> = env::args_os().skip(1).collect();
    let program = if let Some(stripped) = linked_program(&argv0) {
        if args.is_empty() {
            let stripped = display_os(&stripped);
            eprintln!("Usage: z{stripped} <args>\nEquivalent to: zrun {stripped} <args>");
            std::process::exit(255);
        }
        stripped
    } else {
        if args.is_empty() {
            eprintln!("Usage: zrun <command> <args>");
            std::process::exit(255);
        }
        args.remove(0)
    };
    if args.is_empty() {
        eprintln!("Usage: zrun <command> <args>");
        std::process::exit(255);
    }

    let mut temporaries = Vec::new();
    let mut final_args: Vec<OsString> = Vec::new();
    for arg in args {
        if let Some((ext, suffix)) = compressed_arg(&arg) {
            let tmp = match Builder::new().suffix(&suffix).tempfile() {
                Ok(tmp) => tmp,
                Err(e) => {
                    eprintln!("zrun: cannot create temporary file: {e}");
                    drop(temporaries);
                    std::process::exit(1);
                }
            };
            let spec = decompressor(ext);
            let tmp_stdout = match tmp.reopen() {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("zrun: cannot create temporary file: {e}");
                    drop(tmp);
                    drop(temporaries);
                    std::process::exit(1);
                }
            };
            let status = Command::new(spec[0])
                .args(&spec[1..])
                .arg(&arg)
                .stdout(Stdio::from(tmp_stdout))
                .status();
            let status = match status {
                Ok(status) => status,
                Err(err) => {
                    let (message, _code) = perl_exec_error(&err);
                    eprintln!(
                        "Can't exec \"{}\": {message} at /bin/zrun line 76.",
                        spec[0]
                    );
                    eprintln!(
                        "zrun: preprocessing for {} terminated with code 2",
                        display_os(&arg)
                    );
                    drop(tmp);
                    drop(temporaries);
                    std::process::exit(25);
                }
            };
            if !status.success() {
                if let Some(code) = status.code() {
                    eprintln!(
                        "zrun: preprocessing for {} terminated with code {code}",
                        display_os(&arg)
                    );
                } else {
                    eprintln!(
                        "zrun: preprocessing for {} terminated abnormally: 1",
                        display_os(&arg)
                    );
                }
                drop(tmp);
                drop(temporaries);
                std::process::exit(25);
            }
            final_args.push(tmp.path().as_os_str().to_os_string());
            temporaries.push(tmp);
        } else {
            final_args.push(arg);
        }
    }

    let status = Command::new(&program).args(&final_args).status();
    let status = match status {
        Ok(status) => status,
        Err(err) => {
            let program_display = display_os(&program);
            let (message, code) = perl_exec_error(&err);
            eprintln!("Can't exec \"{program_display}\": {message} at /bin/zrun line 98.");
            eprintln!("zrun: {program_display} terminated abnormally: -1");
            drop(temporaries);
            std::process::exit(code);
        }
    };
    if !status.success() && status.code().is_none() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            eprintln!(
                "zrun: {} terminated abnormally: {}",
                display_os(&program),
                status.signal().unwrap_or(1)
            );
            let had_temporaries = !temporaries.is_empty();
            drop(temporaries);
            std::process::exit(if had_temporaries { 25 } else { 255 });
        }
    }
    drop(temporaries);
    exit_with_status(status);
}
