// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::fs;
use std::io::{self, Read};

#[derive(Default)]
struct Opts {
    quiet: bool,
    list: bool,
    invert: bool,
    verbose: bool,
}

fn help() {
    println!(
        "Usage: isutf8 [-hqliv] [--help] [--quiet] [--list] [--invert] [--verbose] [file ...]"
    );
}

fn line_char(bytes: &[u8], pos: usize) -> (usize, usize, usize) {
    let before = &bytes[..pos.min(bytes.len())];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let line_start = before
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let ch = std::str::from_utf8(&bytes[line_start..pos])
        .map(|s| s.chars().count() + 1)
        .unwrap_or(pos - line_start + 1);
    (line, ch, pos)
}

fn print_verbose(bytes: &[u8], pos: usize) {
    let start = pos.saturating_sub(16);
    let end = (pos + 16).min(bytes.len());
    let slice = &bytes[start..end];
    for b in slice {
        print!("{b:02X} ");
    }
    let pad = 3 * (16usize.saturating_sub(slice.len()));
    for _ in 0..pad {
        print!(" ");
    }
    print!(" | ");
    for &b in slice {
        let c = if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '.'
        };
        print!("{c}");
    }
    println!();
    for i in start..end {
        if i == pos {
            print!("^^ ");
        } else {
            print!("   ");
        }
    }
    print!(" | ");
    for i in start..end {
        if i == pos {
            print!("^");
        } else {
            print!(" ");
        }
    }
    println!();
}

fn check(name: &str, bytes: &[u8], opts: &Opts) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => {
            if opts.invert && opts.list && !opts.quiet {
                println!("{name}");
            }
            true
        }
        Err(e) => {
            if !opts.invert && !opts.quiet {
                if opts.list {
                    println!("{name}");
                } else {
                    let pos = e.valid_up_to();
                    let (line, ch, byte) = line_char(bytes, pos);
                    println!("{name}: line {line}, char {ch}, byte {byte}: invalid UTF-8");
                    if opts.verbose {
                        print_verbose(bytes, pos);
                    }
                }
            }
            false
        }
    }
}

fn main() {
    let mut opts = Opts::default();
    let mut files = Vec::new();
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--help" => {
                help();
                return;
            }
            "--quiet" => opts.quiet = true,
            "--list" => opts.list = true,
            "--invert" => opts.invert = true,
            "--verbose" => opts.verbose = true,
            _ if arg.starts_with('-') && arg != "-" => {
                for ch in arg.chars().skip(1) {
                    match ch {
                        'h' => {
                            help();
                            return;
                        }
                        'q' => opts.quiet = true,
                        'l' => opts.list = true,
                        'i' => opts.invert = true,
                        'v' => opts.verbose = true,
                        _ => {
                            help();
                            std::process::exit(1);
                        }
                    }
                }
            }
            _ => files.push(arg),
        }
    }

    let mut all_ok = true;
    if files.is_empty() {
        let mut bytes = Vec::new();
        if io::stdin().read_to_end(&mut bytes).is_err() {
            std::process::exit(1);
        }
        all_ok &= check("stdin", &bytes, &opts);
    } else {
        for file in files {
            match fs::read(&file) {
                Ok(bytes) => all_ok &= check(&file, &bytes, &opts),
                Err(e) => {
                    if !opts.quiet {
                        eprintln!("{file}: {e}");
                    }
                    all_ok = false;
                }
            }
        }
    }
    std::process::exit(if all_ok { 0 } else { 1 });
}
