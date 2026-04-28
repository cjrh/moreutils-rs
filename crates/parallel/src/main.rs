// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::{shell_command, status_code};
use std::env;
use std::fs;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

fn usage() -> ! {
    println!(
        "parallel [OPTIONS] command -- arguments\n\tfor each argument, run command with argument, in parallel"
    );
    println!("parallel [OPTIONS] -- commands\n\trun specified commands in parallel");
    std::process::exit(1);
}

fn loadavg() -> f64 {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

fn spawn_job(command: &[String], args: &[String], replace: bool) -> Child {
    if command.is_empty() {
        shell_command(&args[0]).spawn().unwrap_or_else(|e| {
            eprintln!("parallel: {}: {e}", args[0]);
            std::process::exit(1);
        })
    } else {
        let mut argv = Vec::new();
        if replace {
            let arg = args.first().map(String::as_str).unwrap_or("");
            for c in command {
                argv.push(c.replace("{}", arg));
            }
        } else {
            argv.extend_from_slice(command);
            argv.extend_from_slice(args);
        }
        Command::new(&argv[0])
            .args(&argv[1..])
            .spawn()
            .unwrap_or_else(|e| {
                eprintln!("parallel: {}: {e}", argv[0]);
                std::process::exit(1);
            })
    }
}

fn reap_one(children: &mut Vec<Child>, block: bool) -> Option<i32> {
    loop {
        for i in 0..children.len() {
            match children[i].try_wait() {
                Ok(Some(status)) => {
                    let mut child = children.remove(i);
                    let _ = child.wait();
                    return Some(status_code(status));
                }
                Ok(None) => {}
                Err(_) => return Some(1),
            }
        }
        if !block {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn main() {
    let mut maxjobs: isize = -1;
    let mut maxload: Option<f64> = None;
    let mut replace = false;
    let mut nargs = 1usize;
    let mut rest = Vec::new();
    let mut it = env::args().skip(1).peekable();
    while let Some(arg) = it.peek().cloned() {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        let arg = it.next().unwrap();
        match arg.as_str() {
            "-h" => usage(),
            "-i" => replace = true,
            "-j" => {
                maxjobs = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    eprintln!("option '-j' is not a number");
                    std::process::exit(2)
                })
            }
            "-l" => {
                maxload = Some(it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    eprintln!("option '-l' is not a number");
                    std::process::exit(2)
                }))
            }
            "-n" => {
                nargs = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|&n| n > 0)
                    .unwrap_or_else(|| {
                        eprintln!("option '-n' is not a positive number");
                        std::process::exit(2)
                    })
            }
            _ => usage(),
        }
    }
    rest.extend(it);
    if replace && nargs > 1 {
        eprintln!("options -i and -n are incompatible");
        std::process::exit(2);
    }
    if maxjobs < 0 {
        maxjobs = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as isize;
    }

    let Some(sep) = rest.iter().position(|a| a == "--") else {
        usage();
    };
    let command = rest[..sep].to_vec();
    let arguments = rest[sep + 1..].to_vec();
    if nargs > 1 && command.is_empty() {
        eprintln!("option -n cannot be used without a command");
        std::process::exit(2);
    }

    let mut argidx = 0usize;
    let mut children: Vec<Child> = Vec::new();
    let mut ret = 0;
    while argidx < arguments.len() {
        while maxjobs != 0 && children.len() >= maxjobs as usize {
            ret |= reap_one(&mut children, true).unwrap_or(1);
        }
        if let Some(max) = maxload {
            while loadavg() >= max {
                if let Some(code) = reap_one(&mut children, false) {
                    ret |= code;
                } else {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
        let count = if command.is_empty() {
            1
        } else {
            nargs.min(arguments.len() - argidx)
        };
        let child = spawn_job(&command, &arguments[argidx..argidx + count], replace);
        children.push(child);
        argidx += count;
    }
    while !children.is_empty() {
        ret |= reap_one(&mut children, true).unwrap_or(1);
    }
    std::process::exit(ret);
}
