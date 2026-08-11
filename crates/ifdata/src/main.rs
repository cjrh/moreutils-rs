// SPDX-License-Identifier: GPL-2.0-only

use cjrh_moreutils_common::plain_os_error;
use nix::ifaddrs::{InterfaceAddress, getifaddrs};
use nix::sys::socket::{AddressFamily, SockFlag, SockType, SockaddrStorage, socket};
use std::env;
use std::fs;
use std::io;
use std::mem;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

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

fn stats_not_found(iface: &str) -> ! {
    eprintln!("Error getting statistics for {iface}");
    std::process::exit(1)
}

fn is_stats_or_rate(opt: &str) -> bool {
    opt.starts_with("-si") || opt.starts_with("-so") || opt == "-bips" || opt == "-bops"
}

struct IoctlSocket(OwnedFd);

impl IoctlSocket {
    fn new() -> io::Result<Self> {
        socket(
            AddressFamily::Inet,
            SockType::Datagram,
            SockFlag::empty(),
            None,
        )
        .map(Self)
        .map_err(io::Error::from)
    }
}

fn interfaces_for(iface: &str) -> io::Result<Vec<InterfaceAddress>> {
    getifaddrs().map_err(io::Error::from).map(|interfaces| {
        interfaces
            .filter(|interface| interface.interface_name == iface)
            .collect()
    })
}

fn ifreq_for(iface: &str) -> libc::ifreq {
    // SAFETY: an all-zero ifreq is a valid starting value for these getter
    // ioctls. The interface name is copied into its fixed-size buffer below.
    let mut ifreq: libc::ifreq = unsafe { mem::zeroed() };
    for (dst, src) in ifreq.ifr_name.iter_mut().zip(iface.bytes()) {
        *dst = src as libc::c_char;
    }
    ifreq
}

fn ioctl_ifreq(fd: RawFd, iface: &str, request: libc::Ioctl) -> io::Result<libc::ifreq> {
    let mut ifreq = ifreq_for(iface);
    // SAFETY: fd is a live AF_INET datagram socket; request is one of the
    // SIOCGIF* getter requests used below; and ifreq points to writable storage
    // initialized with a bounded interface name.
    let result = unsafe { libc::ioctl(fd, request, &mut ifreq) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ifreq)
    }
}

fn exists(iface: &str) -> bool {
    interfaces_for(iface).is_ok_and(|interfaces| !interfaces.is_empty())
}

fn ipv4(address: &SockaddrStorage) -> Option<Ipv4Addr> {
    let address: SocketAddrV4 = (*address.as_sockaddr_in()?).into();
    Some(*address.ip())
}

fn addr(iface: &str) -> Option<Ipv4Addr> {
    interfaces_for(iface)
        .ok()?
        .iter()
        .find_map(|interface| interface.address.as_ref().and_then(ipv4))
}

fn netmask(iface: &str) -> Option<Ipv4Addr> {
    interfaces_for(iface)
        .ok()?
        .iter()
        .find_map(|interface| interface.netmask.as_ref().and_then(ipv4))
}

fn sockaddr_to_v4(sockaddr: libc::sockaddr) -> Option<Ipv4Addr> {
    if i32::from(sockaddr.sa_family) != libc::AF_INET {
        return None;
    }
    // SAFETY: SIOCGIF*ADDR populated this sockaddr with an AF_INET
    // sockaddr_in. Both structures have the platform ABI layout expected by
    // the ioctl, and the family check above verifies the active representation.
    let address: libc::sockaddr_in = unsafe { mem::transmute(sockaddr) };
    Some(Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)))
}

fn broadcast(fd: RawFd, iface: &str) -> Option<Ipv4Addr> {
    let ifreq = ioctl_ifreq(fd, iface, libc::SIOCGIFBRDADDR as libc::Ioctl).ok()?;
    // SAFETY: a successful SIOCGIFBRDADDR initializes the address member of
    // ifr_ifru, which is copied immediately and interpreted above.
    let sockaddr = unsafe { ifreq.ifr_ifru.ifru_addr };
    sockaddr_to_v4(sockaddr)
}

fn mtu(fd: RawFd, iface: &str) -> Option<i32> {
    let ifreq = ioctl_ifreq(fd, iface, libc::SIOCGIFMTU as libc::Ioctl).ok()?;
    // SAFETY: a successful SIOCGIFMTU initializes the MTU member of ifr_ifru.
    Some(unsafe { ifreq.ifr_ifru.ifru_mtu })
}

fn flags(iface: &str) -> Option<i16> {
    interfaces_for(iface)
        .ok()?
        .first()
        .map(|interface| interface.flags.bits() as i16)
}

fn hwaddr(fd: RawFd, iface: &str) -> Option<[u8; 6]> {
    let ifreq = ioctl_ifreq(fd, iface, libc::SIOCGIFHWADDR as libc::Ioctl).ok()?;
    // SAFETY: a successful SIOCGIFHWADDR initializes the hardware-address
    // sockaddr member of ifr_ifru. Linux stores the address bytes in sa_data.
    let sockaddr = unsafe { ifreq.ifr_ifru.ifru_hwaddr };
    let mut address = [0; 6];
    for (dst, src) in address.iter_mut().zip(sockaddr.sa_data) {
        *dst = src as u8;
    }
    Some(address)
}

fn network(addr: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(addr) & u32::from(mask))
}

fn ipv4_text(address: Option<Ipv4Addr>) -> String {
    address.map_or_else(|| "NON-IP".to_owned(), |address| address.to_string())
}

fn network_text(address: Option<Ipv4Addr>, netmask: Option<Ipv4Addr>) -> String {
    match (address, netmask) {
        (Some(address), Some(netmask)) => network(address, netmask).to_string(),
        _ => "NON-IP".to_owned(),
    }
}

fn broadcast_text(address: Option<Ipv4Addr>, broadcast: Option<Ipv4Addr>) -> String {
    if address.is_some() {
        broadcast.unwrap_or(Ipv4Addr::UNSPECIFIED).to_string()
    } else {
        "NON-IP".to_owned()
    }
}

fn config_text(
    address: Option<Ipv4Addr>,
    netmask: Option<Ipv4Addr>,
    broadcast: Option<Ipv4Addr>,
    mtu: i32,
) -> String {
    format!(
        "{} {} {} {}",
        ipv4_text(address),
        ipv4_text(netmask),
        broadcast_text(address, broadcast),
        mtu
    )
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
    plain_os_error(err)
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
    let ex = exists(iface);
    match opt {
        "-e" => {
            std::process::exit(if ex { 0 } else { 1 });
        }
        "-pe" => {
            println!("{}", if ex { "yes" } else { "no" });
            return;
        }
        _ if !ex && is_stats_or_rate(opt) => stats_not_found(iface),
        _ if !ex => iface_not_found(iface),
        _ => {}
    }

    match opt {
        "-p" => match mtu(fd, iface) {
            Some(mtu) => println!(
                "{}",
                config_text(addr(iface), netmask(iface), broadcast(fd, iface), mtu)
            ),
            None => println!("NON-IP"),
        },
        "-pa" => println!("{}", ipv4_text(addr(iface))),
        "-pn" => println!("{}", ipv4_text(netmask(iface))),
        "-pN" => println!("{}", network_text(addr(iface), netmask(iface))),
        "-pb" => println!("{}", broadcast_text(addr(iface), broadcast(fd, iface))),
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
        "-pf" => flags(iface).map(print_flags).unwrap_or_else(|| fail()),
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

    let socket = IoctlSocket::new().unwrap_or_else(|err| {
        eprintln!("socket: {}", os_error_message(&err));
        std::process::exit(1);
    });

    for opt in opts {
        run_command(socket.0.as_raw_fd(), opt, iface);
    }
}

#[cfg(test)]
mod tests {
    use super::{broadcast_text, config_text, ipv4_text, network_text};
    use std::net::Ipv4Addr;

    #[test]
    fn existing_non_ip_interface_values_are_rendered_as_non_ip() {
        assert_eq!(ipv4_text(None), "NON-IP");
        assert_eq!(network_text(None, None), "NON-IP");
        assert_eq!(broadcast_text(None, None), "NON-IP");
        assert_eq!(
            config_text(None, None, None, 1400),
            "NON-IP NON-IP NON-IP 1400"
        );
    }

    #[test]
    fn ipv4_values_and_missing_broadcast_are_rendered() {
        let address = Some(Ipv4Addr::new(10, 23, 4, 5));
        let netmask = Some(Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(ipv4_text(address), "10.23.4.5");
        assert_eq!(network_text(address, netmask), "10.23.4.0");
        assert_eq!(broadcast_text(address, None), "0.0.0.0");
        assert_eq!(
            config_text(address, netmask, None, 1400),
            "10.23.4.5 255.255.255.0 0.0.0.0 1400"
        );
    }
}
