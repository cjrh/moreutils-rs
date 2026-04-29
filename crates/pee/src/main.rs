// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::shell_command;
use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

struct PipeChild {
    command: String,
    child: Child,
    stdin: Option<ChildStdin>,
    inactive: bool,
}

fn main() {
    let mut ignore_sigpipe = true;
    let mut ignore_write_errors = true;
    let mut commands = Vec::new();
    let mut parsing_options = true;
    for arg in env::args().skip(1) {
        if parsing_options {
            match arg.as_str() {
                "--ignore-sigpipe" => {
                    ignore_sigpipe = true;
                    continue;
                }
                "--no-ignore-sigpipe" => {
                    ignore_sigpipe = false;
                    continue;
                }
                "--ignore-write-errors" => {
                    ignore_write_errors = true;
                    continue;
                }
                "--no-ignore-write-errors" => {
                    ignore_write_errors = false;
                    continue;
                }
                _ => parsing_options = false,
            }
        }
        commands.push(arg);
    }

    set_sigpipe(ignore_sigpipe);

    let mut children = Vec::new();
    for command in commands {
        let mut cmd = shell_command(&command);
        set_child_sigpipe(&mut cmd, ignore_sigpipe);
        let mut child = cmd.stdin(Stdio::piped()).spawn().unwrap_or_else(|_| {
            eprintln!("Can not open pipe to '{command}'");
            std::process::exit(1);
        });
        let stdin = child.stdin.take();
        children.push(PipeChild {
            command,
            child,
            stdin,
            inactive: false,
        });
    }

    let mut buf = [0u8; 8192];
    loop {
        let n = match io::stdin().read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let total_children = children.len();
        let mut inactive_count = children.iter().filter(|c| c.inactive).count();
        for c in &mut children {
            if c.inactive {
                continue;
            }
            let ok = c
                .stdin
                .as_mut()
                .is_some_and(|w| w.write_all(&buf[..n]).is_ok());
            if !ok {
                inactive_count += 1;
                if !ignore_write_errors {
                    eprintln!("Write error to `{}'", c.command);
                }
                if !ignore_write_errors || inactive_count == total_children {
                    std::process::exit(1);
                }
                c.inactive = true;
                c.stdin.take();
            }
        }
    }

    let mut ret = 0;
    for mut c in children {
        drop(c.stdin.take());
        match c.child.wait() {
            Ok(status) => ret |= pee_status_code(status),
            Err(_) => ret |= 1,
        }
    }
    std::process::exit(ret);
}

fn pee_status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else {
        1
    }
}

#[cfg(unix)]
fn set_sigpipe(ignore: bool) {
    let handler = if ignore { libc::SIG_IGN } else { libc::SIG_DFL };
    unsafe {
        libc::signal(libc::SIGPIPE, handler);
    }
}

#[cfg(not(unix))]
fn set_sigpipe(_ignore: bool) {}

#[cfg(unix)]
fn set_child_sigpipe(command: &mut std::process::Command, ignore: bool) {
    unsafe {
        command.pre_exec(move || {
            let handler = if ignore { libc::SIG_IGN } else { libc::SIG_DFL };
            libc::signal(libc::SIGPIPE, handler);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn set_child_sigpipe(_command: &mut std::process::Command, _ignore: bool) {}
