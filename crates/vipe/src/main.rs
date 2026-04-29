// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::Command;
use tempfile::Builder;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn program_name() -> String {
    env::args().next().unwrap_or_else(|| "vipe".into())
}

fn usage() -> ! {
    eprintln!("Usage: {} [--suffix=extension]", program_name());
    std::process::exit(255);
}

fn unknown_option(option: &str) -> ! {
    eprintln!("Unknown option: {option}");
    usage();
}

fn missing_suffix_argument() -> ! {
    eprintln!("Option suffix requires an argument");
    usage();
}

fn parse_suffix() -> String {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut suffix = String::new();
    let mut i = 0;
    let mut parse_options = true;

    while i < args.len() {
        let arg = &args[i];
        if parse_options && arg == "--" {
            parse_options = false;
            i += 1;
            continue;
        }

        if parse_options && arg.starts_with('-') && arg.len() > 1 {
            let trimmed = arg.trim_start_matches('-');
            let (name, inline_value) = match trimmed.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (trimmed, None),
            };
            let lower = name.to_ascii_lowercase();
            if !"suffix".starts_with(&lower) || lower.is_empty() {
                unknown_option(name);
            }
            let value = if let Some(value) = inline_value {
                value
            } else {
                i += 1;
                if i >= args.len() {
                    missing_suffix_argument();
                }
                args[i].clone()
            };
            suffix = value;
        }
        i += 1;
    }

    if !suffix.is_empty() && !suffix.starts_with('.') {
        suffix.insert(0, '.');
    }
    suffix
}

fn split_editor(value: &str) -> Vec<String> {
    value.split_whitespace().map(ToOwned::to_owned).collect()
}

fn is_executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            {
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

fn editor() -> (Vec<String>, Option<i32>) {
    let editor_probe = fs::metadata("/usr/bin/editor")
        .err()
        .and_then(|e| e.raw_os_error());
    let mut editor = if is_executable(Path::new("/usr/bin/editor")) {
        vec!["/usr/bin/editor".into()]
    } else {
        vec!["vi".into()]
    };
    if let Ok(value) = env::var("EDITOR") {
        editor = split_editor(&value);
    }
    if let Ok(value) = env::var("VISUAL") {
        editor = split_editor(&value);
    }
    (editor, editor_probe)
}

fn os_error_text(err: &io::Error) -> String {
    match err.raw_os_error() {
        Some(errno) => unsafe {
            CStr::from_ptr(libc::strerror(errno))
                .to_string_lossy()
                .into_owned()
        },
        None => err.to_string(),
    }
}

fn die_io(message: &str, err: io::Error) -> ! {
    eprintln!("{message}: {}", os_error_text(&err));
    std::process::exit(err.raw_os_error().unwrap_or(255));
}

#[cfg(unix)]
fn stdin_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_tty() -> bool {
    false
}

#[cfg(unix)]
fn reopen_stdio_on_tty() -> File {
    unsafe {
        if libc::close(libc::STDIN_FILENO) == -1 {
            die_io("close stdin", io::Error::last_os_error());
        }
    }
    let tty_in = OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .unwrap_or_else(|e| die_io("reopen stdin", e));
    if tty_in.as_raw_fd() != libc::STDIN_FILENO {
        unsafe {
            if libc::dup2(tty_in.as_raw_fd(), libc::STDIN_FILENO) == -1 {
                die_io("reopen stdin", io::Error::last_os_error());
            }
        }
    }
    std::mem::forget(tty_in);

    let out_fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if out_fd == -1 {
        die_io("save stdout", io::Error::last_os_error());
    }
    unsafe {
        if libc::close(libc::STDOUT_FILENO) == -1 {
            die_io("close stdout", io::Error::last_os_error());
        }
    }
    let tty_out = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .unwrap_or_else(|e| die_io("reopen stdout", e));
    if tty_out.as_raw_fd() != libc::STDOUT_FILENO {
        unsafe {
            if libc::dup2(tty_out.as_raw_fd(), libc::STDOUT_FILENO) == -1 {
                die_io("reopen stdout", io::Error::last_os_error());
            }
        }
    }
    std::mem::forget(tty_out);

    unsafe { File::from_raw_fd(out_fd) }
}

#[cfg(unix)]
use std::os::fd::FromRawFd;

#[cfg(not(unix))]
fn reopen_stdio_on_tty() -> File {
    File::create("/dev/null").unwrap()
}

fn main() {
    let suffix = parse_suffix();
    let mut tmp = Builder::new()
        .suffix(&suffix)
        .tempfile()
        .unwrap_or_else(|_| {
            eprintln!("cannot create tempfile");
            std::process::exit(1);
        });

    if !stdin_is_tty() {
        let mut input = Vec::new();
        if let Err(e) = io::stdin().read_to_end(&mut input) {
            die_io("read stdin", e);
        }
        if let Err(e) = tmp.write_all(&input) {
            die_io("write temp", e);
        }
    }
    if let Err(e) = tmp.flush() {
        die_io("write temp", e);
    }

    let mut final_stdout = reopen_stdio_on_tty();

    let (editor, editor_errno) = editor();
    let status = if let Some((program, args)) = editor.split_first() {
        Command::new(program).args(args).arg(tmp.path()).status()
    } else {
        Command::new("").arg(tmp.path()).status()
    }
    .unwrap_or_else(|e| {
        if let Some(program) = editor.first() {
            eprintln!(
                "Can't exec \"{}\": {} at {} line 87.",
                program,
                os_error_text(&e),
                program_name()
            );
        }
        eprintln!("{} exited nonzero, aborting", editor.join(" "));
        std::process::exit(e.raw_os_error().or(editor_errno).unwrap_or(255));
    });
    if !status.success() {
        eprintln!("{} exited nonzero, aborting", editor.join(" "));
        std::process::exit(editor_errno.unwrap_or(255));
    }

    let mut input = File::open(tmp.path()).unwrap_or_else(|e| {
        eprintln!(
            "{}: cannot read {}: {}",
            program_name(),
            tmp.path().display(),
            os_error_text(&e)
        );
        std::process::exit(e.raw_os_error().unwrap_or(255));
    });
    let mut out = Vec::new();
    input.read_to_end(&mut out).unwrap_or_else(|e| {
        eprintln!(
            "{}: cannot read {}: {}",
            program_name(),
            tmp.path().display(),
            os_error_text(&e)
        );
        std::process::exit(e.raw_os_error().unwrap_or(255));
    });
    if let Err(e) = final_stdout.write_all(&out) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::raise(libc::SIGPIPE);
            }
        }
        die_io("write failure", e);
    }
}
