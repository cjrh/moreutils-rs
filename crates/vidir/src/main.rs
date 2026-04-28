// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

fn editor() -> String {
    env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into())
}

fn main() {
    let dirs: Vec<String> = env::args().skip(1).collect();
    let dirs = if dirs.is_empty() {
        vec![".".to_string()]
    } else {
        dirs
    };
    let mut entries: HashMap<u64, PathBuf> = HashMap::new();
    let mut tmp = NamedTempFile::new().unwrap();
    for dir in &dirs {
        let base = Path::new(dir);
        let rd = fs::read_dir(base).unwrap_or_else(|e| {
            eprintln!("vidir: {dir}: {e}");
            std::process::exit(1);
        });
        for ent in rd.flatten() {
            let path = ent.path();
            let md = fs::symlink_metadata(&path).unwrap();
            let ino = md.ino();
            entries.insert(ino, path.clone());
            let display = if dirs.len() == 1 {
                path.file_name().unwrap().to_string_lossy().into_owned()
            } else {
                path.to_string_lossy().into_owned()
            };
            writeln!(tmp, "{ino}\t{display}").unwrap();
        }
    }
    tmp.flush().unwrap();
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", editor()))
        .arg("vidir")
        .arg(tmp.path())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("vidir: editor: {e}");
            std::process::exit(1);
        });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let mut seen = HashSet::new();
    let text = fs::read_to_string(tmp.path()).unwrap();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((ino_s, name)) = line.split_once(char::is_whitespace) else {
            eprintln!("vidir: bad line: {line}");
            std::process::exit(1);
        };
        let ino: u64 = ino_s.parse().unwrap_or_else(|_| {
            eprintln!("vidir: bad inode: {ino_s}");
            std::process::exit(1);
        });
        let Some(old) = entries.get(&ino) else {
            eprintln!("vidir: unknown inode: {ino}");
            std::process::exit(1);
        };
        seen.insert(ino);
        let name = name.trim_start();
        let new = if dirs.len() == 1 {
            Path::new(&dirs[0]).join(name)
        } else {
            PathBuf::from(name)
        };
        if &new != old {
            fs::rename(old, &new).unwrap_or_else(|e| {
                eprintln!("vidir: rename {} to {}: {e}", old.display(), new.display());
                std::process::exit(1);
            });
        }
    }
    for (ino, old) in entries {
        if !seen.contains(&ino) {
            if old.is_dir() {
                fs::remove_dir(&old).ok();
            } else {
                fs::remove_file(&old).ok();
            }
        }
    }
}
