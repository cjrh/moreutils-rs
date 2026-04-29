// SPDX-License-Identifier: GPL-3.0-or-later

use std::env;
use std::ffi::CStr;
use std::fs;
use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;

const COMMANDS: &[(&str, &str)] = &[
    ("-e", "Reports interface existence via return code"),
    ("-p", "Print out the whole config of iface"),
    ("-pe", "Print out yes or no according to existence"),
    ("-pa", "Print out the address"),
    ("-pn", "Print netmask"),
    ("-pN", "Print network address"),
    ("-pb", "Print broadcast"),
    ("-pm", "Print mtu"),
    ("-ph", "Print out the hardware address"),
    ("-pf", "Print flags"),
    ("-si", "Print all statistics on input"),
    ("-sip", "Print # of in packets"),
    ("-sib", "Print # of in bytes"),
    ("-sie", "Print # of in errors"),
    ("-sid", "Print # of in drops"),
    ("-sif", "Print # of in fifo overruns"),
    ("-sic", "Print # of in compress"),
    ("-sim", "Print # of in multicast"),
    ("-so", "Print all statistics on output"),
    ("-sop", "Print # of out packets"),
    ("-sob", "Print # of out bytes"),
    ("-soe", "Print # of out errors"),
    ("-sod", "Print # of out drops"),
    ("-sof", "Print # of out fifo overruns"),
    ("-sox", "Print # of out collisions"),
    ("-soc", "Print # of out carrier loss"),
    ("-som", "Print # of out multicast"),
    ("-bips", "Print # of incoming bytes per second"),
    ("-bops", "Print # of outgoing bytes per second"),
];

struct Socket(RawFd);

impl Socket {
    fn new() -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(fd))
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

fn usage() -> ! {
    eprintln!("Usage: /bin/ifdata [options] iface");
    for (opt, desc) in COMMANDS {
        eprintln!("  {opt:>5}   {desc}");
    }
    std::process::exit(1)
}

fn fail() -> ! {
    std::process::exit(1)
}

fn iface_not_found(iface: &str) -> ! {
    eprintln!("No such network interface: {iface}");
    std::process::exit(1)
}

fn stack_smash_abort() -> ! {
    eprintln!("*** stack smashing detected ***: terminated");
    unsafe {
        libc::raise(libc::SIGABRT);
    }
    std::process::exit(134)
}

fn is_stats_or_rate(opt: &str) -> bool {
    opt.starts_with("-si") || opt.starts_with("-so") || opt == "-bips" || opt == "-bops"
}

fn ifreq_for(iface: &str) -> libc::ifreq {
    let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
    for (dst, src) in ifr.ifr_name.iter_mut().zip(iface.bytes()) {
        *dst = src as libc::c_char;
    }
    ifr
}

fn ioctl_ifreq(fd: RawFd, iface: &str, request: libc::Ioctl) -> io::Result<libc::ifreq> {
    let mut ifr = ifreq_for(iface);
    let rc = unsafe { libc::ioctl(fd, request, &mut ifr) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ifr)
    }
}

fn exists(fd: RawFd, iface: &str) -> bool {
    ioctl_ifreq(fd, iface, libc::SIOCGIFFLAGS).is_ok()
}

fn sockaddr_to_v4(sockaddr: libc::sockaddr) -> Option<Ipv4Addr> {
    if i32::from(sockaddr.sa_family) != libc::AF_INET {
        return None;
    }
    let sin: libc::sockaddr_in = unsafe { mem::transmute(sockaddr) };
    Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
}

fn ioctl_addr(fd: RawFd, iface: &str, request: libc::Ioctl) -> Option<Ipv4Addr> {
    let ifr = ioctl_ifreq(fd, iface, request).ok()?;
    let sockaddr = unsafe { ifr.ifr_ifru.ifru_addr };
    sockaddr_to_v4(sockaddr)
}

fn addr(fd: RawFd, iface: &str) -> Option<Ipv4Addr> {
    ioctl_addr(fd, iface, libc::SIOCGIFADDR)
}

fn netmask(fd: RawFd, iface: &str) -> Option<Ipv4Addr> {
    ioctl_addr(fd, iface, libc::SIOCGIFNETMASK)
}

fn broadcast(fd: RawFd, iface: &str) -> Option<Ipv4Addr> {
    ioctl_addr(fd, iface, libc::SIOCGIFBRDADDR)
}

fn mtu(fd: RawFd, iface: &str) -> Option<i32> {
    let ifr = ioctl_ifreq(fd, iface, libc::SIOCGIFMTU).ok()?;
    Some(unsafe { ifr.ifr_ifru.ifru_mtu })
}

fn flags(fd: RawFd, iface: &str) -> Option<i16> {
    let ifr = ioctl_ifreq(fd, iface, libc::SIOCGIFFLAGS).ok()?;
    Some(unsafe { ifr.ifr_ifru.ifru_flags })
}

fn hwaddr(fd: RawFd, iface: &str) -> Option<[u8; 6]> {
    let ifr = ioctl_ifreq(fd, iface, libc::SIOCGIFHWADDR).ok()?;
    let sockaddr = unsafe { ifr.ifr_ifru.ifru_hwaddr };
    let mut out = [0u8; 6];
    for (dst, src) in out.iter_mut().zip(sockaddr.sa_data.iter()) {
        *dst = *src as u8;
    }
    Some(out)
}

fn network(addr: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(addr) & u32::from(mask))
}

fn stats(iface: &str) -> Option<([u64; 8], [u64; 8])> {
    let text = fs::read_to_string("/proc/net/dev").unwrap_or_else(|e| {
        eprintln!("fopen(\"/proc/net/dev\"): {}", os_error_message(&e));
        std::process::exit(1);
    });
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == iface {
            let vals: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if vals.len() >= 16 {
                return Some((vals[0..8].try_into().ok()?, vals[8..16].try_into().ok()?));
            }
            eprintln!("Invalid data read, check!");
            std::process::exit(1);
        }
    }
    None
}

fn os_error_message(err: &io::Error) -> String {
    if let Some(code) = err.raw_os_error() {
        unsafe {
            return CStr::from_ptr(libc::strerror(code))
                .to_string_lossy()
                .into_owned();
        }
    }
    err.to_string()
}

fn print_flags(bits: i16) {
    let bits = bits as u32;
    let names = [
        (0x1, "Up"),
        (0x2, "Broadcast"),
        (0x4, "Debugging"),
        (0x8, "Loopback"),
        (0x10, "Ppp"),
        (0x20, "No-trailers"),
        (0x40, "Running"),
        (0x80, "No-arp"),
        (0x100, "Promiscuous"),
        (0x200, "All-multicast"),
        (0x400, "Load-master"),
        (0x800, "Load-slave"),
        (0x1000, "Multicast"),
        (0x2000, "Port-select"),
        (0x4000, "Auto-detect"),
        (0x8000, "Dynaddr"),
        (0x10000, "Unknown-flags"),
    ];
    for (bit, name) in names {
        println!("{}{}", if bits & bit != 0 { "On  " } else { "Off " }, name);
    }
}

fn run_command(fd: RawFd, opt: &str, iface: &str) {
    let ex = exists(fd, iface);
    match opt {
        "-e" => {
            std::process::exit(if ex { 0 } else { 1 });
        }
        "-pe" => {
            println!("{}", if ex { "yes" } else { "no" });
            return;
        }
        _ if !ex && is_stats_or_rate(opt) => stack_smash_abort(),
        _ if !ex => iface_not_found(iface),
        _ => {}
    }

    match opt {
        "-p" => match (addr(fd, iface), netmask(fd, iface), mtu(fd, iface)) {
            (Some(a), Some(n), Some(m)) => println!(
                "{} {} {} {}",
                a,
                n,
                broadcast(fd, iface).unwrap_or(Ipv4Addr::UNSPECIFIED),
                m
            ),
            _ => println!("NON-IP"),
        },
        "-pa" => addr(fd, iface)
            .map(|x| println!("{x}"))
            .unwrap_or_else(|| fail()),
        "-pn" => netmask(fd, iface)
            .map(|x| println!("{x}"))
            .unwrap_or_else(|| fail()),
        "-pN" => match (addr(fd, iface), netmask(fd, iface)) {
            (Some(a), Some(n)) => println!("{}", network(a, n)),
            _ => fail(),
        },
        "-pb" => println!("{}", broadcast(fd, iface).unwrap_or(Ipv4Addr::UNSPECIFIED)),
        "-pm" => mtu(fd, iface)
            .map(|x| println!("{x}"))
            .unwrap_or_else(|| fail()),
        "-ph" => {
            let mac = hwaddr(fd, iface).unwrap_or([0; 6]);
            if mac == [0; 6] {
                eprintln!("Error: {iface}: no hardware address");
                fail();
            }
            println!(
                "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );
        }
        "-pf" => flags(fd, iface).map(print_flags).unwrap_or_else(|| fail()),
        "-si" => stats(iface)
            .map(|(rx, _)| {
                println!(
                    "{} {} {} {} {} {} {} {}",
                    rx[0], rx[1], rx[2], rx[3], rx[4], rx[5], rx[6], rx[7]
                )
            })
            .unwrap_or_else(|| fail()),
        "-so" => stats(iface)
            .map(|(_, tx)| {
                println!(
                    "{} {} {} {} {} {} {} {}",
                    tx[0], tx[1], tx[2], tx[3], tx[4], tx[5], tx[6], tx[7]
                )
            })
            .unwrap_or_else(|| fail()),
        "-sip" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[1]))
            .unwrap_or_else(|| fail()),
        "-sib" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[0]))
            .unwrap_or_else(|| fail()),
        "-sie" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[2]))
            .unwrap_or_else(|| fail()),
        "-sid" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[3]))
            .unwrap_or_else(|| fail()),
        "-sif" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[4]))
            .unwrap_or_else(|| fail()),
        "-sic" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[6]))
            .unwrap_or_else(|| fail()),
        "-sim" => stats(iface)
            .map(|(rx, _)| println!("{}", rx[7]))
            .unwrap_or_else(|| fail()),
        "-sop" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[1]))
            .unwrap_or_else(|| fail()),
        "-sob" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[0]))
            .unwrap_or_else(|| fail()),
        "-soe" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[2]))
            .unwrap_or_else(|| fail()),
        "-sod" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[3]))
            .unwrap_or_else(|| fail()),
        "-sof" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[4]))
            .unwrap_or_else(|| fail()),
        "-sox" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[5]))
            .unwrap_or_else(|| fail()),
        "-soc" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[6]))
            .unwrap_or_else(|| fail()),
        "-som" => stats(iface)
            .map(|(_, tx)| println!("{}", tx[7]))
            .unwrap_or_else(|| fail()),
        "-bips" | "-bops" => {
            let before = stats(iface).unwrap_or_else(|| fail());
            std::thread::sleep(std::time::Duration::from_secs(1));
            let after = stats(iface).unwrap_or_else(|| fail());
            if opt == "-bips" {
                println!("{}", after.0[0].saturating_sub(before.0[0]));
            } else {
                println!("{}", after.1[0].saturating_sub(before.1[0]));
            }
        }
        _ => usage(),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    if args.len() == 1 && !args[0].starts_with('-') {
        return;
    }
    if args.len() < 2 {
        usage();
    }

    let iface = args.last().unwrap();
    let opts = &args[..args.len() - 1];
    if opts.is_empty()
        || opts
            .iter()
            .any(|opt| !COMMANDS.iter().any(|(cmd, _)| cmd == opt))
    {
        usage();
    }

    let sock = Socket::new().unwrap_or_else(|err| {
        eprintln!("socket: {}", os_error_message(&err));
        std::process::exit(1);
    });
    for opt in opts {
        run_command(sock.0, opt, iface);
    }
}
