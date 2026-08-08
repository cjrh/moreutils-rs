// SPDX-License-Identifier: GPL-2.0-only

use cjrh_moreutils_common::plain_os_error;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;

type Items = BTreeMap<usize, String>;

fn main() {
    let argv0 = env::args().next().unwrap_or_else(|| "vidir".to_string());
    let (verbose, args) = parse_args(&argv0);
    let mut paths = collect_paths(&argv0, args);

    if paths.iter().any(|path| path.chars().any(char::is_control)) {
        eprintln!("{argv0}: control characters in filenames are not supported");
        std::process::exit(255);
    }

    let mut tmp = tempfile::Builder::new()
        .prefix("dir")
        .tempfile()
        .unwrap_or_else(|err| die255(&argv0, &format!("cannot create temporary file: {err}")));

    let mut items = Items::new();
    let mut number = 0usize;
    for path in paths.drain(..) {
        if is_dot_or_dotdot_path(&path) {
            continue;
        }
        number += 1;
        items.insert(number, path.clone());
        writeln!(tmp, "{number:04}\t{path}").unwrap_or_else(|err| {
            die255(
                &argv0,
                &format!("cannot write {}: {err}", tmp.path().display()),
            )
        });
    }
    tmp.flush().unwrap_or_else(|err| {
        die255(
            &argv0,
            &format!("cannot write {}: {err}", tmp.path().display()),
        )
    });

    run_editor(tmp.path());
    let mut error = apply_edits(&argv0, verbose, tmp.path(), &mut items);

    let backup = format!("{}~", tmp.path().display());
    if path_exists_or_symlink(&backup) {
        let _ = fs::remove_file(backup);
    }

    let mut remaining: Vec<String> = items.into_values().collect();
    remaining.sort();
    for item in remaining.into_iter().rev() {
        if let Err(err) = remove_item(&item) {
            eprintln!("{argv0}: failed to remove {item}: {}", plain_os_error(&err));
            error = true;
        }
        if verbose {
            println!("removed '{item}'");
        }
    }

    std::process::exit(if error { 1 } else { 0 });
}

fn parse_args(argv0: &str) -> (bool, Vec<String>) {
    let mut verbose = false;
    let mut positional = Vec::new();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--" {
            positional.extend(args);
            break;
        } else if arg == "-v" || arg == "--verbose" {
            verbose = true;
        } else if arg.starts_with('-') && arg != "-" {
            let opt = arg.trim_start_matches('-');
            eprintln!("Unknown option: {opt}");
            eprintln!("Usage: {argv0} [--verbose] [directory|file|-]");
            std::process::exit(255);
        } else {
            positional.push(arg);
        }
    }

    if positional.is_empty() {
        positional.push(".".to_string());
    }
    (verbose, positional)
}

fn collect_paths(argv0: &str, args: Vec<String>) -> Vec<String> {
    let mut paths = Vec::new();
    for arg in args {
        if arg == "-" {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let mut line =
                    line.unwrap_or_else(|err| die255(argv0, &format!("cannot read stdin: {err}")));
                if line.ends_with('\r') {
                    line.pop();
                }
                paths.push(line);
            }
            if let Err(err) = File::open("/dev/tty") {
                eprintln!("reopen: {}", plain_os_error(&err));
                std::process::exit(err.raw_os_error().unwrap_or(1));
            }
        } else if Path::new(&arg).is_dir() {
            let prefix = format!("{}/", arg.trim_end_matches('/'));
            let mut names = Vec::new();
            let read_dir = fs::read_dir(&arg)
                .unwrap_or_else(|err| die255(argv0, &format!("cannot read {arg}: {err}")));
            for entry in read_dir {
                let entry =
                    entry.unwrap_or_else(|err| die255(argv0, &format!("cannot read {arg}: {err}")));
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            paths.extend(names.into_iter().map(|name| format!("{prefix}{name}")));
        } else {
            paths.push(arg);
        }
    }
    paths
}

fn run_editor(path: &Path) {
    let mut editor = vec!["vi".to_string()];
    if Path::new("/usr/bin/editor").is_file() && is_executable(Path::new("/usr/bin/editor")) {
        editor = vec!["/usr/bin/editor".to_string()];
    }
    if let Ok(value) = env::var("EDITOR") {
        editor = split_editor(&value);
    }
    if let Ok(value) = env::var("VISUAL") {
        editor = split_editor(&value);
    }
    if editor.is_empty() {
        editor.push("".to_string());
    }

    let status = Command::new(&editor[0])
        .args(&editor[1..])
        .arg(path)
        .status();

    match status {
        Ok(status) if status.success() => {}
        _ => {
            eprintln!("{} exited nonzero, aborting", editor.join(" "));
            std::process::exit(2);
        }
    }
}

fn split_editor(value: &str) -> Vec<String> {
    value
        .split(' ')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

fn apply_edits(argv0: &str, verbose: bool, path: &Path, items: &mut Items) -> bool {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| die255(argv0, &format!("cannot read {}: {err}", path.display())));
    let mut error = false;

    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.trim().is_empty() {
            continue;
        }

        let Some((number, name)) = parse_edit_line(line) else {
            eprintln!("{argv0}: unable to parse line \"{line}\", aborting");
            std::process::exit(25);
        };
        let Some(src) = items.get(&number).cloned() else {
            eprintln!("{argv0}: unknown item number {number}");
            std::process::exit(25);
        };

        if name != src {
            if name.is_empty() {
                continue;
            }

            if !path_exists_or_symlink(&src) {
                eprintln!("{argv0}: {src} does not exist");
                items.remove(&number);
                continue;
            }

            if path_exists_or_symlink(name) {
                let tmp_name = unused_backup_name(name);
                if let Err(err) = fs::rename(name, &tmp_name) {
                    eprintln!(
                        "{argv0}: failed to rename {name} to {tmp_name}: {}",
                        plain_os_error(&err)
                    );
                    error = true;
                } else {
                    if verbose {
                        println!("'{name}' -> '{tmp_name}'");
                    }
                    for item in items.values_mut() {
                        if item == name {
                            *item = tmp_name.clone();
                        }
                    }
                }
            }

            let dir = dirname(name);
            if !Path::new(&dir).is_dir() {
                if let Err(err) = fs::create_dir_all(&dir) {
                    eprintln!(
                        "{argv0}: failed to create directory tree {dir}: {}",
                        plain_os_error(&err)
                    );
                    error = true;
                    items.remove(&number);
                    continue;
                }
            }

            if let Err(err) = fs::rename(&src, name) {
                eprintln!(
                    "{argv0}: failed to rename {src} to {name}: {}",
                    plain_os_error(&err)
                );
                error = true;
            } else {
                if Path::new(name).is_dir() {
                    update_directory_children(items, &src, name);
                }
                if verbose {
                    println!("'{src}' => '{name}'");
                }
            }
        }
        items.remove(&number);
    }

    error
}

fn parse_edit_line(line: &str) -> Option<(usize, &str)> {
    let digit_len = line.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let number = line[..digit_len].parse().ok()?;
    let rest = &line[digit_len..];
    let name = rest.strip_prefix('\t').unwrap_or(rest);
    Some((number, name))
}

fn unused_backup_name(name: &str) -> String {
    let mut backup = format!("{name}~");
    let mut count = 0usize;
    while path_exists_or_symlink(&backup) {
        count += 1;
        backup = format!("{name}~{count}");
    }
    backup
}

fn update_directory_children(items: &mut Items, src: &str, dst: &str) {
    let prefix = format!("{src}/");
    for item in items.values_mut() {
        if item == src {
            *item = dst.to_string();
        } else if let Some(rest) = item.strip_prefix(&prefix) {
            *item = format!("{dst}/{rest}");
        }
    }
}

fn remove_item(item: &str) -> io::Result<()> {
    let path = Path::new(item);
    if path.is_dir()
        && !path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

fn path_exists_or_symlink<P: AsRef<Path>>(path: P) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn dirname(path: &str) -> String {
    let parent = Path::new(path).parent().unwrap_or_else(|| Path::new("."));
    if parent.as_os_str().is_empty() {
        ".".to_string()
    } else {
        parent.to_string_lossy().into_owned()
    }
}

fn is_dot_or_dotdot_path(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == "." || path == ".." || path.ends_with("/.") || path.ends_with("/..")
}

fn die255(argv0: &str, message: &str) -> ! {
    eprintln!("{argv0}: {message}");
    std::process::exit(255);
}
