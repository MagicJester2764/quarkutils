//! The protocols, as smoltcp keeps them: two interfaces, each with the
//! sockets that go through it, and what DHCP said; and the streams let go
//! of that have not finished saying goodbye.
//!
//! `eth0` is the card. `lo` is what this machine says to itself: 127.0.0.1
//! and ::1, and the card's own addresses too, so that a connection to the
//! machine by the address it has on its network is answered here rather
//! than sent out to be answered by nobody. A socket is in one interface's
//! set or the other's, by where it is going ([`Stack::side_for`]); a
//! listener is in both.


use alloc::vec;
use alloc::vec::Vec;
use quark_rt::{nic, println, syscall};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};

use crate::card::Card;
use crate::lo::Lo;

/// Which interface a socket goes through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Eth,
    Lo,
}

/// The address QEMU's user network gives, and its gateway and DNS relay:
/// what is taken when DHCP says nothing.
const FALLBACK: (Ipv4Address, u8) = (Ipv4Address::new(10, 0, 2, 15), 24);
const FALLBACK_ROUTER: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
const FALLBACK_DNS: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);

/// The most turns `lo` is given in one poll: a conversation the machine has
/// with itself stops when a receiver's buffer is full, and this is a bound
/// on one that does not.
const LO_TURNS: usize = 256;
/// A stream let go of that is still saying goodbye after this long goes
/// anyway.
const RETIRE_LIMIT: Duration = Duration::from_secs(60);

/// The time, as the protocols count it.
pub fn now() -> Instant {
    Instant::from_micros((syscall::sys_clock() / 1000) as i64)
}

pub struct Stack {
    pub card: Card,
    eth: Interface,
    lo_dev: Lo,
    lo: Interface,
    pub eth_sockets: SocketSet<'static>,
    pub lo_sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    pub mac: [u8; 6],
    /// What DHCP gave, or what was taken when it gave nothing.
    pub ipv4: Option<Ipv4Cidr>,
    pub router: Option<Ipv4Address>,
    pub dns: Vec<IpAddress>,
    /// Streams let go of, still saying goodbye, and since when.
    retiring: Vec<(Side, SocketHandle, Instant)>,
}

impl Stack {
    pub fn new(link: nic::Link, mac: [u8; 6]) -> Stack {
        let mut card = Card::new(link);
        let mut seed = [0u8; 8];
        let _ = quark_rt::random::fill(&mut seed);
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed = u64::from_le_bytes(seed);
        let eth = Interface::new(config, &mut card, now());

        let mut lo_dev = Lo::new();
        let mut lo_config = Config::new(HardwareAddress::Ip);
        lo_config.random_seed = u64::from_le_bytes(seed).rotate_left(17);
        let mut lo = Interface::new(lo_config, &mut lo_dev, now());
        lo.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8));
            let _ = addrs.push(IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128));
        });

        let mut eth_sockets = SocketSet::new(vec![]);
        let dhcp = eth_sockets.add(dhcpv4::Socket::new());
        Stack {
            card,
            eth,
            lo_dev,
            lo,
            eth_sockets,
            lo_sockets: SocketSet::new(vec![]),
            dhcp,
            mac,
            ipv4: None,
            router: None,
            dns: Vec::new(),
            retiring: Vec::new(),
        }
    }

    /// Whether the card has an address.
    pub fn configured(&self) -> bool {
        self.ipv4.is_some()
    }

    /// Move the protocols on: what has come in, what is due to go out.
    /// What `lo` sends it receives at once, until it has nothing in flight.
    pub fn poll(&mut self) {
        let t = now();
        self.eth.poll(t, &mut self.card, &mut self.eth_sockets);
        for _ in 0..LO_TURNS {
            self.lo.poll(t, &mut self.lo_dev, &mut self.lo_sockets);
            if self.lo_dev.is_empty() {
                break;
            }
        }
        self.dhcp_events();
        self.sweep();
    }

    /// A stream nobody wants: one listening goes now, and any other is reset
    /// and goes once it has said so.
    pub fn drop_stream(&mut self, side: Side, h: SocketHandle) {
        let t = self.sockets(side).get_mut::<tcp::Socket>(h);
        if t.state() == tcp::State::Listen {
            self.sockets(side).remove(h);
        } else {
            t.abort();
            self.retire(side, h);
        }
    }

    /// A stream that is nobody's any more, closed or aborted: it goes once
    /// it has said goodbye, or after a minute whether it has or not.
    pub fn retire(&mut self, side: Side, h: SocketHandle) {
        self.retiring.push((side, h, now()));
    }

    fn sweep(&mut self) {
        let t = now();
        let mut k = 0;
        while k < self.retiring.len() {
            let (side, h, since) = self.retiring[k];
            let s = self.sockets(side).get::<tcp::Socket>(h);
            let done = match s.state() {
                // Aborted, and the reset sent: smoltcp forgets the other end
                // when it has.
                tcp::State::Closed => s.remote_endpoint().is_none(),
                tcp::State::TimeWait | tcp::State::Listen => true,
                _ => false,
            };
            if done || t - since > RETIRE_LIMIT {
                self.sockets(side).remove(h);
                self.retiring.swap_remove(k);
            } else {
                k += 1;
            }
        }
    }

    /// How long until something is due, at most.
    pub fn poll_delay(&mut self) -> Option<Duration> {
        let t = now();
        match (self.eth.poll_delay(t, &self.eth_sockets), self.lo.poll_delay(t, &self.lo_sockets)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn dhcp_events(&mut self) {
        // What was said, taken out of the socket before anything is done
        // about it.
        let event = match self.eth_sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            None => return,
            Some(dhcpv4::Event::Configured(config)) => Some((
                config.address,
                config.router,
                config.dns_servers.iter().map(|&a| IpAddress::Ipv4(a)).collect::<Vec<IpAddress>>(),
            )),
            Some(dhcpv4::Event::Deconfigured) => None,
        };
        match event {
            Some((address, router, dns)) => {
                self.take_ipv4(address, router, dns);
                let a = address.address().octets();
                println!("[net] DHCP: acquired {}.{}.{}.{}/{}", a[0], a[1], a[2], a[3], address.prefix_len());
            }
            // Said when the client starts, too, with nothing to give up.
            None => {
                let Some(old) = self.ipv4.take() else { return };
                self.eth.update_ip_addrs(|addrs| addrs.retain(|a| *a != IpCidr::Ipv4(old)));
                self.lo.update_ip_addrs(|addrs| addrs.retain(|a| *a != IpCidr::Ipv4(old)));
                self.eth.routes_mut().remove_default_ipv4_route();
                self.router = None;
                println!("[net] DHCP: the address was given up");
            }
        }
    }

    /// The card's address is `cidr`, its way out `router`, its names asked
    /// of `dns`. The address is `lo`'s too.
    fn take_ipv4(&mut self, cidr: Ipv4Cidr, router: Option<Ipv4Address>, dns: Vec<IpAddress>) {
        if let Some(old) = self.ipv4 {
            self.eth.update_ip_addrs(|addrs| addrs.retain(|a| *a != IpCidr::Ipv4(old)));
            self.lo.update_ip_addrs(|addrs| addrs.retain(|a| *a != IpCidr::Ipv4(old)));
        }
        self.eth.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv4(cidr));
        });
        self.lo.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(cidr.address(), 32)));
        });
        match router {
            Some(r) => {
                let _ = self.eth.routes_mut().add_default_ipv4_route(r);
            }
            None => {
                self.eth.routes_mut().remove_default_ipv4_route();
            }
        }
        self.ipv4 = Some(cidr);
        self.router = router;
        self.dns = dns;
    }

    /// What QEMU's user network would have given, when DHCP gave nothing.
    pub fn fallback(&mut self) {
        let mut dns = Vec::new();
        dns.push(IpAddress::Ipv4(FALLBACK_DNS));
        self.take_ipv4(Ipv4Cidr::new(FALLBACK.0, FALLBACK.1), Some(FALLBACK_ROUTER), dns);
    }

    /// Which interface reaches `addr`: this machine's own addresses are
    /// `lo`'s, and everything else the card's.
    pub fn side_for(&self, addr: IpAddress) -> Side {
        let own = match addr {
            IpAddress::Ipv4(a) => a.is_loopback() || self.ipv4.is_some_and(|c| c.address() == a),
            IpAddress::Ipv6(a) => a.is_loopback() || self.eth.has_ip_addr(addr),
        };
        if own { Side::Lo } else { Side::Eth }
    }

    /// The sockets of a side.
    pub fn sockets(&mut self, side: Side) -> &mut SocketSet<'static> {
        match side {
            Side::Eth => &mut self.eth_sockets,
            Side::Lo => &mut self.lo_sockets,
        }
    }

    /// A side's sockets and its interface's context, which a connection
    /// needs to be begun.
    pub fn parts(&mut self, side: Side) -> (&mut SocketSet<'static>, &mut smoltcp::iface::Context) {
        match side {
            Side::Eth => (&mut self.eth_sockets, self.eth.context()),
            Side::Lo => (&mut self.lo_sockets, self.lo.context()),
        }
    }

    /// The card's IPv4 address, as one word, high byte first; 0 for none.
    pub fn ipv4_word(&self) -> u32 {
        self.ipv4.map_or(0, |c| u32::from_be_bytes(c.address().octets()))
    }
}
