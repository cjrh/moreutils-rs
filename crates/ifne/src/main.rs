// SPDX-License-Identifier: GPL-2.0-only

use std::env;
use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || (args.len() == 1 && args[0] == "-n") {
        eprintln!("Usage: ifne [-n] command [args]");
        std::process::exit(1);
    }
    let run_if_empty = args.first().is_some_and(|a| a == "-n");
    if run_if_empty {
        args.remove(0);
    }

    let mut input = Vec::new();
    if let Err(e) = io::stdin().read_to_end(&mut input) {
        print_os_error("read", &e);
        std::process::exit(1);
    }

    if input.is_empty() && !run_if_empty {
        return;
    }
    if !input.is_empty() && run_if_empty {
        write_stdout_or_die(&input);
        return;
    }

    let mut child = spawn_child(&args);
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(&input) {
            if e.kind() == io::ErrorKind::BrokenPipe {
                terminate_by_signal(libc::SIGPIPE);
            }
            print_os_error("Write error", &e);
            std::process::exit(1);
        }
    }
    let status = child.wait().unwrap_or_else(|e| {
        print_os_error("waitpid", &e);
        std::process::exit(1);
    });
    exit_with_child_status(status);
}

fn write_stdout_or_die(bytes: &[u8]) {
    if let Err(e) = io::stdout().lock().write_all(bytes) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            terminate_by_signal(libc::SIGPIPE);
        }
        print_os_error("write", &e);
        std::process::exit(1);
    }
}

fn spawn_child(args: &[String]) -> Child {
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]).stdin(Stdio::piped());
    command.spawn().unwrap_or_else(|e| {
        if e.raw_os_error() == Some(libc::ENOEXEC) {
            return spawn_script_without_shebang(args)
                .unwrap_or_else(|fallback| exec_error(&args[0], fallback));
        }
        exec_error(&args[0], e)
    })
}

fn spawn_script_without_shebang(args: &[String]) -> io::Result<Child> {
    let script = if args[0].contains('/') {
        PathBuf::from(&args[0])
    } else {
        resolve_in_path(&args[0]).unwrap_or_else(|| PathBuf::from(&args[0]))
    };

    Command::new("/bin/sh")
        .arg(script)
        .args(&args[1..])
        .stdin(Stdio::piped())
        .spawn()
}

fn resolve_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    for dir in env::split_paths(&path) {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(command);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn exec_error(command: &str, err: io::Error) -> ! {
    print_os_error(command, &err);
    std::process::exit(1);
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

fn exit_with_child_status(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        terminate_by_signal(signal);
    }
    std::process::exit(1);
}

fn terminate_by_signal(signal: i32) -> ! {
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
    std::process::exit(128 + signal);
}
