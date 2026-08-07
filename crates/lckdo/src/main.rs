// SPDX-License-Identifier: GPL-2.0-only

use std::env;
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const EX_USAGE: i32 = 64;
const EX_SOFTWARE: i32 = 70;
const EX_OSERR: i32 = 71;
const EX_CANTCREAT: i32 = 73;
const EX_TEMPFAIL: i32 = 75;

#[derive(Debug, Default)]
struct Config {
    wait: bool,
    timeout: Option<u64>,
    exec_direct: bool,
    keep_fd: Option<RawFd>,
    no_create: bool,
    quiet: bool,
    shared: bool,
    test_only: bool,
}

fn main() {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.is_empty() {
        print_usage();
        return;
    }

    let (config, rest) = parse_args(&raw_args);
    if rest.is_empty() || (!config.test_only && rest.len() < 2) {
        eprintln!("lckdo: too few arguments given");
        std::process::exit(EX_USAGE);
    }

    let lockfile = &rest[0];
    if config.test_only {
        test_lock(lockfile, &config);
        return;
    }

    let mut fd = open_lockfile(lockfile, &config).unwrap_or_else(|err| open_error(lockfile, err));
    if let Some(keep_fd) = config.keep_fd {
        // Match lckdo's historical behaviour: the descriptor is duplicated
        // before locking, then the original descriptor is closed.  If the
        // requested descriptor is already the opened descriptor (commonly 3),
        // this closes it and the later lock attempt fails with EBADF.
        if unsafe { libc::dup2(fd, keep_fd) } < 0 {
            let err = io::Error::last_os_error();
            eprintln!(
                "lckdo: unable to lock `{lockfile}': {}",
                error_description(&err)
            );
            std::process::exit(EX_OSERR);
        }
        unsafe {
            libc::close(fd);
        }
        fd = keep_fd;
    }

    match acquire_lock(fd, &config) {
        Ok(()) => {}
        Err(AcquireError::AlreadyLocked { timed_out }) => {
            if !config.quiet {
                if timed_out {
                    eprintln!("lckdo: lock file `{lockfile}' is already locked (timeout waiting)");
                } else {
                    eprintln!("lckdo: lockfile `{lockfile}' is already locked");
                }
            }
            std::process::exit(EX_TEMPFAIL);
        }
        Err(AcquireError::Other(err)) => {
            eprintln!(
                "lckdo: unable to lock `{lockfile}': {}",
                error_description(&err)
            );
            std::process::exit(EX_OSERR);
        }
    }

    if config.exec_direct {
        clear_cloexec(fd);
        exec_program(&rest[1], &rest[2..]);
    } else {
        let status = run_program(&rest[1], &rest[2..]);
        exit_like_lckdo(&rest[1], status);
    }
}

fn parse_args(args: &[String]) -> (Config, Vec<String>) {
    let mut config = Config::default();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            index += 1;
            break;
        }
        if arg == "-" || !arg.starts_with('-') {
            break;
        }

        let chars: Vec<char> = arg.chars().collect();
        let mut pos = 1;
        while pos < chars.len() {
            match chars[pos] {
                'w' => config.wait = true,
                'e' => config.exec_direct = true,
                'n' => config.no_create = true,
                'q' => config.quiet = true,
                's' => config.shared = true,
                'x' => config.shared = false,
                't' => {
                    config.test_only = true;
                    config.no_create = true;
                }
                'W' => {
                    let value = if pos + 1 < chars.len() {
                        chars[pos + 1..].iter().collect::<String>()
                    } else {
                        index += 1;
                        if index >= args.len() {
                            eprintln!("lckdo: option requires an argument -- 'W'");
                            std::process::exit(EX_USAGE);
                        }
                        args[index].clone()
                    };
                    config.wait = true;
                    config.timeout = Some(parse_wait_time(&value));
                    pos = chars.len();
                    continue;
                }
                'E' => {
                    let value = if pos + 1 < chars.len() {
                        chars[pos + 1..].iter().collect::<String>()
                    } else {
                        index += 1;
                        if index >= args.len() {
                            eprintln!("lckdo: option requires an argument -- 'E'");
                            std::process::exit(EX_USAGE);
                        }
                        args[index].clone()
                    };
                    config.exec_direct = true;
                    config.keep_fd = Some(parse_keep_fd(&value));
                    pos = chars.len();
                    continue;
                }
                ch => {
                    eprintln!("lckdo: invalid option -- '{ch}'");
                    std::process::exit(EX_USAGE);
                }
            }
            pos += 1;
        }
        index += 1;
    }

    (config, args[index..].to_vec())
}

fn parse_wait_time(value: &str) -> u64 {
    let mut saw_digit = false;
    let mut parsed = 0_u64;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            break;
        }
        saw_digit = true;
        parsed = parsed
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(byte - b'0')))
            .unwrap_or_else(|| invalid_wait_time(value));
        if parsed > i64::MAX as u64 {
            invalid_wait_time(value);
        }
    }
    if !saw_digit || parsed == 0 {
        invalid_wait_time(value);
    }
    parsed
}

fn invalid_wait_time(value: &str) -> ! {
    eprintln!("lckdo: invalid wait time `{value}'");
    std::process::exit(EX_USAGE);
}

fn parse_keep_fd(value: &str) -> RawFd {
    let mut parsed = 0_i64;
    let mut saw_digit = false;
    let mut negative = false;
    let bytes = value.as_bytes();
    let mut start = 0;
    if bytes.first() == Some(&b'-') {
        negative = true;
        start = 1;
    }
    for &byte in &bytes[start..] {
        if !byte.is_ascii_digit() {
            break;
        }
        saw_digit = true;
        parsed = parsed
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(byte - b'0')))
            .unwrap_or_else(|| invalid_fd(value));
        if parsed > i32::MAX as i64 {
            invalid_fd(value);
        }
    }

    let fd = if saw_digit && !negative {
        parsed as RawFd
    } else {
        0
    };
    if negative || fd == libc::STDERR_FILENO {
        invalid_fd(value);
    }
    fd
}

fn invalid_fd(value: &str) -> ! {
    eprintln!("lckdo: invalid fd# `{value}'");
    std::process::exit(EX_USAGE);
}

fn print_usage() {
    println!("lckdo: execute a program with a lock set.");
    println!("Usage: lckdo [options] lockfile program [arguments]");
    println!("where options are:");
    println!(" -w - if the lock is already held by another process,");
    println!("   wait for it to complete instead of failing immediately");
    println!(" -W sec - the same as -w but wait not more than sec seconds");
    println!(" -e - execute the program directly, no fork/wait");
    println!("   (keeps extra open file descriptor)");
    println!(" -E nnn - set the fd# to keep open in -e case (implies -e)");
    println!(" -n - do not create the lock file if it does not exist");
    println!(" -q - produce no output if lock is already held");
    println!(" -s - lock in shared (read) mode");
    println!(" -x - lock in exclusive (write) mode (default)");
    println!(" -t - test for lock existence (just prints pid if any with -q)");
    println!("   (implies -n)");
}

fn open_lockfile(lockfile: &str, config: &Config) -> io::Result<RawFd> {
    let path = CString::new(lockfile).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file name contained an interior NUL byte",
        )
    })?;
    let mut flags = if config.test_only || config.shared {
        libc::O_RDONLY
    } else {
        libc::O_WRONLY
    };
    if !config.no_create && !config.test_only {
        flags |= libc::O_CREAT;
    }
    let fd = unsafe { libc::open(path.as_ptr(), flags, 0o666) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

fn open_error(lockfile: &str, err: io::Error) -> ! {
    eprintln!(
        "lckdo: unable to open `{lockfile}': {}",
        error_description(&err)
    );
    std::process::exit(EX_CANTCREAT);
}

fn flock_for(lock_type: libc::c_short) -> libc::flock {
    libc::flock {
        l_type: lock_type,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    }
}

#[derive(Debug)]
enum AcquireError {
    AlreadyLocked { timed_out: bool },
    Other(io::Error),
}

fn acquire_lock(fd: RawFd, config: &Config) -> Result<(), AcquireError> {
    let lock_type = if config.shared {
        libc::F_RDLCK as libc::c_short
    } else {
        libc::F_WRLCK as libc::c_short
    };

    if config.wait && config.timeout.is_none() {
        let mut lock = flock_for(lock_type);
        loop {
            if unsafe { libc::fcntl(fd, libc::F_SETLKW, &mut lock) } == 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINTR) {
                return Err(AcquireError::Other(err));
            }
        }
    }

    let start = Instant::now();
    loop {
        let mut lock = flock_for(lock_type);
        if unsafe { libc::fcntl(fd, libc::F_SETLK, &mut lock) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if !is_lock_contention(&err) {
            return Err(AcquireError::Other(err));
        }
        if !config.wait {
            return Err(AcquireError::AlreadyLocked { timed_out: false });
        }
        if let Some(timeout) = config.timeout {
            if start.elapsed() >= Duration::from_secs(timeout) {
                return Err(AcquireError::AlreadyLocked { timed_out: true });
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn is_lock_contention(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(code) if code == libc::EACCES || code == libc::EAGAIN || code == libc::EWOULDBLOCK
    )
}

fn test_lock(lockfile: &str, config: &Config) {
    let fd = match open_lockfile(lockfile, config) {
        Ok(fd) => fd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            print_not_locked(lockfile, config.quiet);
            std::process::exit(0);
        }
        Err(err) => open_error(lockfile, err),
    };

    let lock_type = if config.shared {
        libc::F_RDLCK as libc::c_short
    } else {
        libc::F_WRLCK as libc::c_short
    };
    let mut lock = flock_for(lock_type);
    if unsafe { libc::fcntl(fd, libc::F_GETLK, &mut lock) } != 0 {
        let err = io::Error::last_os_error();
        eprintln!(
            "lckdo: unable to lock `{lockfile}': {}",
            error_description(&err)
        );
        std::process::exit(EX_OSERR);
    }

    if lock.l_type == libc::F_UNLCK as libc::c_short {
        print_not_locked(lockfile, config.quiet);
        std::process::exit(0);
    }

    if config.quiet {
        println!("{}", lock.l_pid);
    } else {
        println!("lockfile `{lockfile}' is locked by process {}", lock.l_pid);
    }
    std::process::exit(EX_TEMPFAIL);
}

fn print_not_locked(lockfile: &str, quiet: bool) {
    if !quiet {
        println!("lockfile `{lockfile}' is not locked");
    }
}

fn run_program(program: &str, args: &[String]) -> ExitStatus {
    let mut command = Command::new(program);
    command.args(args);
    match command.status() {
        Ok(status) => status,
        Err(err) if err.raw_os_error() == Some(libc::ENOEXEC) => {
            run_script_without_shebang(program, args)
                .unwrap_or_else(|fallback_err| exec_error(program, fallback_err))
        }
        Err(err) => exec_error(program, err),
    }
}

fn run_script_without_shebang(program: &str, args: &[String]) -> io::Result<ExitStatus> {
    let script = if program.contains('/') {
        PathBuf::from(program)
    } else {
        resolve_in_path(program).unwrap_or_else(|| PathBuf::from(program))
    };
    Command::new("/bin/sh").arg(script).args(args).status()
}

fn exec_program(program: &str, args: &[String]) -> ! {
    let err = Command::new(program).args(args).exec();
    if err.raw_os_error() == Some(libc::ENOEXEC) {
        let script = if program.contains('/') {
            PathBuf::from(program)
        } else {
            resolve_in_path(program).unwrap_or_else(|| PathBuf::from(program))
        };
        let err = Command::new("/bin/sh").arg(script).args(args).exec();
        exec_error(program, err);
    }
    exec_error(program, err);
}

fn resolve_in_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(program);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn exec_error(program: &str, err: io::Error) -> ! {
    eprintln!(
        "lckdo: unable to execute {program}: {}",
        error_description(&err)
    );
    std::process::exit(EX_OSERR);
}

fn exit_like_lckdo(program: &str, status: ExitStatus) -> ! {
    if let Some(signal) = status.signal() {
        eprintln!("lckdo: {program}: {}", signal_description(signal));
        std::process::exit(EX_SOFTWARE);
    }
    std::process::exit(status.code().unwrap_or(0));
}

fn signal_description(signal: i32) -> String {
    let ptr = unsafe { libc::strsignal(signal) };
    if ptr.is_null() {
        format!("signal {signal}")
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

fn error_description(err: &io::Error) -> String {
    if let Some(code) = err.raw_os_error() {
        let ptr = unsafe { libc::strerror(code) };
        if !ptr.is_null() {
            return unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
        }
    }
    err.to_string()
}

fn clear_cloexec(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags >= 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
    }
}
