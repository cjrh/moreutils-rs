// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::{Datelike, FixedOffset, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use regex::bytes::{Captures, Regex};
use std::env;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: ts [-r] [-i | -s] [-m] [format]");
    std::process::exit(255)
}

fn option_error(message: &str) -> ! {
    eprintln!("{message}");
    usage()
}

#[derive(Debug)]
struct Options {
    rel: bool,
    inc: bool,
    since_start: bool,
    _mono: bool,
    format: Option<String>,
    files: Vec<String>,
}

fn parse_args() -> Options {
    let mut rel = false;
    let mut inc = false;
    let mut since_start = false;
    let mut mono = false;
    let mut free = Vec::new();
    let mut stop_options = false;

    for arg in env::args().skip(1) {
        if !stop_options && arg == "--" {
            stop_options = true;
            continue;
        }

        if !stop_options && arg.starts_with('-') && arg != "-" {
            let name = arg.trim_start_matches('-');
            let (name, value) = match name.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (name, None),
            };
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                "r" | "i" | "s" | "m" => {
                    if value.is_some() {
                        option_error(&format!("Option {lower} does not take an argument"));
                    }
                    match lower.as_str() {
                        "r" => rel = true,
                        "i" => inc = true,
                        "s" => since_start = true,
                        "m" => mono = true,
                        _ => unreachable!(),
                    }
                }
                _ => option_error(&format!("Unknown option: {lower}")),
            }
        } else {
            free.push(arg);
        }
    }

    let format = free.first().cloned();
    let files = if free.len() > 1 {
        free.into_iter().skip(1).collect()
    } else {
        Vec::new()
    };

    Options {
        rel,
        inc,
        since_start,
        _mono: mono,
        format,
        files,
    }
}

fn expand_subseconds(fmt: &str, _secs: i64, micros: u32) -> String {
    let mut expanded = String::with_capacity(fmt.len() + 24);
    let mut chars = fmt.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' && chars.peek() == Some(&'.') {
            chars.next();
            match chars.peek().copied() {
                Some('S') => {
                    chars.next();
                    expanded.push_str(&format!("%S.{micros:06}"));
                }
                Some('T') => {
                    chars.next();
                    expanded.push_str(&format!("%T.{micros:06}"));
                }
                Some('s') => {
                    chars.next();
                    expanded.push_str(&format!("%s.{micros:06}"));
                }
                _ => {
                    expanded.push('%');
                    expanded.push('.');
                }
            }
        } else {
            expanded.push(ch);
        }
    }
    expanded
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
    let opts = parse_args();
    let format = opts.format.clone().unwrap_or_else(|| {
        if opts.inc || opts.since_start {
            "%H:%M:%S".into()
        } else {
            "%b %d %H:%M:%S".into()
        }
    });

    let start = Instant::now();
    let mut last = start;
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let rel_re = opts.rel.then(timestamp_regex);

    if opts.files.is_empty() {
        let stdin = io::stdin();
        process_reader(
            stdin.lock(),
            &mut stdout,
            &format,
            &opts,
            start,
            &mut last,
            rel_re.as_ref(),
        );
    } else {
        for file in &opts.files {
            if file == "-" {
                let stdin = io::stdin();
                process_reader(
                    stdin.lock(),
                    &mut stdout,
                    &format,
                    &opts,
                    start,
                    &mut last,
                    rel_re.as_ref(),
                );
            } else {
                match File::open(file) {
                    Ok(file_handle) => process_reader(
                        io::BufReader::new(file_handle),
                        &mut stdout,
                        &format,
                        &opts,
                        start,
                        &mut last,
                        rel_re.as_ref(),
                    ),
                    Err(err) => eprintln!(
                        "Can't open {file}: {err} at {} line 113.",
                        env::args().next().unwrap_or_else(|| "ts".into())
                    ),
                }
            }
        }
    }
}

fn process_reader<R: BufRead>(
    mut reader: R,
    stdout: &mut io::BufWriter<io::StdoutLock<'_>>,
    format: &str,
    opts: &Options,
    start: Instant,
    last: &mut Instant,
    rel_re: Option<&Regex>,
) {
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).unwrap_or_default();
        if read == 0 {
            break;
        }

        if opts.rel {
            let rendered = convert_relative(&line, format, opts.format.is_some(), rel_re.unwrap());
            let _ = stdout.write_all(&rendered);
            let _ = stdout.flush();
            continue;
        }

        let stamp = if opts.inc {
            let now = Instant::now();
            let d = now.duration_since(*last);
            *last = now;
            format_elapsed(format, d)
        } else if opts.since_start {
            format_elapsed(format, Instant::now().duration_since(start))
        } else {
            let now = Local::now();
            let fmt = expand_subseconds(format, now.timestamp(), now.timestamp_subsec_micros());
            now.format(&fmt).to_string()
        };
        let _ = write!(stdout, "{stamp} ");
        let _ = stdout.write_all(&line);
        let _ = stdout.flush();
    }
}

fn timestamp_regex() -> Regex {
    Regex::new(
        r"(?x)
        \b(
            \d\d[-\s/]\w\w\w(?:/\d\d+)?[\s:]\d\d:\d\d(?::\d\d)?(?:\s+[+-]\d\d\d\d)?
          |
            \w{3}\s+\d{1,2}\s+\d\d:\d\d:\d\d
          |
            \d\d\d\d[-:]\d\d[-:]\d\dT\d\d:\d\d:\d\d\.\d+Z?
          |
            (?:\w\w\w,?\s+)?\d+\s+\w\w\w\s+\d\d+\s+\d\d:\d\d:\d\d(?:\s+\w\w\w|\s[+-]\d\d\d\d)?
          |
            \w\w\w\s+\w\w\w\s+\d\d\s+\d\d:\d\d
        )\b",
    )
    .expect("timestamp regex")
}

fn convert_relative(line: &[u8], format: &str, use_format: bool, re: &Regex) -> Vec<u8> {
    re.replace_all(line, |caps: &Captures<'_>| {
        let matched = caps.get(1).unwrap().as_bytes();
        let matched_str = std::str::from_utf8(matched).unwrap_or("");
        match parse_timestamp(matched_str) {
            Some(epoch) if use_format => format_relative_epoch(epoch, format)
                .map(|s| s.into_bytes())
                .unwrap_or_else(|| matched.to_vec()),
            Some(epoch) => concise_ago(Local::now().timestamp() - epoch).into_bytes(),
            None => matched.to_vec(),
        }
    })
    .into_owned()
}

fn format_relative_epoch(epoch: i64, format: &str) -> Option<String> {
    let format = sanitize_relative_format(format);
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .or_else(|| match Local.timestamp_opt(epoch, 0) {
            LocalResult::Ambiguous(a, _) => Some(a),
            _ => None,
        })
        .map(|dt| dt.format(&format).to_string())
}

fn sanitize_relative_format(format: &str) -> String {
    let mut out = String::with_capacity(format.len());
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                out.push_str("%%");
                chars.next();
            }
            Some('.') => {
                chars.next();
                match chars.peek().copied() {
                    Some('S' | 's' | 'T') => {
                        let token = chars.next().unwrap();
                        out.push_str("%%.");
                        out.push(token);
                    }
                    _ => out.push_str("%."),
                }
            }
            _ => out.push('%'),
        }
    }
    out
}

fn parse_timestamp(text: &str) -> Option<i64> {
    parse_iso8601(text)
        .or_else(|| parse_syslog(text))
        .or_else(|| parse_day_month_time(text))
        .or_else(|| parse_rfc_like(text))
        .or_else(|| parse_lastlog(text))
}

fn parse_iso8601(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let date_parts: Vec<&str> = date.split(['-', ':']).collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year = date_parts[0].parse().ok()?;
    let month = date_parts[1].parse().ok()?;
    let day = date_parts[2].parse().ok()?;
    let zulu = rest.ends_with('Z');
    let rest = rest.trim_end_matches('Z');
    let (time, _frac) = rest.split_once('.')?;
    let (hour, minute, second) = parse_hms(time, true)?;
    let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    if zulu {
        Some(
            FixedOffset::east_opt(0)?
                .from_utc_datetime(&naive)
                .timestamp(),
        )
    } else {
        local_timestamp(naive)
    }
}

fn parse_syslog(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let month = month_number(parts[0])?;
    let day = parts[1].parse().ok()?;
    let (hour, minute, second) = parse_hms(parts[2], true)?;
    local_timestamp(resolve_yearless(month, day, hour, minute, second)?)
}

fn parse_day_month_time(text: &str) -> Option<i64> {
    let normalized = text.replace('-', " ");
    let parts: Vec<&str> = normalized.split_whitespace().collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let day = parts[0].parse().ok()?;
    let month = month_number(parts[1])?;
    let (year, time_index) = if let Some((year_text, _)) = parts[1].split_once('/') {
        let _ = year_text;
        return None;
    } else if parts[1].contains('/') {
        return None;
    } else {
        (Local::now().year(), 2)
    };
    let (hour, minute, second) = parse_hms(parts[time_index], false)?;
    let naive = if year == Local::now().year() {
        resolve_yearless(month, day, hour, minute, second)?
    } else {
        NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?
    };
    if parts.len() == 4 {
        fixed_timestamp(naive, parts[3])
    } else {
        local_timestamp(naive)
    }
}

fn parse_rfc_like(text: &str) -> Option<i64> {
    let mut parts: Vec<&str> = text.split_whitespace().collect();
    if parts
        .first()
        .is_some_and(|part| part.ends_with(',') || weekday(part).is_some())
    {
        parts.remove(0);
    }
    if !(4..=5).contains(&parts.len()) {
        return None;
    }
    let day = parts[0].parse().ok()?;
    let month = month_number(parts[1])?;
    let year = parse_year(parts[2])?;
    let (hour, minute, second) = parse_hms(parts[3], true)?;
    let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    if parts.len() == 5 {
        fixed_timestamp(naive, parts[4])
    } else {
        local_timestamp(naive)
    }
}

fn parse_lastlog(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() != 4 || weekday(parts[0]).is_none() {
        return None;
    }
    let month = month_number(parts[1])?;
    let day = parts[2].parse().ok()?;
    let (hour, minute, second) = parse_hms(parts[3], false)?;
    local_timestamp(resolve_yearless(month, day, hour, minute, second)?)
}

fn parse_hms(text: &str, require_seconds: bool) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() == 2 && !require_seconds {
        Some((parts[0].parse().ok()?, parts[1].parse().ok()?, 0))
    } else if parts.len() == 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

fn parse_year(text: &str) -> Option<i32> {
    let year: i32 = text.parse().ok()?;
    if text.len() <= 2 {
        if year >= 77 {
            Some(1900 + year)
        } else {
            Some(2000 + year)
        }
    } else {
        Some(year)
    }
}

fn month_number(text: &str) -> Option<u32> {
    match text.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn weekday(text: &str) -> Option<()> {
    match text
        .trim_end_matches(',')
        .get(..3)?
        .to_ascii_lowercase()
        .as_str()
    {
        "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun" => Some(()),
        _ => None,
    }
}

fn resolve_yearless(
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<NaiveDateTime> {
    let now = Local::now().naive_local();
    let year = Local::now().year();
    let this_year = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    if this_year > now {
        NaiveDate::from_ymd_opt(year - 1, month, day)?.and_hms_opt(hour, minute, second)
    } else {
        Some(this_year)
    }
}

fn local_timestamp(naive: NaiveDateTime) -> Option<i64> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.timestamp()),
        LocalResult::Ambiguous(dt, _) => Some(dt.timestamp()),
        LocalResult::None => None,
    }
}

fn fixed_timestamp(naive: NaiveDateTime, tz: &str) -> Option<i64> {
    let offset = match tz.to_ascii_uppercase().as_str() {
        "GMT" | "UTC" | "Z" => FixedOffset::east_opt(0)?,
        _ => parse_numeric_offset(tz)?,
    };
    Some(offset.from_local_datetime(&naive).single()?.timestamp())
}

fn parse_numeric_offset(tz: &str) -> Option<FixedOffset> {
    if tz.len() != 5 {
        return None;
    }
    let sign = match &tz[..1] {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let hours: i32 = tz[1..3].parse().ok()?;
    let mins: i32 = tz[3..5].parse().ok()?;
    FixedOffset::east_opt(sign * (hours * 3600 + mins * 60))
}

fn concise_ago(delta: i64) -> String {
    let (suffix, secs) = if delta >= 0 {
        ("ago", delta)
    } else {
        ("from now", -delta)
    };
    if secs == 0 {
        return "right now".to_string();
    }
    let mut values = [
        secs / (365 * 24 * 60 * 60),
        (secs % (365 * 24 * 60 * 60)) / (24 * 60 * 60),
        (secs % (24 * 60 * 60)) / (60 * 60),
        (secs % (60 * 60)) / 60,
        secs % 60,
    ];
    let limits = [i64::MAX, 365, 24, 60, 60];
    let labels = ["y", "d", "h", "m", "s"];

    let nonzero: Vec<usize> = values
        .iter()
        .enumerate()
        .filter_map(|(idx, value)| (*value > 0).then_some(idx))
        .collect();
    if nonzero.len() > 2 {
        let round_idx = nonzero[2];
        if values[round_idx] >= limits[round_idx] / 2 {
            values[nonzero[1]] += 1;
        }
        for idx in round_idx..values.len() {
            values[idx] = 0;
        }
        for idx in (1..values.len()).rev() {
            if values[idx] >= limits[idx] {
                values[idx - 1] += values[idx] / limits[idx];
                values[idx] %= limits[idx];
            }
        }
    }

    let mut rendered = String::new();
    let mut used = 0;
    for (value, label) in values.into_iter().zip(labels) {
        if value > 0 {
            rendered.push_str(&format!("{value}{label}"));
            used += 1;
        }
        if used == 2 {
            break;
        }
    }
    if rendered.is_empty() {
        rendered.push_str("0s");
    }
    format!("{rendered} {suffix}")
}
