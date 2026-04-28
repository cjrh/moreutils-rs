// SPDX-License-Identifier: GPL-3.0-or-later

use moreutils_common::{shell_command, status_code};
use std::env;
use std::process::Stdio;

fn main() {
    let args: Vec<String> = env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("mispipe");
    if args.len() != 3 {
        eprintln!("{prog}: Wrong number of args, aborting");
        std::process::exit(1);
    }

    let mut cmd1 = shell_command(&args[1]);
    let mut child1 = cmd1.stdout(Stdio::piped()).spawn().unwrap_or_else(|e| {
        eprintln!("{prog}: {}: {e}", args[1]);
        std::process::exit(1);
    });
    let stdout1 = child1.stdout.take().expect("piped stdout");
    let mut cmd2 = shell_command(&args[2]);
    let mut child2 = cmd2
        .stdin(Stdio::from(stdout1))
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("{prog}: {}: {e}", args[2]);
            std::process::exit(1);
        });
    let status1 = child1.wait().unwrap_or_else(|e| {
        eprintln!("{prog}: waitpid() failed: {e}");
        std::process::exit(1);
    });
    let _ = child2.wait();
    std::process::exit(status_code(status1));
}
