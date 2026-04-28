// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::{shell_command, status_code};
use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Stdio};

struct PipeChild {
    command: String,
    child: Child,
    stdin: Option<ChildStdin>,
    inactive: bool,
}

fn main() {
    let mut ignore_write_errors = true;
    let mut commands = Vec::new();
    let mut parsing_options = true;
    for arg in env::args().skip(1) {
        if parsing_options {
            match arg.as_str() {
                "--ignore-sigpipe" | "--no-ignore-sigpipe" => continue,
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

    let mut children = Vec::new();
    for command in commands {
        let mut child = shell_command(&command)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|_| {
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
                    eprintln!("Write error to `{}`", c.command);
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
            Ok(status) => ret |= status_code(status),
            Err(_) => ret |= 1,
        }
    }
    std::process::exit(ret);
}
