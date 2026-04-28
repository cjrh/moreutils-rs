// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::ExitStatus;

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

pub fn status_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else {
        #[cfg(unix)]
        {
            128 + status.signal().unwrap_or(1)
        }
        #[cfg(not(unix))]
        {
            1
        }
    }
}

pub fn exit_with_status(status: ExitStatus) -> ! {
    std::process::exit(status_code(status));
}

pub fn shell_command(command: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("/bin/sh");
    #[cfg(unix)]
    cmd.arg0("sh");
    cmd.arg("-c").arg("--").arg(command);
    cmd
}

pub fn usage(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

pub fn write_all_or_die<W: std::io::Write>(mut w: W, bytes: &[u8]) {
    if let Err(e) = w.write_all(bytes) {
        eprintln!("write: {e}");
        std::process::exit(1);
    }
}
