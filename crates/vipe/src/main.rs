// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::io::{self, Read, Write};
use std::process::Command;
use tempfile::NamedTempFile;

fn editor() -> String {
    env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into())
}

fn main() {
    let mut tmp = NamedTempFile::new().unwrap_or_else(|e| {
        eprintln!("vipe: {e}");
        std::process::exit(1);
    });
    let mut input = Vec::new();
    if io::stdin().read_to_end(&mut input).is_err() {
        std::process::exit(1);
    }
    if tmp.write_all(&input).is_err() || tmp.flush().is_err() {
        std::process::exit(1);
    }
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", editor()))
        .arg("vipe")
        .arg(tmp.path())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("vipe: editor: {e}");
            std::process::exit(1);
        });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    let out = std::fs::read(tmp.path()).unwrap_or_else(|e| {
        eprintln!("vipe: {e}");
        std::process::exit(1);
    });
    if io::stdout().write_all(&out).is_err() {
        std::process::exit(1);
    }
}
