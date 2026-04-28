// SPDX-License-Identifier: GPL-3.0-or-later

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
    let mut append = false;
    let mut outname: Option<String> = None;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "-h" => usage(),
            "-a" => append = true,
            _ if outname.is_none() => outname = Some(arg),
            _ => {
                eprintln!("sponge: too many arguments");
                std::process::exit(1);
            }
        }
    }

    let mut input = Vec::new();
    if let Err(e) = io::stdin().read_to_end(&mut input) {
        eprintln!("failed to read from stdin: {e}");
        std::process::exit(1);
    }

    let Some(outname) = outname else {
        if let Err(e) = io::stdout().write_all(&input) {
            eprintln!("error writing buffer to output file: {e}");
            std::process::exit(1);
        }
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
                eprintln!("mkstemp failed: {e}");
                std::process::exit(1);
            });
        if let Err(e) = tmp.write_all(&data).and_then(|_| tmp.flush()) {
            eprintln!("error writing buffer to temporary file: {e}");
            std::process::exit(1);
        }
        #[cfg(unix)]
        {
            let mode = existing_mode.unwrap_or_else(|| {
                let mask = unsafe { libc::umask(0) };
                unsafe { libc::umask(mask) };
                0o666 & !mask
            });
            if let Err(e) = tmp
                .as_file()
                .set_permissions(fs::Permissions::from_mode(mode))
            {
                eprintln!("chmod: {e}");
                std::process::exit(1);
            }
        }
        if let Err(e) = tmp.persist(path) {
            // Non-atomic fallback, matching sponge's behaviour for awkward filesystems.
            let (_file, _err) = (e.file, e.error);
            if let Err(e) = fs::write(path, &data) {
                eprintln!("error opening output file: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let mut opts = OpenOptions::new();
        opts.write(true).truncate(true).create(true);
        #[cfg(unix)]
        opts.mode(existing_mode.unwrap_or(0o666));
        let mut f = opts.open(path).unwrap_or_else(|e| {
            eprintln!("error opening output file: {e}");
            std::process::exit(1);
        });
        if let Err(e) = f.write_all(&data) {
            eprintln!("error writing buffer to output file: {e}");
            std::process::exit(1);
        }
    }
}
