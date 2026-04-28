// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::{Local, TimeZone, Utc};
use std::env;
use std::io::{self, BufRead, Write};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: ts [-r] [-i | -s] [-m] [format]");
    std::process::exit(255)
}

fn expand_subseconds(fmt: &str, secs: i64, micros: u32) -> String {
    let mut f = fmt.to_string();
    f = f.replace("%.S", &format!("%S.{micros:06}"));
    f = f.replace("%.T", &format!("%H:%M:%S.{micros:06}"));
    f = f.replace("%.s", &format!("{secs}.{micros:06}"));
    f
}

fn format_elapsed(fmt: &str, elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs() as i64;
    let micros = elapsed.subsec_micros();
    let fmt = expand_subseconds(fmt, secs, micros);
    Utc.timestamp_opt(secs, micros * 1000)
        .single()
        .unwrap()
        .format(&fmt)
        .to_string()
}

fn main() {
    let mut rel = false;
    let mut inc = false;
    let mut since_start = false;
    let mut _mono = false;
    let mut free = Vec::new();
    for arg in env::args().skip(1) {
        if arg.starts_with('-') && arg != "-" {
            for ch in arg.chars().skip(1) {
                match ch {
                    'r' => rel = true,
                    'i' => {
                        inc = true;
                        since_start = false;
                    }
                    's' => {
                        inc = false;
                        since_start = true;
                    }
                    'm' => _mono = true,
                    _ => usage(),
                }
            }
        } else {
            free.push(arg);
        }
    }
    if free.len() > 1 {
        usage();
    }
    let format = free.first().cloned().unwrap_or_else(|| {
        if inc || since_start {
            "%H:%M:%S".into()
        } else {
            "%b %d %H:%M:%S".into()
        }
    });

    let start = Instant::now();
    let mut last = start;
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let mut line = line.unwrap_or_default();
        line.push('\n');
        if rel {
            // A conservative subset: if a format was provided, leave parsing to
            // future work rather than corrupting unrecognised input. The normal
            // timestamping modes are fully implemented.
            let _ = stdout.write_all(line.as_bytes());
            continue;
        }
        let stamp = if inc {
            let now = Instant::now();
            let d = now.duration_since(last);
            last = now;
            format_elapsed(&format, d)
        } else if since_start {
            format_elapsed(&format, Instant::now().duration_since(start))
        } else {
            let now = Local::now();
            let fmt = expand_subseconds(&format, now.timestamp(), now.timestamp_subsec_micros());
            now.format(&fmt).to_string()
        };
        let _ = write!(stdout, "{stamp} {line}");
        let _ = stdout.flush();
    }
}
