// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn main() {
    let mut verbose = false;
    let mut stderr_trigger = false;
    let mut command: Vec<String> = env::args().skip(1).collect();

    let mut command_start = 0;
    while command_start < command.len() {
        let arg = &command[command_start];
        if arg == "--" {
            command_start += 1;
            break;
        }
        if arg == "-" || !arg.starts_with('-') {
            break;
        }
        let mut option_chars: Vec<char> = arg.chars().skip(1).collect();
        if option_chars.len() > 1 && option_chars.last() == Some(&'-') {
            option_chars.pop();
        }
        for ch in option_chars {
            match ch {
                'v' => verbose = true,
                'e' => stderr_trigger = true,
                _ => eprintln!("Unknown option: {ch}"),
            }
        }
        command_start += 1;
    }
    command.drain(..command_start);

    if command.is_empty() {
        eprintln!("usage: chronic COMMAND...");
        std::process::exit(255);
    }

    let output = run_child(&command);
    let exit_code = output.status.code();
    let signaled = exit_code.is_none();
    let retval = exit_code.unwrap_or(0);
    let should_show = retval != 0 || signaled || (stderr_trigger && !output.stderr.is_empty());

    if should_show {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        if verbose {
            let _ = writeln!(stdout, "STDOUT:");
        }
        let _ = stdout.write_all(&output.stdout);
        if verbose {
            let _ = writeln!(stdout, "\nSTDERR:");
        }
        let _ = stdout.flush();
        let _ = stderr.write_all(&output.stderr);
        let _ = stderr.flush();
        if verbose {
            let _ = writeln!(stdout, "\nRETVAL: {retval}");
        }
    }

    if let Some(code) = exit_code {
        if code != 0 {
            std::process::exit(code);
        }
    } else {
        std::process::exit(1);
    }

    if stderr_trigger && !output.stderr.is_empty() {
        std::process::exit(2);
    }
}

fn run_child(command: &[String]) -> Output {
    if !command[0].contains('/') && env::var_os("PATH").is_none_or(|path| path.is_empty()) {
        eprintln!(
            "Command '{}' not found in  at /bin/chronic line 69.",
            command[0]
        );
        std::process::exit(255);
    }

    Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::inherit())
        .output()
        .unwrap_or_else(|err| {
            if err.raw_os_error() == Some(8) {
                return run_script_without_shebang(command)
                    .unwrap_or_else(|fallback_err| exec_error(&command[0], fallback_err));
            }
            exec_error(&command[0], err)
        })
}

fn run_script_without_shebang(command: &[String]) -> io::Result<Output> {
    let script = if command[0].contains('/') {
        PathBuf::from(&command[0])
    } else {
        resolve_in_path(&command[0]).unwrap_or_else(|| PathBuf::from(&command[0]))
    };

    Command::new("/bin/sh")
        .arg(script)
        .args(&command[1..])
        .stdin(Stdio::inherit())
        .output()
}

fn resolve_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
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
    match err.kind() {
        io::ErrorKind::NotFound => {
            if command.contains('/') {
                eprintln!("file not found: {command} at /bin/chronic line 69.");
            } else {
                let path = env::var("PATH").unwrap_or_default().replace(':', ", ");
                eprintln!("Command '{command}' not found in {path} at /bin/chronic line 69.");
            }
            std::process::exit(2);
        }
        io::ErrorKind::PermissionDenied => {
            if command.contains('/') {
                eprintln!("permission denied: {command} at /bin/chronic line 69.");
                std::process::exit(255);
            } else {
                let path = env::var("PATH").unwrap_or_default().replace(':', ", ");
                eprintln!("Command '{command}' not found in {path} at /bin/chronic line 69.");
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("{command}: {err}");
            std::process::exit(1);
        }
    }
}
