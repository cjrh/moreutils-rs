// SPDX-License-Identifier: GPL-2.0-only

use cjrh_moreutils_common::shell_command;
use std::env;
use std::fs;
use std::process::{Child, Command, ExitStatus};

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

fn spawn_job(command: &[String], args: &[String], replace: bool) -> Option<Child> {
    if command.is_empty() {
        return Some(shell_command(&args[0]).spawn().unwrap_or_else(|e| {
            eprintln!("parallel: {}: {e}", args[0]);
            std::process::exit(1);
        }));
    }

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
    Command::new(&argv[0]).args(&argv[1..]).spawn().ok()
}

fn parallel_status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else {
        1
    }
}

fn reap_one(children: &mut Vec<Child>, block: bool) -> Option<i32> {
    loop {
        for i in 0..children.len() {
            match children[i].try_wait() {
                Ok(Some(status)) => {
                    let mut child = children.remove(i);
                    let _ = child.wait();
                    return Some(parallel_status_code(status));
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
    let prog = env::args().next().unwrap_or_else(|| "parallel".to_string());
    let args: Vec<String> = env::args().skip(1).collect();
    let mut maxjobs: isize = -1;
    let mut maxload: Option<f64> = None;
    let mut replace = false;
    let mut nargs = 1usize;
    let mut argidx_parse = 0usize;

    while argidx_parse < args.len() {
        let arg = &args[argidx_parse];
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }

        let option_text = &arg[1..];
        let mut option_idx = 0usize;
        while option_idx < option_text.len() {
            let ch = option_text[option_idx..].chars().next().unwrap();
            option_idx += ch.len_utf8();
            match ch {
                'h' => usage(),
                'i' => replace = true,
                'j' | 'l' | 'n' => {
                    let value = if option_idx < option_text.len() {
                        let value = option_text[option_idx..].to_string();
                        option_idx = option_text.len();
                        value
                    } else {
                        argidx_parse += 1;
                        let Some(value) = args.get(argidx_parse) else {
                            eprintln!("{prog}: option requires an argument -- '{ch}'");
                            usage();
                        };
                        value.clone()
                    };
                    match ch {
                        'j' => {
                            maxjobs = value.parse().unwrap_or_else(|_| {
                                eprintln!("option '{value}' is not a number");
                                std::process::exit(2)
                            });
                        }
                        'l' => {
                            maxload = Some(value.parse().unwrap_or_else(|_| {
                                eprintln!("option '{value}' is not a number");
                                std::process::exit(2)
                            }));
                        }
                        'n' => {
                            let parsed: isize = value.parse().unwrap_or_else(|_| {
                                eprintln!("option '{value}' is not a positive number");
                                std::process::exit(2)
                            });
                            if parsed <= 0 {
                                eprintln!("option '{value}' is not a positive number");
                                std::process::exit(2);
                            }
                            nargs = parsed as usize;
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    eprintln!("{prog}: invalid option -- '{ch}'");
                    usage();
                }
            }
        }
        argidx_parse += 1;
    }

    let rest = args[argidx_parse..].to_vec();
    if replace && nargs > 1 {
        eprintln!("options -i and -n are incompatible");
        std::process::exit(2);
    }
    let Some(sep) = rest.iter().position(|a| a == "--") else {
        std::process::exit(0);
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
        let max_running = if maxjobs == 0 {
            Some(1)
        } else if maxjobs > 0 {
            Some(maxjobs as usize)
        } else {
            None
        };
        while max_running.is_some_and(|max| children.len() >= max) {
            ret |= reap_one(&mut children, true).unwrap_or(1);
        }
        if let Some(max) = maxload {
            while max >= 0.0 && loadavg() >= max {
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
        if let Some(child) = spawn_job(&command, &arguments[argidx..argidx + count], replace) {
            children.push(child);
        } else {
            ret |= 1;
        }
        argidx += count;
    }
    while !children.is_empty() {
        ret |= reap_one(&mut children, true).unwrap_or(1);
    }
    std::process::exit(ret);
}
