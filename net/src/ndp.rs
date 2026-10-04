//! IPv6 on the card: an address of its own, and the router's.
//!
//! smoltcp answers neighbour solicitations and echoes and does not listen
//! to routers. What a router advertises is read here, from a raw socket that
//! sees every ICMPv6 packet the card receives while smoltcp's own handling
//! of them goes on beside it:
//!
//! - an address on the link, from the card's: fe80::/64 and its EUI-64;
//! - a router solicitation to every router, three times four seconds apart,
//!   until one has answered (RFC 4861);
//! - from an advertisement, a default route by the router that said it, for
//!   as long as it said; an address on every prefix that may be configured
//!   so — the prefix and the card's EUI-64 (RFC 4862); and the DNS servers
//!   it named (RFC 8106). An address, a route and a server each go when
//!   what was said of them runs out.
//!
//! An address is not first checked for a duplicate on the link: the
//! card's own number is in it.

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::raw;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{
    EthernetAddress, Icmpv6Packet, Icmpv6Repr, IpAddress, IpCidr, IpProtocol, IpVersion, Ipv6Address, Ipv6Cidr,
    Ipv6Packet, Ipv6Repr, NdiscRepr, RawHardwareAddress,
};

/// Solicitations sent before giving up on a router answering, and how far
/// apart (RFC 4861's MAX_RTR_SOLICITATIONS and RTR_SOLICITATION_INTERVAL).
const SOLICITATIONS: u32 = 3;
const SOLICIT_EVERY: Duration = Duration::from_secs(4);
/// ff02::2, every router on the link.
const ALL_ROUTERS: Ipv6Address = Ipv6Address::new(0xff02, 0, 0, 0, 0, 0, 0, 2);
/// What a router advertisement is, and its options of interest.
const ROUTER_ADVERT: u8 = 134;
const OPT_PREFIX: u8 = 3;
const OPT_RDNSS: u8 = 25;
/// A prefix's flags: on the link, and an address may be made on it.
const PREFIX_AUTONOMOUS: u8 = 0x40;
/// The most DNS servers kept from advertisements.
const MAX_SERVERS: usize = 3;

/// The card's number, made into the low half of an IPv6 address
/// (modified EUI-64): its first byte's universal bit turned over, and
/// ff:fe in the middle.
fn interface_id(mac: [u8; 6]) -> [u8; 8] {
    [mac[0] ^ 0x02, mac[1], mac[2], 0xff, 0xfe, mac[3], mac[4], mac[5]]
}

fn with_id(prefix: Ipv6Address, mac: [u8; 6]) -> Ipv6Address {
    let mut b = prefix.octets();
    b[8..].copy_from_slice(&interface_id(mac));
    Ipv6Address::from(b)
}

fn be16(b: &[u8]) -> u32 {
    u32::from(b[0]) << 8 | u32::from(b[1])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// What an advertisement said.
struct Advert {
    router: Ipv6Address,
    /// For how long the router is a default route; nought for not.
    lifetime: Duration,
    /// Prefixes an address may be made on, and for how long it is valid.
    prefixes: Vec<(Ipv6Address, Duration)>,
    servers: Vec<(Ipv6Address, Duration)>,
}

/// An advertisement out of an IPv6 packet as the raw socket gives it, if it
/// is one and may be believed: from a router's address on the link, sent no
/// further than the link (a hop limit of 255, which no router passing it on
/// leaves), with a checksum that holds.
fn advert(packet: &[u8]) -> Option<Advert> {
    let ip = Ipv6Packet::new_checked(packet).ok()?;
    let (src, dst) = (ip.src_addr(), ip.dst_addr());
    if ip.hop_limit() != 255 || !src.is_unicast_link_local() || ip.next_header() != IpProtocol::Icmpv6 {
        return None;
    }
    let icmp = Icmpv6Packet::new_checked(ip.payload()).ok()?;
    if !icmp.verify_checksum(&src, &dst) {
        return None;
    }
    let b = ip.payload();
    if b.len() < 16 || b[0] != ROUTER_ADVERT || b[1] != 0 {
        return None;
    }
    let mut a = Advert {
        router: src,
        lifetime: Duration::from_secs(u64::from(be16(&b[6..8]))),
        prefixes: Vec::new(),
        servers: Vec::new(),
    };
    let mut opts = &b[16..];
    while opts.len() >= 2 {
        let len = usize::from(opts[1]) * 8;
        if len == 0 || len > opts.len() {
            break;
        }
        let o = &opts[..len];
        match o[0] {
            OPT_PREFIX if len == 32 => {
                let mut p = [0u8; 16];
                p.copy_from_slice(&o[16..32]);
                let prefix = Ipv6Address::from(p);
                if o[2] == 64 && o[3] & PREFIX_AUTONOMOUS != 0 && !prefix.is_unicast_link_local() {
                    a.prefixes.push((prefix, Duration::from_secs(u64::from(be32(&o[4..8])))));
                }
            }
            OPT_RDNSS if len >= 24 => {
                let life = Duration::from_secs(u64::from(be32(&o[4..8])));
                for s in o[8..].chunks_exact(16) {
                    let mut p = [0u8; 16];
                    p.copy_from_slice(s);
                    a.servers.push((Ipv6Address::from(p), life));
                }
            }
            _ => {}
        }
        opts = &opts[len..];
    }
    Some(a)
}

pub struct Ndp {
    raw: SocketHandle,
    mac: [u8; 6],
    link_local: Ipv6Address,
    solicited: u32,
    next_solicit: Instant,
    /// A router has answered.
    heard: bool,
    /// Addresses made on prefixes, and when each stops being valid.
    addresses: Vec<(Ipv6Address, Instant)>,
    router: Option<(Ipv6Address, Instant)>,
    pub servers: Vec<(Ipv6Address, Instant)>,
}

impl Ndp {
    /// The card's address on the link, and a socket that hears routers.
    pub fn new(eth: &mut Interface, sockets: &mut SocketSet<'static>, mac: [u8; 6], now: Instant) -> Ndp {
        let link_local = with_id(Ipv6Address::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), mac);
        eth.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(link_local, 64)));
        });
        let raw = raw::Socket::new(
            IpVersion::Ipv6,
            IpProtocol::Icmpv6,
            raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 8], vec![0; 4096]),
            raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 2], vec![0; 256]),
        );
        Ndp {
            raw: sockets.add(raw),
            mac,
            link_local,
            solicited: 0,
            next_solicit: now,
            heard: false,
            addresses: Vec::new(),
            router: None,
            servers: Vec::new(),
        }
    }

    /// Ask every router to say what it has, if it is time to.
    fn solicit(&mut self, sockets: &mut SocketSet<'static>, now: Instant) {
        if self.heard || self.solicited >= SOLICITATIONS || now < self.next_solicit {
            return;
        }
        self.solicited += 1;
        self.next_solicit = now + SOLICIT_EVERY;
        let rs = Icmpv6Repr::Ndisc(NdiscRepr::RouterSolicit {
            lladdr: Some(RawHardwareAddress::from(EthernetAddress(self.mac))),
        });
        let ip = Ipv6Repr {
            src_addr: self.link_local,
            dst_addr: ALL_ROUTERS,
            next_header: IpProtocol::Icmpv6,
            payload_len: rs.buffer_len(),
            hop_limit: 255,
        };
        let mut packet = vec![0u8; ip.buffer_len() + rs.buffer_len()];
        let mut p = Ipv6Packet::new_unchecked(&mut packet[..]);
        ip.emit(&mut p);
        rs.emit(
            &ip.src_addr,
            &ip.dst_addr,
            &mut Icmpv6Packet::new_unchecked(p.payload_mut()),
            &ChecksumCapabilities::default(),
        );
        let _ = sockets.get_mut::<raw::Socket>(self.raw).send_slice(&packet);
    }

    /// Hear routers, and do what they said: addresses for `eth` — and for
    /// `lo`, so this machine reaches itself by them — a default route, DNS
    /// servers. Then what has run out goes.
    pub fn turn(&mut self, eth: &mut Interface, lo: &mut Interface, sockets: &mut SocketSet<'static>, now: Instant) {
        self.solicit(sockets, now);
        let mut adverts = Vec::new();
        {
            let raw = sockets.get_mut::<raw::Socket>(self.raw);
            while let Ok(packet) = raw.recv() {
                if let Some(a) = advert(packet) {
                    adverts.push(a);
                }
            }
        }
        for a in adverts {
            self.heard = true;
            // The router, as a way out.
            if a.lifetime > Duration::ZERO {
                let _ = eth.routes_mut().add_default_ipv6_route(a.router);
                self.router = Some((a.router, now + a.lifetime));
            } else if self.router.is_some_and(|(r, _)| r == a.router) {
                eth.routes_mut().remove_default_ipv6_route();
                self.router = None;
            }
            // An address on each prefix.
            for (prefix, valid) in a.prefixes {
                let addr = with_id(prefix, self.mac);
                let known = self.addresses.iter().position(|&(x, _)| x == addr);
                match (known, valid > Duration::ZERO) {
                    (Some(k), true) => self.addresses[k].1 = now + valid,
                    (None, true) => {
                        eth.update_ip_addrs(|addrs| {
                            let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(addr, 64)));
                        });
                        lo.update_ip_addrs(|addrs| {
                            let _ = addrs.push(IpCidr::Ipv6(Ipv6Cidr::new(addr, 128)));
                        });
                        self.addresses.push((addr, now + valid));
                    }
                    (Some(_), false) => self.forget(eth, lo, addr),
                    (None, false) => {}
                }
            }
            for (server, life) in a.servers {
                let known = self.servers.iter().position(|&(s, _)| s == server);
                match (known, life > Duration::ZERO) {
                    (Some(k), true) => self.servers[k].1 = now + life,
                    (None, true) if self.servers.len() < MAX_SERVERS => self.servers.push((server, now + life)),
                    (Some(k), false) => {
                        self.servers.remove(k);
                    }
                    _ => {}
                }
            }
        }
        // What has run out.
        if self.router.is_some_and(|(_, until)| now >= until) {
            eth.routes_mut().remove_default_ipv6_route();
            self.router = None;
        }
        let gone: Vec<Ipv6Address> = self.addresses.iter().filter(|&&(_, until)| now >= until).map(|&(a, _)| a).collect();
        for addr in gone {
            self.forget(eth, lo, addr);
        }
        self.servers.retain(|&(_, until)| now < until);
    }

    fn forget(&mut self, eth: &mut Interface, lo: &mut Interface, addr: Ipv6Address) {
        eth.update_ip_addrs(|addrs| addrs.retain(|a| a.address() != IpAddress::Ipv6(addr)));
        lo.update_ip_addrs(|addrs| addrs.retain(|a| a.address() != IpAddress::Ipv6(addr)));
        self.addresses.retain(|&(a, _)| a != addr);
    }
}
