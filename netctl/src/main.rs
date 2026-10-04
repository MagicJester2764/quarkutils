#![no_std]
#![no_main]

//! What the network is doing, and what of it is let in.
//!
//! ```text
//! netctl                     the card and lo, the ways out, the resolver,
//!                            every socket and the filter
//! netctl filter              the filter's rules
//! netctl filter drop|accept [tcp|udp|icmp|any] [port N[-M]] [from ADDRESS[/BITS]]
//! netctl filter remove N     take out rule N
//! netctl filter clear        take out every rule
//! ```
//!
//! A rule is about what comes in: a packet of a protocol, for one of this
//! machine's ports, from an address. The first rule that matches a packet
//! decides, and a packet none matches is let in; one dropped is dropped
//! without a word. Changing the filter is a capability (`NetAdmin`), which
//! a session holds if its account has the `network` right; anybody may see
//! it.

use quark_rt::manifest::CapReq;
use quark_rt::{args, nameserver, net, print, println, socket, syscall};

quark_rt::manifest!([CapReq::net_admin()]);

static mut TEXT: [u8; 16384] = [0; 16384];

fn usage() -> ! {
    println!("usage: netctl");
    println!("       netctl filter");
    println!("       netctl filter drop|accept [tcp|udp|icmp|any] [port N[-M]] [from ADDRESS[/BITS]]");
    println!("       netctl filter remove N");
    println!("       netctl filter clear");
    syscall::sys_exit_code(2);
}

fn net_tid() -> usize {
    match nameserver::lookup_retry(b"net", 50) {
        Some(tid) => tid,
        None => {
            println!("netctl: there is no network");
            syscall::sys_exit_code(1);
        }
    }
}

/// A number written in decimal digits and nothing else.
fn number(text: &[u8]) -> Option<u32> {
    if text.is_empty() || text.len() > 9 {
        return None;
    }
    text.iter().try_fold(0u32, |n, &b| b.is_ascii_digit().then(|| n * 10 + u32::from(b - b'0')))
}

/// An IPv6 address: groups of hexadecimal digits between colons, and at
/// most one `::` for as many noughts as it takes.
fn ipv6(text: &[u8]) -> Option<[u8; 16]> {
    let mut groups = [0u16; 8];
    let (head, tail) = match text.windows(2).position(|w| w == b"::") {
        Some(i) => (&text[..i], Some(&text[i + 2..])),
        None => (text, None),
    };
    let parse = |part: &[u8], out: &mut [u16]| -> Option<usize> {
        if part.is_empty() {
            return Some(0);
        }
        let mut n = 0;
        for g in part.split(|&b| b == b':') {
            if g.is_empty() || g.len() > 4 || n == out.len() {
                return None;
            }
            out[n] = g.iter().try_fold(0u16, |v, &b| (b as char).to_digit(16).map(|d| v << 4 | d as u16))?;
            n += 1;
        }
        Some(n)
    };
    let mut back = [0u16; 8];
    let h = parse(head, &mut groups)?;
    let t = match tail {
        Some(tail) => parse(tail, &mut back)?,
        None if h == 8 => 0,
        None => return None,
    };
    if h + t > if tail.is_some() { 7 } else { 8 } {
        return None;
    }
    groups[8 - t..].copy_from_slice(&back[..t]);
    let mut b = [0u8; 16];
    for (i, g) in groups.iter().enumerate() {
        b[i * 2..i * 2 + 2].copy_from_slice(&g.to_be_bytes());
    }
    Some(b)
}

/// An address and how many of its bits a packet's must share: all of them
/// unless it says.
fn prefix(text: &[u8]) -> Option<([u8; 16], u8, bool)> {
    let (addr, bits) = match text.iter().position(|&b| b == b'/') {
        Some(i) => (&text[..i], Some(number(&text[i + 1..])?)),
        None => (text, None),
    };
    if let Some(v4) = socket::parse_ipv4(addr) {
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&v4);
        let bits = bits.unwrap_or(32);
        return (bits <= 32).then_some((b, bits as u8, false));
    }
    let bits = bits.unwrap_or(128);
    (bits <= 128).then_some((ipv6(addr)?, bits as u8, true))
}

/// A rule out of the words after `drop` or `accept`.
fn rule(drop: bool) -> Option<net::Rule> {
    let mut r = net::Rule { drop, protocol: 0, ports: (0, u16::MAX), from: None };
    let mut i = 3;
    while let Some(word) = args::argv(i) {
        i += 1;
        match word {
            b"tcp" => r.protocol = 6,
            b"udp" => r.protocol = 17,
            b"icmp" => r.protocol = 1,
            b"any" => r.protocol = 0,
            b"port" | b"ports" => {
                let spec = args::argv(i)?;
                i += 1;
                let (first, last) = match spec.iter().position(|&b| b == b'-') {
                    Some(d) => (number(&spec[..d])?, number(&spec[d + 1..])?),
                    None => (number(spec)?, number(spec)?),
                };
                if first > last || last > u32::from(u16::MAX) {
                    return None;
                }
                r.ports = (first as u16, last as u16);
            }
            b"from" => {
                r.from = Some(prefix(args::argv(i)?)?);
                i += 1;
            }
            _ => return None,
        }
    }
    Some(r)
}

/// What the stack says it is doing; or only the filter.
fn show(filter_only: bool) -> ! {
    let text = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };
    let (n, _) = match net::status(net_tid(), text) {
        Ok(said) => said,
        Err(_) => {
            println!("netctl: the network did not say");
            syscall::sys_exit_code(1);
        }
    };
    let mut said = &text[..n];
    if filter_only {
        // From the line that begins it.
        if let Some(at) = said.windows(7).position(|w| w == b"filter ") {
            said = &said[at..];
        }
    }
    print!("{}", core::str::from_utf8(said).unwrap_or(""));
    syscall::sys_exit_code(0);
}

fn report(changed: Result<usize, u64>) -> ! {
    match changed {
        Ok(n) => {
            println!("netctl: {} rule{}", n, if n == 1 { "" } else { "s" });
            syscall::sys_exit_code(0);
        }
        Err(1) => {
            println!("netctl: this account may not change the filter");
            syscall::sys_exit_code(1);
        }
        Err(_) => {
            println!("netctl: the network would not do that");
            syscall::sys_exit_code(1);
        }
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    match args::argv(1) {
        None => show(false),
        Some(b"filter") => {}
        Some(_) => usage(),
    }
    match args::argv(2) {
        None => show(true),
        Some(b"clear") if args::argv(3).is_none() => report(net::filter_clear(net_tid())),
        Some(b"remove") => match args::argv(3).and_then(number) {
            Some(n) if args::argv(4).is_none() => report(net::filter_remove(net_tid(), n as usize)),
            _ => usage(),
        },
        Some(word @ (b"drop" | b"accept")) => match rule(word == b"drop") {
            Some(r) => report(net::filter_add(net_tid(), &r)),
            None => usage(),
        },
        Some(_) => usage(),
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::sys_exit_code(255);
}
