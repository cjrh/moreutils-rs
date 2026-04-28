// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read};

#[derive(Default)]
struct Opts {
    quiet: bool,
    list: bool,
    invert: bool,
    verbose: bool,
}

struct InvalidUtf8 {
    pos: usize,
    message: &'static str,
}

fn help(program: &str) {
    println!(
        "Usage: {program} [OPTION]... [FILE]...\nCheck whether input files are valid UTF-8.\n\n  -h, --help       display this help text and exit\n  -q, --quiet      suppress all normal output\n  -l, --list       print only names of FILEs containing invalid UTF-8\n  -i, --invert     list valid UTF-8 files instead of invalid ones\n  -v, --verbose    print detailed error (multiple lines)\n\nThis is version 1.2."
    );
}

fn parse_args(program: &str) -> (Opts, Vec<String>) {
    let mut opts = Opts::default();
    let mut files = Vec::new();
    let mut options_enabled = true;

    for arg in env::args().skip(1) {
        if options_enabled && arg == "--" {
            options_enabled = false;
            continue;
        }

        if options_enabled && arg.starts_with("--") && arg.len() > 2 {
            parse_long_option(program, &arg, &mut opts);
            continue;
        }

        if options_enabled && arg.starts_with('-') && arg != "-" {
            for ch in arg.chars().skip(1) {
                match ch {
                    'h' => {
                        help(program);
                        std::process::exit(0);
                    }
                    'q' => opts.quiet = true,
                    'l' => opts.list = true,
                    'i' => opts.invert = true,
                    'v' => opts.verbose = true,
                    _ => {
                        eprintln!("{program}: invalid option -- '{ch}'");
                        help(program);
                        std::process::exit(1);
                    }
                }
            }
            continue;
        }

        files.push(arg);
    }

    (opts, files)
}

fn parse_long_option(program: &str, arg: &str, opts: &mut Opts) {
    let option = &arg[2..];
    let (name, has_argument) = option
        .split_once('=')
        .map(|(name, _)| (name, true))
        .unwrap_or((option, false));

    let matches: Vec<(&str, char)> = [
        ("help", 'h'),
        ("quiet", 'q'),
        ("list-only", 'l'),
        ("invert", 'i'),
        ("verbose", 'v'),
    ]
    .into_iter()
    .filter(|(long, _)| long.starts_with(name))
    .collect();

    if matches.len() != 1 {
        eprintln!("{program}: unrecognized option '{arg}'");
        help(program);
        std::process::exit(1);
    }

    let (canonical, short) = matches[0];
    if has_argument {
        eprintln!("{program}: option '--{canonical}' doesn't allow an argument");
        help(program);
        std::process::exit(1);
    }

    match short {
        'h' => {
            help(program);
            std::process::exit(0);
        }
        'q' => opts.quiet = true,
        'l' => opts.list = true,
        'i' => opts.invert = true,
        'v' => opts.verbose = true,
        _ => unreachable!(),
    }
}

fn validate_utf8(bytes: &[u8]) -> Result<(), InvalidUtf8> {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0x00..=0x7f => i += 1,
            0xc2..=0xdf => {
                if i + 1 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between C2 and DF, expecting a 2nd byte.",
                    });
                }
                if !is_cont(bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between C2 and DF, expecting a 2nd byte between 80 and BF",
                    });
                }
                i += 2;
            }
            0xe0 => {
                if i + 2 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of E0, expecting two following bytes.",
                    });
                }
                if !(0xa0..=0xbf).contains(&bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of E0, expecting a 2nd byte between A0 and BF.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of E0, expecting a 3nd byte between 80 and BF.",
                    });
                }
                i += 3;
            }
            0xe1..=0xec => {
                if i + 2 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between E1 and EC, expecting two following bytes.",
                    });
                }
                if !is_cont(bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between E1 and EC, expecting the 2nd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between E1 and EC, expecting the 3rd byte between 80 and BF.",
                    });
                }
                i += 3;
            }
            0xed => {
                if i + 2 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of ED, expecting two following bytes.",
                    });
                }
                if !(0x80..=0x9f).contains(&bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of ED, expecting 2nd byte between 80 and 9F.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of ED, expecting 3rd byte between 80 and BF.",
                    });
                }
                i += 3;
            }
            0xee..=0xef => {
                if i + 2 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between EE and EF, two following bytes.",
                    });
                }
                if !is_cont(bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between EE and EF, expecting 2nd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte between EE and EF, expecting 3rd byte between 80 and BF.",
                    });
                }
                i += 3;
            }
            0xf0 => {
                if i + 3 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F0, expecting three following bytes.",
                    });
                }
                if !(0x90..=0xbf).contains(&bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F0, expecting 2nd byte between 90 and BF.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F0, expecting 3rd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 3]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F0, expecting 4th byte between 80 and BF.",
                    });
                }
                i += 4;
            }
            0xf1..=0xf3 => {
                if i + 3 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F1, F2, or F3, expecting three following bytes.",
                    });
                }
                if !is_cont(bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F1, F2, or F3, expecting a 2nd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F1, F2, or F3, expecting a 3rd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 3]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F1, F2, or F3, expecting a 4th byte between 80 and BF.",
                    });
                }
                i += 4;
            }
            0xf4 => {
                if i + 3 >= bytes.len() {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F4, expecting three following bytes.",
                    });
                }
                if !(0x80..=0x8f).contains(&bytes[i + 1]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F4, expecting 2nd byte between 80 and 8F.",
                    });
                }
                if !is_cont(bytes[i + 2]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F4, expecting 3rd byte between 80 and BF.",
                    });
                }
                if !is_cont(bytes[i + 3]) {
                    return Err(InvalidUtf8 {
                        pos: i,
                        message: "After a first byte of F4, expecting 4th byte between 80 and BF.",
                    });
                }
                i += 4;
            }
            _ => {
                return Err(InvalidUtf8 {
                    pos: i,
                    message: "Expecting bytes in the following ranges: 00..7F C2..F4.",
                });
            }
        }
    }
    Ok(())
}

fn is_cont(byte: u8) -> bool {
    (0x80..=0xbf).contains(&byte)
}

fn line_char(bytes: &[u8], pos: usize, stdin: bool) -> (usize, usize, usize) {
    let before = &bytes[..pos.min(bytes.len())];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let line_start = before
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let mut ch = pos - line_start;
    if !stdin {
        ch += 1;
    }
    (line, ch, pos)
}

fn print_verbose(bytes: &[u8], pos: usize) {
    let start = pos.saturating_sub(8);
    let end = (start + 16).min(bytes.len());
    let slice = &bytes[start..end];
    let caret = pos - start;

    for b in slice {
        print!("{b:02X} ");
    }
    for _ in 0..(16usize.saturating_sub(slice.len()) * 3) {
        print!(" ");
    }
    print!(" | ");
    for &b in slice {
        let c = if (0x20..=0x7e).contains(&b) {
            b as char
        } else {
            '.'
        };
        print!("{c}");
    }
    println!();

    for i in 0..16 {
        if i == caret {
            print!("^^ ");
        } else {
            print!("   ");
        }
    }
    print!(" | ");
    for _ in 0..caret {
        print!(" ");
    }
    println!("^\n");
}

fn check(name: &str, bytes: &[u8], opts: &Opts, stdin: bool) -> bool {
    match validate_utf8(bytes) {
        Ok(()) => {
            if opts.invert && !opts.quiet && !stdin && !bytes.is_empty() {
                println!("{name}");
            }
            true
        }
        Err(err) => {
            if !opts.quiet && !opts.invert {
                if opts.list {
                    println!("{name}");
                } else {
                    let (line, ch, byte) = line_char(bytes, err.pos, stdin);
                    println!(
                        "{name}: line {line}, char {ch}, byte {byte}: {}",
                        err.message
                    );
                    if opts.verbose {
                        print_verbose(bytes, err.pos);
                    }
                }
            }
            false
        }
    }
}

fn main() {
    let program = env::args().next().unwrap_or_else(|| "isutf8".to_string());
    let (opts, files) = parse_args(&program);

    let mut all_ok = true;
    if files.is_empty() {
        let mut bytes = Vec::new();
        if io::stdin().read_to_end(&mut bytes).is_err() {
            std::process::exit(1);
        }
        all_ok &= check("(standard input)", &bytes, &opts, true);
    } else {
        for file in files {
            match read_file_for_checking(&file) {
                Ok(bytes) => all_ok &= check(&file, &bytes, &opts, false),
                Err(e) => {
                    print_os_error(e.prefix, &e.error);
                    all_ok = false;
                }
            }
        }
    }
    std::process::exit(if all_ok { 0 } else { 1 });
}

struct ReadError {
    prefix: &'static str,
    error: io::Error,
}

fn read_file_for_checking(path: &str) -> Result<Vec<u8>, ReadError> {
    let mut file = File::open(path).map_err(|error| ReadError {
        prefix: "open",
        error,
    })?;
    let metadata = file.metadata().map_err(|error| ReadError {
        prefix: "fstat",
        error,
    })?;
    if metadata.is_dir() {
        return Ok(Vec::new());
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| ReadError {
        prefix: "open",
        error,
    })?;
    Ok(bytes)
}

fn print_os_error(prefix: &str, err: &io::Error) {
    eprintln!("{prefix}: {}", os_error_message(err));
}

fn os_error_message(err: &io::Error) -> String {
    if let Some(errno) = err.raw_os_error() {
        unsafe {
            let message = libc::strerror(errno);
            if !message.is_null() {
                return CStr::from_ptr(message).to_string_lossy().into_owned();
            }
        }
    }
    err.to_string()
}
