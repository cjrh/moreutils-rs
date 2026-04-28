// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::exit_with_status;
use std::env;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const EX_TEMPFAIL: i32 = 75;
const EX_CANTCREAT: i32 = 73;
const EX_OSERR: i32 = 71;

fn main() {
    let mut wait = false;
    let mut timeout: Option<f64> = None;
    let mut exec_direct = false;
    let mut no_create = false;
    let mut quiet = false;
    let mut shared = false;
    let mut test_only = false;
    let mut rest = Vec::new();
    let mut it = env::args().skip(1).peekable();
    while let Some(arg) = it.peek().cloned() {
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        let arg = it.next().unwrap();
        match arg.as_str() {
            "-w" => wait = true,
            "-W" => {
                wait = true;
                timeout = it.next().and_then(|s| s.parse().ok());
            }
            "-e" => exec_direct = true,
            "-E" => {
                exec_direct = true;
                let _ = it.next();
            }
            "-n" => no_create = true,
            "-q" => quiet = true,
            "-s" => shared = true,
            "-x" => shared = false,
            "-t" => test_only = true,
            _ => {
                eprintln!("Usage: lckdo [options] lockfile program [arguments]");
                std::process::exit(1);
            }
        }
    }
    rest.extend(it);
    if rest.is_empty() || (!test_only && rest.len() < 2) {
        eprintln!("Usage: lckdo [options] lockfile program [arguments]");
        std::process::exit(1);
    }
    let lockfile = &rest[0];
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).mode(0o666);
    if !no_create {
        opts.create(true);
    }
    let file = opts.open(lockfile).unwrap_or_else(|e| {
        if !quiet {
            eprintln!("lckdo: {lockfile}: {e}");
        }
        std::process::exit(EX_CANTCREAT);
    });
    let fd = file.as_raw_fd();
    let op_base = if shared { libc::LOCK_SH } else { libc::LOCK_EX };
    let start = Instant::now();
    loop {
        let op = op_base | if wait { libc::LOCK_NB } else { libc::LOCK_NB };
        let rc = unsafe { libc::flock(fd, op) };
        if rc == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) && err.raw_os_error() != Some(libc::EAGAIN)
        {
            if !quiet {
                eprintln!("lckdo: lock: {err}");
            }
            std::process::exit(EX_OSERR);
        }
        if !wait || timeout.is_some_and(|t| start.elapsed().as_secs_f64() >= t) {
            if !quiet {
                eprintln!("lckdo: {lockfile}: lock already held");
            }
            std::process::exit(EX_TEMPFAIL);
        }
        thread::sleep(Duration::from_millis(100));
    }
    if test_only {
        return;
    }

    if exec_direct {
        let err = Command::new(&rest[1]).args(&rest[2..]).exec();
        eprintln!("lckdo: {}: {err}", rest[1]);
        std::process::exit(1);
    } else {
        let status = Command::new(&rest[1])
            .args(&rest[2..])
            .status()
            .unwrap_or_else(|e| {
                eprintln!("lckdo: {}: {e}", rest[1]);
                std::process::exit(1);
            });
        exit_with_status(status);
    }
}
