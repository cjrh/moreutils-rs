// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::exit_with_status;
use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::Builder;

fn decompressor(ext: &str) -> &'static [&'static str] {
    match ext {
        "bz2" => &["bzip2", "-d", "-c"],
        "xz" => &["xz", "-d", "-c"],
        "lzo" => &["lzop", "-d", "-c"],
        "lzma" => &["lzma", "-d", "-c"],
        "zst" => &["zstd", "-d", "-c"],
        _ => &["gzip", "-d", "-c"],
    }
}

fn compressed_ext(path: &str) -> Option<&'static str> {
    for ext in ["gz", "Z", "bz2", "xz", "lzo", "lzma", "zst"] {
        if path.ends_with(&format!(".{ext}")) {
            return Some(ext);
        }
    }
    None
}

fn main() {
    let argv0 = env::args().next().unwrap_or_else(|| "zrun".into());
    let base = Path::new(&argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("zrun");
    let mut args: Vec<String> = env::args().skip(1).collect();
    let program = if let Some(stripped) = base
        .strip_prefix('z')
        .filter(|s| *s != "run" && !s.is_empty())
    {
        if args.is_empty() {
            eprintln!("Usage: z{stripped} <args>\nEquivalent to: zrun {stripped} <args>");
            std::process::exit(255);
        }
        stripped.to_string()
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
        if let Some(ext) = compressed_ext(&arg) {
            let stem = Path::new(&arg)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("zrun");
            let tmp = Builder::new()
                .suffix(&format!("-{stem}"))
                .tempfile()
                .unwrap_or_else(|e| {
                    eprintln!("zrun: cannot create temporary file: {e}");
                    std::process::exit(1);
                });
            let spec = decompressor(ext);
            let output = Command::new(spec[0])
                .args(&spec[1..])
                .arg(&arg)
                .stdout(Stdio::piped())
                .output()
                .unwrap_or_else(|e| {
                    eprintln!("zrun: preprocessing for {arg} failed: {e}");
                    std::process::exit(1);
                });
            if !output.status.success() {
                eprintln!(
                    "zrun: preprocessing for {arg} terminated with code {}",
                    output.status.code().unwrap_or(1)
                );
                std::process::exit(255);
            }
            std::fs::write(tmp.path(), output.stdout).unwrap_or_else(|e| {
                eprintln!("zrun: cannot write temporary file: {e}");
                std::process::exit(1);
            });
            final_args.push(tmp.path().as_os_str().to_os_string());
            temporaries.push(tmp);
        } else {
            final_args.push(arg.into());
        }
    }

    let status = Command::new(&program)
        .args(&final_args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("zrun: {program}: {e}");
            std::process::exit(1);
        });
    drop(temporaries);
    exit_with_status(status);
}
