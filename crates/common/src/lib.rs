// SPDX-License-Identifier: GPL-2.0-only

use std::io;
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

fn os_error_suffix_index(message: &str) -> Option<usize> {
    for (index, _) in message.match_indices(" (os error ") {
        let Some((code, remainder)) = message[index + " (os error ".len()..].split_once(')') else {
            continue;
        };
        if code.parse::<i32>().is_ok()
            && (remainder.is_empty() || remainder.starts_with(" at path "))
        {
            return Some(index);
        }
    }
    None
}

fn remove_os_error_suffix(message: String) -> String {
    match os_error_suffix_index(&message) {
        Some(index) => message[..index].to_owned(),
        None => message,
    }
}

/// Returns an OS error message without Rust's `" (os error N)"` suffix.
///
/// This matches the `strerror`-style messages emitted by the original utilities
/// without requiring callers to handle C string pointers.
pub fn plain_os_error(err: &io::Error) -> String {
    remove_os_error_suffix(err.to_string())
}

pub fn write_all_or_die<W: std::io::Write>(mut w: W, bytes: &[u8]) {
    if let Err(e) = w.write_all(bytes) {
        eprintln!("write: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{plain_os_error, remove_os_error_suffix};
    use std::io;

    #[test]
    fn plain_os_error_removes_the_raw_os_error_suffix_and_context() {
        let error = io::Error::from_raw_os_error(2);
        let rendered = error.to_string();
        let expected = rendered
            .split_once(" (os error ")
            .map_or(rendered.as_str(), |(message, _)| message);

        assert_eq!(plain_os_error(&error), expected);
        assert_eq!(
            remove_os_error_suffix(
                "No such file or directory (os error 2) at path \"missing\"".to_owned(),
            ),
            "No such file or directory",
        );
    }

    #[test]
    fn plain_os_error_preserves_non_os_errors() {
        let error = io::Error::other("detail (os error 2) retained");

        assert_eq!(plain_os_error(&error), "detail (os error 2) retained");
    }
}
