// SPDX-License-Identifier: GPL-2.0-only

use cjrh_moreutils_common::plain_os_error;
#[cfg(unix)]
use nix::sys::stat::{Mode, umask};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use tempfile::Builder;

fn usage() -> ! {
    println!("sponge [-a] <file>: soak up all input from stdin and write it to <file>");
    std::process::exit(0)
}

fn main() {
    let (append, outname) = parse_args();

    let mut input = Vec::new();
    if let Err(e) = io::stdin().read_to_end(&mut input) {
        print_os_error("failed to read from stdin", &e);
        std::process::exit(1);
    }

    let Some(outname) = outname else {
        write_all_or_die(
            io::stdout().lock(),
            &input,
            "error writing buffer to output file",
        );
        return;
    };

    let path = Path::new(&outname);
    let meta = fs::symlink_metadata(path).ok();
    let existing_mode = meta.as_ref().map(|m| {
        #[cfg(unix)]
        {
            m.permissions().mode()
        }
        #[cfg(not(unix))]
        {
            0o666
        }
    });
    #[cfg(unix)]
    let atomic_candidate = meta
        .as_ref()
        .map(|m| m.file_type().is_file() && !m.file_type().is_symlink())
        .unwrap_or(true);
    #[cfg(not(unix))]
    let atomic_candidate = meta.as_ref().map(|m| m.is_file()).unwrap_or(true);

    let mut data = Vec::new();
    if append && meta.as_ref().is_some_and(|m| m.is_file()) {
        if let Err(e) = File::open(path).and_then(|mut f| f.read_to_end(&mut data)) {
            eprintln!("read file: {e}");
            std::process::exit(1);
        }
    }
    data.extend_from_slice(&input);

    if atomic_candidate {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut tmp = Builder::new()
            .prefix(".sponge.")
            .tempfile_in(dir)
            .unwrap_or_else(|e| {
                print_os_error("error opening output file", &e);
                std::process::exit(1);
            });
        write_all_or_die(&mut tmp, &data, "error writing buffer to temporary file");
        if let Err(e) = tmp.flush() {
            print_os_error("error writing buffer to temporary file", &e);
            std::process::exit(1);
        }
        #[cfg(unix)]
        {
            let mode = existing_mode.unwrap_or_else(|| {
                let mask = umask(Mode::empty());
                umask(mask);
                0o666 & !mask.bits()
            });
            if let Err(e) = tmp
                .as_file()
                .set_permissions(fs::Permissions::from_mode(mode))
            {
                print_os_error("chmod", &e);
                std::process::exit(1);
            }
        }
        if let Err(e) = tmp.persist(path) {
            // Non-atomic fallback, matching sponge's behaviour for awkward filesystems.
            let (_file, _err) = (e.file, e.error);
            if let Err(e) = fs::write(path, &data) {
                print_os_error("error opening output file", &e);
                std::process::exit(1);
            }
        }
    } else {
        let mut opts = OpenOptions::new();
        opts.write(true).truncate(true).create(true);
        #[cfg(unix)]
        opts.mode(existing_mode.unwrap_or(0o666));
        let mut f = opts.open(path).unwrap_or_else(|e| {
            print_os_error("error opening output file", &e);
            std::process::exit(1);
        });
        write_all_or_die(&mut f, &data, "error writing buffer to output file");
    }
}

fn parse_args() -> (bool, Option<String>) {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "sponge".to_string());
    let mut append = false;
    let mut files = Vec::new();
    let mut parsing_options = true;

    for arg in args {
        if parsing_options && arg == "--" {
            parsing_options = false;
        } else if parsing_options && arg.starts_with('-') && arg.len() > 1 {
            for option in arg[1..].chars() {
                match option {
                    'a' => append = true,
                    'h' => usage(),
                    other => eprintln!("{program}: invalid option -- '{other}'"),
                }
            }
        } else {
            files.push(arg);
        }
    }

    (append, files.into_iter().next())
}

fn write_all_or_die<W: Write>(mut writer: W, bytes: &[u8], prefix: &str) {
    if let Err(e) = writer.write_all(bytes) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            terminate_by_signal(libc::SIGPIPE);
        }
        print_os_error(prefix, &e);
        std::process::exit(1);
    }
}

fn print_os_error(prefix: &str, err: &io::Error) {
    eprintln!("{prefix}: {}", os_error_message(err));
}

fn os_error_message(err: &io::Error) -> String {
    plain_os_error(err)
}

fn terminate_by_signal(signal: i32) -> ! {
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    std::process::exit(128 + signal);
}
