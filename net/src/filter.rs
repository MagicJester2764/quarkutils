//! What comes in, and what of it is let in.
//!
//! A list of rules, the first that matches a packet deciding, and a packet
//! no rule matches let in. A rule says what it does — let in, or drop
//! without a word — and what it is about: a protocol, a range of this
//! machine's ports a packet is for, and the addresses it may come from.
//! Every packet the card brings and every packet `lo` carries is asked
//! about before smoltcp sees it, so a rule about a port is about a
//! connection to it from anywhere, this machine included.
//!
//! Who may change the rules is the stack's to say (`TAG_FILTER`): a caller
//! that could offer the right to (`NetAdmin`).

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Write;

/// The protocols a rule may be about, as IPv4 numbers them; ICMP is IPv6's
/// too.
pub const ANY: u8 = 0;
pub const ICMP: u8 = 1;
pub const TCP: u8 = 6;
pub const UDP: u8 = 17;
const ICMPV6: u8 = 58;
/// The most rules there are.
pub const MAX_RULES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub drop: bool,
    pub protocol: u8,
    /// This machine's ports, first and last: all of them, for every port.
    pub ports: (u16, u16),
    /// Where from: an address — IPv4's in the first four bytes — and how
    /// many of its bits must match, or `None` for anywhere.
    pub from: Option<([u8; 16], u8, bool)>,
}

impl Rule {
    fn matches(&self, p: &Packet) -> bool {
        let protocol_ok = match self.protocol {
            ANY => true,
            ICMP => p.protocol == ICMP || p.protocol == ICMPV6,
            other => p.protocol == other,
        };
        let ports_ok = self.ports == (0, u16::MAX) || p.port.is_some_and(|port| self.ports.0 <= port && port <= self.ports.1);
        let from_ok = match self.from {
            None => true,
            Some((addr, bits, v6)) => v6 == p.v6 && prefix_matches(&addr, &p.from, bits),
        };
        protocol_ok && ports_ok && from_ok
    }

    /// The rule as `netctl filter` writes one.
    pub fn describe(&self, out: &mut impl Write) {
        let _ = write!(out, "{}", if self.drop { "drop" } else { "accept" });
        let _ = write!(
            out,
            " {}",
            match self.protocol {
                TCP => "tcp",
                UDP => "udp",
                ICMP => "icmp",
                _ => "any",
            }
        );
        if self.ports != (0, u16::MAX) {
            if self.ports.0 == self.ports.1 {
                let _ = write!(out, " port {}", self.ports.0);
            } else {
                let _ = write!(out, " ports {}-{}", self.ports.0, self.ports.1);
            }
        }
        if let Some((addr, bits, v6)) = self.from {
            let _ = write!(out, " from ");
            if v6 {
                let words: Vec<u16> = addr.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                for (i, w) in words.iter().enumerate() {
                    let _ = write!(out, "{}{:x}", if i > 0 { ":" } else { "" }, w);
                }
            } else {
                let _ = write!(out, "{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
            }
            let _ = write!(out, "/{}", bits);
        }
    }
}

fn prefix_matches(want: &[u8; 16], got: &[u8; 16], bits: u8) -> bool {
    let bits = usize::from(bits).min(128);
    let whole = bits / 8;
    if want[..whole] != got[..whole] {
        return false;
    }
    let rest = bits % 8;
    rest == 0 || (want[whole] ^ got[whole]) & (0xFFu8 << (8 - rest)) == 0
}

/// What a rule is matched against: a packet's protocol, the port of this
/// machine's it is for, and where it is from.
struct Packet {
    protocol: u8,
    port: Option<u16>,
    from: [u8; 16],
    v6: bool,
}

/// An IP packet's protocol, port and source; nothing for what is not one.
fn packet(ip: &[u8]) -> Option<Packet> {
    let mut from = [0u8; 16];
    let (protocol, v6, l4, fragment) = match ip.first()? >> 4 {
        4 => {
            let header = usize::from(ip[0] & 0x0F) * 4;
            from[..4].copy_from_slice(ip.get(12..16)?);
            // Only a datagram's first fragment says its ports.
            let offset = u16::from_be_bytes([*ip.get(6)?, *ip.get(7)?]) & 0x1FFF;
            (*ip.get(9)?, false, ip.get(header..)?, offset != 0)
        }
        6 => {
            from.copy_from_slice(ip.get(8..24)?);
            (*ip.get(6)?, true, ip.get(40..)?, false)
        }
        _ => return None,
    };
    let port = match protocol {
        TCP | UDP if !fragment => Some(u16::from_be_bytes([*l4.get(2)?, *l4.get(3)?])),
        _ => None,
    };
    Some(Packet { protocol, port, from, v6 })
}

pub struct Filter {
    pub rules: Vec<Rule>,
    /// Packets dropped since the stack started.
    pub dropped: u64,
}

/// The one filter, which the card and `lo` ask and the stack changes.
pub type Shared = Rc<RefCell<Filter>>;

pub fn new() -> Shared {
    Rc::new(RefCell::new(Filter { rules: Vec::new(), dropped: 0 }))
}

impl Filter {
    /// Whether an IP packet is let in. What is not one is.
    pub fn admits(&mut self, ip: &[u8]) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        let Some(p) = packet(ip) else { return true };
        match self.rules.iter().find(|r| r.matches(&p)) {
            Some(r) if r.drop => {
                self.dropped += 1;
                false
            }
            _ => true,
        }
    }

    /// Whether an Ethernet frame is: what it carries, if that is IP.
    pub fn admits_frame(&mut self, frame: &[u8]) -> bool {
        match frame.get(12..14) {
            Some([0x08, 0x00]) | Some([0x86, 0xDD]) => self.admits(&frame[14..]),
            _ => true,
        }
    }
}
