// SPDX-License-Identifier: GPL-2.0-only

use std::collections::HashMap;
use std::env;
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

fn read_lines(path: &str) -> io::Result<Vec<Vec<u8>>> {
    let mut input: Box<dyn BufRead> = if path == "-" {
        Box::new(BufReader::new(io::stdin().lock()))
    } else {
        Box::new(BufReader::new(File::open(path)?))
    };

    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        let bytes = input.read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        lines.push(line);
    }
    Ok(lines)
}

fn read_lines_or_die(path: &str) -> Vec<Vec<u8>> {
    read_lines(path).unwrap_or_else(|err| {
        let code = err.raw_os_error().unwrap_or(2);
        eprintln!("{path}: {}", os_error_message(&err));
        std::process::exit(code);
    })
}

fn os_error_message(err: &io::Error) -> String {
    if let Some(code) = err.raw_os_error() {
        // SAFETY: strerror returns a pointer to a NUL-terminated static buffer
        // for a valid OS errno value.
        unsafe {
            return CStr::from_ptr(libc::strerror(code))
                .to_string_lossy()
                .into_owned();
        }
    }
    err.to_string()
}

fn counts(lines: &[Vec<u8>]) -> HashMap<Vec<u8>, usize> {
    let mut m = HashMap::new();
    for line in lines {
        *m.entry(line.clone()).or_insert(0) += 1;
    }
    m
}

fn write_line<W: Write>(out: &mut W, line: &[u8]) -> io::Result<()> {
    out.write_all(line)?;
    out.write_all(b"\n")
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.len() >= 4 && args[3] == "_" {
        args.remove(3);
    }
    if args.len() != 3 {
        eprintln!("Usage: combine file1 OP file2");
        std::process::exit(255);
    }

    let file1 = &args[0];
    let op = args[1].to_ascii_lowercase();
    let file2 = &args[2];
    let mut out = io::BufWriter::new(io::stdout().lock());

    match op.as_str() {
        "and" => {
            let lines2 = read_lines_or_die(file2);
            let seen2 = counts(&lines2);
            let lines1 = read_lines_or_die(file1);
            for line in &lines1 {
                if seen2.contains_key(line.as_slice()) {
                    write_line(&mut out, line).unwrap();
                }
            }
        }
        "not" => {
            let lines2 = read_lines_or_die(file2);
            let seen2 = counts(&lines2);
            let lines1 = read_lines_or_die(file1);
            for line in &lines1 {
                if !seen2.contains_key(line.as_slice()) {
                    write_line(&mut out, line).unwrap();
                }
            }
        }
        "or" => {
            let lines1 = read_lines_or_die(file1);
            for line in &lines1 {
                write_line(&mut out, line).unwrap();
            }
            out.flush().unwrap();
            let lines2 = read_lines_or_die(file2);
            for line in &lines2 {
                write_line(&mut out, line).unwrap();
            }
        }
        "xor" => {
            let lines2 = read_lines_or_die(file2);
            let mut state: HashMap<Vec<u8>, bool> = HashMap::new();
            for line in &lines2 {
                state.insert(line.clone(), true);
            }

            let lines1 = read_lines_or_die(file1);
            for line in &lines1 {
                if state.contains_key(line.as_slice()) {
                    state.insert(line.clone(), false);
                } else {
                    write_line(&mut out, line).unwrap();
                }
            }

            for line in &lines2 {
                if state.get(line.as_slice()) == Some(&true) {
                    write_line(&mut out, line).unwrap();
                }
            }
        }
        _ => {
            eprintln!("unknown operation, {op}");
            std::process::exit(255);
        }
    }
}
