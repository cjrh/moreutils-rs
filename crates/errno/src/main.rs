// SPDX-License-Identifier: GPL-2.0-only

use std::env;
use std::ffi::{CStr, CString};
use std::io::{self, Write};
use std::process::Command;

macro_rules! errnos {
    ($( $name:ident ),+ $(,)?) => { &[ $( (stringify!($name), libc::$name as i32), )+ ] };
}

fn table() -> &'static [(&'static str, i32)] {
    errnos!(
        EPERM,
        ENOENT,
        ESRCH,
        EINTR,
        EIO,
        ENXIO,
        E2BIG,
        ENOEXEC,
        EBADF,
        ECHILD,
        EAGAIN,
        ENOMEM,
        EACCES,
        EFAULT,
        ENOTBLK,
        EBUSY,
        EEXIST,
        EXDEV,
        ENODEV,
        ENOTDIR,
        EISDIR,
        EINVAL,
        ENFILE,
        EMFILE,
        ENOTTY,
        ETXTBSY,
        EFBIG,
        ENOSPC,
        ESPIPE,
        EROFS,
        EMLINK,
        EPIPE,
        EDOM,
        ERANGE,
        EDEADLK,
        ENAMETOOLONG,
        ENOLCK,
        ENOSYS,
        ENOTEMPTY,
        ELOOP,
        EWOULDBLOCK,
        ENOMSG,
        EIDRM,
        ECHRNG,
        EL2NSYNC,
        EL3HLT,
        EL3RST,
        ELNRNG,
        EUNATCH,
        ENOCSI,
        EL2HLT,
        EBADE,
        EBADR,
        EXFULL,
        ENOANO,
        EBADRQC,
        EBADSLT,
        EDEADLOCK,
        EBFONT,
        ENOSTR,
        ENODATA,
        ETIME,
        ENOSR,
        ENONET,
        ENOPKG,
        EREMOTE,
        ENOLINK,
        EADV,
        ESRMNT,
        ECOMM,
        EPROTO,
        EMULTIHOP,
        EDOTDOT,
        EBADMSG,
        EOVERFLOW,
        ENOTUNIQ,
        EBADFD,
        EREMCHG,
        ELIBACC,
        ELIBBAD,
        ELIBSCN,
        ELIBMAX,
        ELIBEXEC,
        EILSEQ,
        ERESTART,
        ESTRPIPE,
        EUSERS,
        ENOTSOCK,
        EDESTADDRREQ,
        EMSGSIZE,
        EPROTOTYPE,
        ENOPROTOOPT,
        EPROTONOSUPPORT,
        ESOCKTNOSUPPORT,
        EOPNOTSUPP,
        EPFNOSUPPORT,
        EAFNOSUPPORT,
        EADDRINUSE,
        EADDRNOTAVAIL,
        ENETDOWN,
        ENETUNREACH,
        ENETRESET,
        ECONNABORTED,
        ECONNRESET,
        ENOBUFS,
        EISCONN,
        ENOTCONN,
        ESHUTDOWN,
        ETOOMANYREFS,
        ETIMEDOUT,
        ECONNREFUSED,
        EHOSTDOWN,
        EHOSTUNREACH,
        EALREADY,
        EINPROGRESS,
        ESTALE,
        EUCLEAN,
        ENOTNAM,
        ENAVAIL,
        EISNAM,
        EREMOTEIO,
        EDQUOT,
        ENOMEDIUM,
        EMEDIUMTYPE,
        ECANCELED,
        ENOKEY,
        EKEYEXPIRED,
        EKEYREVOKED,
        EKEYREJECTED,
        EOWNERDEAD,
        ENOTRECOVERABLE,
        ERFKILL,
        EHWPOISON,
        ENOTSUP
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Help,
    List,
    Search,
    SearchAllLocales,
}

fn usage() {
    println!("Usage: errno [-lsS] [--list] [--search] [--search-all-locales] [keyword]");
}

fn set_initial_locale() {
    let empty = c"";
    // SAFETY: `LC_ALL` is a valid C locale category and `empty` is a static,
    // NUL-terminated string borrowed for this call. `setlocale` changes
    // process-global C state, so this binary calls it once on its only Rust
    // thread before it performs locale-sensitive work or starts a child.
    // A failed locale initialization is intentionally ignored to match errno.
    unsafe {
        libc::setlocale(libc::LC_ALL, empty.as_ptr());
    }
}

fn set_locale(locale: &str) {
    if let Ok(locale) = CString::new(locale) {
        // SAFETY: `LC_ALL` is valid and `CString` guarantees a NUL-terminated
        // buffer that remains alive throughout the call. Locale changes are
        // serialized in `search_all_locales` on this single-threaded binary;
        // `setlocale`'s null result is intentionally ignored for unavailable
        // locales, matching errno's search-all-locales behavior.
        unsafe {
            libc::setlocale(libc::LC_ALL, locale.as_ptr());
        }
    }
}

fn desc_bytes(code: i32) -> Vec<u8> {
    // SAFETY: strerror accepts an errno value and returns either null or a
    // borrowed, NUL-terminated message in the currently selected C locale.
    // Its storage may be reused by later libc calls, so this single-threaded
    // program copies the bytes immediately and never frees or retains `p`.
    // A Rust String cannot represent legacy locale encodings losslessly; using
    // the C bytes is required for errno's byte-for-byte output contract.
    let p = unsafe { libc::strerror(code) };
    if p.is_null() {
        format!("Unknown error {code}").into_bytes()
    } else {
        // SAFETY: strerror's non-null result is a NUL-terminated C string
        // valid until the next libc call that reuses its storage. to_bytes
        // borrows it only long enough for to_vec to make an owned copy.
        unsafe { CStr::from_ptr(p) }.to_bytes().to_vec()
    }
}

fn print_one<W: Write>(out: &mut W, name: &str, code: i32) -> io::Result<()> {
    write!(out, "{name} {code} ")?;
    out.write_all(&desc_bytes(code))?;
    out.write_all(b"\n")
}

fn print_table() {
    let mut out = io::BufWriter::new(io::stdout().lock());
    for &(name, code) in table() {
        print_one(&mut out, name, code).unwrap();
    }
}

fn description_matches(code: i32, words: &[String]) -> bool {
    let desc = String::from_utf8_lossy(&desc_bytes(code)).to_lowercase();
    words.iter().all(|word| desc.contains(word))
}

fn search_current_locale(words: &[String]) {
    let mut out = io::BufWriter::new(io::stdout().lock());
    for &(name, code) in table() {
        if description_matches(code, words) {
            print_one(&mut out, name, code).unwrap();
        }
    }
}

fn search_all_locales(words: &[String]) {
    let locales = Command::new("locale")
        .arg("-a")
        .output()
        .unwrap_or_else(|err| {
            eprintln!(
                "ERROR: Can't execute locale -a: {}: {}",
                err.raw_os_error().unwrap_or(0),
                err
            );
            std::process::exit(1);
        });
    let mut out = io::BufWriter::new(io::stdout().lock());
    for locale in String::from_utf8_lossy(&locales.stdout).lines() {
        set_locale(locale);
        for &(name, code) in table() {
            if description_matches(code, words) {
                print_one(&mut out, name, code).unwrap();
            }
        }
    }
}

fn lookup(arg: &str, out: &mut impl Write) -> bool {
    if let Some(&(name, code)) = table()
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(arg))
    {
        print_one(out, name, code).unwrap();
        return true;
    }

    if arg.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) {
        let digits: String = arg.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = digits.parse::<i64>() {
            if let Some(&(name, _)) = table().iter().find(|(_, c)| i64::from(*c) == code) {
                print_one(out, name, code as i32).unwrap();
                return true;
            }
        }
        return false;
    }

    if arg.starts_with('E') || arg.starts_with('e') {
        return false;
    }

    eprintln!("ERROR: Not understood: {arg}");
    false
}

fn parse_args(args: &[String]) -> (Option<Action>, Vec<String>) {
    let mut action = None;
    let mut operands = Vec::new();
    let mut end_options = false;

    for arg in args {
        if end_options {
            operands.push(arg.clone());
            continue;
        }
        if arg == "--" {
            end_options = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if long.contains('=') {
                let name = long.split_once('=').map(|(name, _)| name).unwrap_or(long);
                match name {
                    "help" | "list" | "search" | "search-all-locales" => {
                        eprintln!("/bin/errno: option '--{name}' doesn't allow an argument");
                    }
                    _ => eprintln!("/bin/errno: unrecognized option '--{long}'"),
                }
                continue;
            }
            match long {
                "help" => action = Some(Action::Help),
                "list" => action = Some(Action::List),
                "search" => action = Some(Action::Search),
                "search-all-locales" => action = Some(Action::SearchAllLocales),
                _ => eprintln!("/bin/errno: unrecognized option '--{long}'"),
            }
            continue;
        }
        if arg.starts_with('-') && arg != "-" {
            for ch in arg.chars().skip(1) {
                match ch {
                    'h' => action = Some(Action::Help),
                    'l' => action = Some(Action::List),
                    's' => action = Some(Action::Search),
                    'S' => action = Some(Action::SearchAllLocales),
                    _ => eprintln!("/bin/errno: invalid option -- '{ch}'"),
                }
            }
            continue;
        }
        operands.push(arg.clone());
    }

    (action, operands)
}

fn main() {
    set_initial_locale();
    let args: Vec<String> = env::args().skip(1).collect();
    let (action, operands) = parse_args(&args);

    match action {
        Some(Action::Help) => usage(),
        Some(Action::List) => print_table(),
        Some(Action::Search) => {
            let words: Vec<String> = operands.iter().map(|s| s.to_lowercase()).collect();
            search_current_locale(&words);
        }
        Some(Action::SearchAllLocales) => {
            let words: Vec<String> = operands.iter().map(|s| s.to_lowercase()).collect();
            search_all_locales(&words);
        }
        None => {
            let mut failed = false;
            let mut out = io::BufWriter::new(io::stdout().lock());
            for arg in operands {
                if !lookup(&arg, &mut out) {
                    failed = true;
                }
            }
            out.flush().unwrap();
            if failed {
                std::process::exit(1);
            }
        }
    }
}
