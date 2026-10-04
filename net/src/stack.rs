//! The protocols, as smoltcp keeps them: two interfaces, each with the
//! sockets that go through it, and what DHCP said; and the two things about
//! streams that are not one socket's — a port listened on, and a stream let
//! go of that has not finished saying goodbye.
//!
//! `eth0` is the card: IPv4 by DHCP, and IPv6 by what routers advertise
//! (`ndp`). `lo` is what this machine says to itself: 127.0.0.1
//! and ::1, and the card's own addresses too, so that a connection to the
//! machine by the address it has on its network is answered here rather
//! than sent out to be answered by nobody. A socket is in one interface's
//! set or the other's, by where it is going ([`Stack::side_for`]); a
//! listener is in both.
//!
//! A smoltcp socket listens for one connection and then is it, and it has
//! no queue of connections half made: a second SYN while the only socket
//! listening is answering the first is refused. So a port listened on is
//! several sockets ([`Stack::listen`]), and packets are taken in one at a
//! time, with a socket listening again on each side after every one while
//! there is room for another connection ([`Stack::refill`]).
//!
//! A datagram for an address on the network that nothing answers "who has
//! it?" for waits at the head of its socket's queue for an answer, and
//! smoltcp sends a socket's queue in order: one for nobody held up every
//! datagram behind it, to anybody, for good — and its asking again every
//! second took the card's one question a second, so that once another
//! neighbour's answer expired it was never asked for again. So every
//! datagram the card's sockets queue goes through [`Stack::send_datagram`],
//! which keeps what is queued where; a socket that has sent nothing for
//! [`STUCK`] has its queue let go of, and the address at its head is given
//! up for [`GIVEN_UP`] — what is sent there meanwhile is dropped where it is
//! sent, and what is sent anywhere else goes.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use quark_rt::{nic, println, syscall};
use smoltcp::iface::{Config, Interface, PollIngressSingleResult, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, tcp, udp, Socket};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv4Cidr};

use crate::card::Card;
use crate::filter;
use crate::lo::Lo;
use crate::ndp::Ndp;

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
/// The most packets taken from the card in one poll: what is left is taken
/// at the next, after whoever is waiting has been answered.
const CARD_PACKETS: usize = 512;
/// A stream's buffers, each way: what its window can be.
pub const STREAM_BUF: usize = 64 * 1024;
/// Data unacknowledged for this long, or a connection unanswered, is given
/// up.
pub const TCP_TIMEOUT: Duration = Duration::from_secs(60);
/// A stream let go of that is still saying goodbye after this long goes
/// anyway.
const RETIRE_LIMIT: Duration = Duration::from_secs(60);

/// How long a datagram socket on the card may send nothing while it has
/// something to send — three tries at "who has it?" — before its queue is
/// let go of.
const STUCK: Duration = Duration::from_secs(3);
/// How long an address nothing answered for is given up for.
const GIVEN_UP: Duration = Duration::from_secs(30);

/// A datagram socket on the card with something queued: for where, how
/// long each, and since when it has sent nothing.
struct Queued {
    socket: SocketHandle,
    datagrams: VecDeque<(IpAddress, usize)>,
    idle: Option<Instant>,
}

/// A stream socket with the buffers and the timeout every stream has here.
pub fn new_stream() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; STREAM_BUF]), tcp::SocketBuffer::new(vec![0; STREAM_BUF]));
    s.set_timeout(Some(TCP_TIMEOUT));
    s
}

/// A port listened on: its sockets on both sides, listening or connected
/// to and not yet taken.
struct Listen {
    local: IpListenEndpoint,
    sockets: Vec<(Side, SocketHandle)>,
    /// The most connections made, or being made, and not yet taken.
    backlog: usize,
}

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
    /// The DNS servers DHCP named.
    dns4: Vec<IpAddress>,
    pub ndp: Ndp,
    /// What comes in, and what of it is let in: the card's and `lo`'s.
    pub filter: filter::Shared,
    listens: Vec<Option<Listen>>,
    /// Streams let go of, still saying goodbye, and since when.
    retiring: Vec<(Side, SocketHandle, Instant)>,
    /// The card's datagram sockets with something to send.
    queued: Vec<Queued>,
    /// Addresses nothing answered for, and until when they are given up.
    given_up: Vec<(IpAddress, Instant)>,
    /// Datagrams dropped for them.
    pub unanswered: u64,
}

impl Stack {
    pub fn new(link: nic::Link, mac: [u8; 6]) -> Stack {
        let filter = filter::new();
        let mut card = Card::new(link, filter.clone());
        let mut seed = [0u8; 8];
        let _ = quark_rt::random::fill(&mut seed);
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        config.random_seed = u64::from_le_bytes(seed);
        let mut eth = Interface::new(config, &mut card, now());

        let mut lo_dev = Lo::new(filter.clone());
        let mut lo_config = Config::new(HardwareAddress::Ip);
        lo_config.random_seed = u64::from_le_bytes(seed).rotate_left(17);
        let mut lo = Interface::new(lo_config, &mut lo_dev, now());
        lo.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8));
            let _ = addrs.push(IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128));
        });

        let mut eth_sockets = SocketSet::new(vec![]);
        let dhcp = eth_sockets.add(dhcpv4::Socket::new());
        let ndp = Ndp::new(&mut eth, &mut eth_sockets, mac, now());
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
            dns4: Vec::new(),
            ndp,
            filter,
            listens: Vec::new(),
            retiring: Vec::new(),
            queued: Vec::new(),
            given_up: Vec::new(),
            unanswered: 0,
        }
    }

    /// Whether the card has an address.
    pub fn configured(&self) -> bool {
        self.ipv4.is_some()
    }

    /// Move the protocols on: what has come in, a packet at a time, and what
    /// is due to go out. What `lo` sends it receives at once, until it has
    /// nothing in flight.
    pub fn poll(&mut self) {
        let t = now();
        for _ in 0..CARD_PACKETS {
            if matches!(self.eth.poll_ingress_single(t, &mut self.card, &mut self.eth_sockets), PollIngressSingleResult::None) {
                break;
            }
            self.refill(Side::Eth);
        }
        self.ndp.turn(&mut self.eth, &mut self.lo, &mut self.eth_sockets, t);
        let before = self.datagrams_waiting();
        self.eth.poll_egress(t, &mut self.card, &mut self.eth_sockets);
        self.unstick(&before, t);
        for _ in 0..LO_TURNS {
            while !matches!(self.lo.poll_ingress_single(t, &mut self.lo_dev, &mut self.lo_sockets), PollIngressSingleResult::None) {
                self.refill(Side::Lo);
            }
            self.lo.poll_egress(t, &mut self.lo_dev, &mut self.lo_sockets);
            if self.lo_dev.is_empty() {
                break;
            }
        }
        self.dhcp_events();
        self.sweep();
    }

    /// Queue a datagram on `side`'s socket `h`, for `to`; on the card, kept
    /// account of. One for an address given up for is dropped here.
    pub fn send_datagram(&mut self, side: Side, h: SocketHandle, data: &[u8], to: IpEndpoint) -> Result<(), udp::SendError> {
        if side == Side::Lo {
            return self.lo_sockets.get_mut::<udp::Socket>(h).send_slice(data, to);
        }
        let t = now();
        self.given_up.retain(|&(_, until)| until > t);
        if self.given_up.iter().any(|&(a, _)| a == to.addr) {
            self.unanswered += 1;
            return Ok(());
        }
        self.eth_sockets.get_mut::<udp::Socket>(h).send_slice(data, to)?;
        match self.queued.iter_mut().find(|q| q.socket == h) {
            Some(q) => q.datagrams.push_back((to.addr, data.len())),
            None => self.queued.push(Queued { socket: h, datagrams: VecDeque::from([(to.addr, data.len())]), idle: None }),
        }
        Ok(())
    }

    /// Take a datagram socket out of `side`'s set, and out of the account.
    pub fn remove_datagram(&mut self, side: Side, h: SocketHandle) {
        if side == Side::Eth {
            self.queued.retain(|q| q.socket != h);
        }
        self.sockets(side).remove(h);
    }

    /// The card's datagram sockets with something to send, and how much.
    fn datagrams_waiting(&self) -> Vec<(SocketHandle, usize)> {
        self.eth_sockets
            .iter()
            .filter_map(|(h, s)| match s {
                Socket::Udp(u) if u.send_queue() > 0 => Some((h, u.send_queue())),
                _ => None,
            })
            .collect()
    }

    /// After the card's turn: what each datagram socket sent comes off its
    /// account, from the front; one that has sent nothing for [`STUCK`] has
    /// its queue let go of, and the address at its head is given up.
    fn unstick(&mut self, before: &[(SocketHandle, usize)], t: Instant) {
        let after = self.datagrams_waiting();
        let mut stuck = Vec::new();
        self.queued.retain_mut(|q| {
            // Sent everything, or gone.
            let Some(&(_, now)) = after.iter().find(|(h, _)| *h == q.socket) else { return false };
            let was = before.iter().find(|(h, _)| *h == q.socket).map_or(now, |&(_, n)| n);
            if now < was {
                let mut sent = was - now;
                while let Some(&(_, len)) = q.datagrams.front() {
                    if len > sent {
                        break;
                    }
                    sent -= len;
                    q.datagrams.pop_front();
                }
                q.idle = None;
                return true;
            }
            match q.idle {
                None => q.idle = Some(t),
                Some(since) if t - since >= STUCK => {
                    stuck.push((q.socket, q.datagrams.front().map(|&(a, _)| a), q.datagrams.len()));
                    return false;
                }
                Some(_) => {}
            }
            true
        });
        for (h, head, dropped) in stuck {
            // Said by `netctl`, not on the console: a line printed after
            // a prompt pushes the prompt off its line.
            if let Some(addr) = head {
                self.given_up.retain(|&(a, _)| a != addr);
                self.given_up.push((addr, t + GIVEN_UP));
            }
            self.unanswered += dropped.max(1) as u64;
            let s = self.eth_sockets.get_mut::<udp::Socket>(h);
            let at = s.endpoint();
            s.close();
            let _ = s.bind(at);
        }
    }

    /// Listen at `local`, keeping up to `backlog` connections made and not
    /// yet taken: an id for it, or nothing if smoltcp will not listen there.
    pub fn listen(&mut self, local: IpListenEndpoint, backlog: usize) -> Option<usize> {
        let mut l = Listen { local, sockets: Vec::new(), backlog: backlog.max(1) };
        for side in [Side::Eth, Side::Lo] {
            let mut t = new_stream();
            if t.listen(local).is_err() {
                for (side, h) in l.sockets {
                    self.sockets(side).remove(h);
                }
                return None;
            }
            l.sockets.push((side, self.sockets(side).add(t)));
        }
        let id = match self.listens.iter().position(|l| l.is_none()) {
            Some(id) => id,
            None => {
                self.listens.push(None);
                self.listens.len() - 1
            }
        };
        self.listens[id] = Some(l);
        Some(id)
    }

    pub fn set_backlog(&mut self, id: usize, backlog: usize) {
        if let Some(l) = self.listens.get_mut(id).and_then(|l| l.as_mut()) {
            l.backlog = backlog.max(1);
        }
        self.refill(Side::Eth);
        self.refill(Side::Lo);
    }

    /// Whether anything listens on `port`.
    pub fn listening_on(&self, port: u16) -> bool {
        self.listens.iter().flatten().any(|l| l.local.port == port)
    }

    /// The oldest connection made to listener `id` and not yet taken: its
    /// socket, the caller's from now on. One reset before it was taken goes.
    pub fn accept(&mut self, id: usize) -> Option<(Side, SocketHandle)> {
        let mut taken = None;
        let mut k = 0;
        while let Some(&(side, h)) = self.listens.get(id)?.as_ref()?.sockets.get(k) {
            match self.sockets(side).get::<tcp::Socket>(h).state() {
                tcp::State::Listen | tcp::State::SynReceived => k += 1,
                state => {
                    self.listens[id].as_mut()?.sockets.remove(k);
                    if matches!(state, tcp::State::Established | tcp::State::CloseWait) {
                        taken = Some((side, h));
                        break;
                    }
                    self.retire(side, h);
                }
            }
        }
        // Taking one may be room for another.
        self.refill(Side::Eth);
        self.refill(Side::Lo);
        taken
    }

    /// Whether listener `id` has a connection to take.
    pub fn pending(&self, id: usize) -> bool {
        let Some(Some(l)) = self.listens.get(id) else { return false };
        l.sockets.iter().any(|&(side, h)| {
            let set = match side {
                Side::Eth => &self.eth_sockets,
                Side::Lo => &self.lo_sockets,
            };
            matches!(set.get::<tcp::Socket>(h).state(), tcp::State::Established | tcp::State::CloseWait)
        })
    }

    /// Stop listening: the sockets listening go, and connections not yet
    /// taken are reset.
    pub fn unlisten(&mut self, id: usize) {
        let Some(l) = self.listens.get_mut(id).and_then(|l| l.take()) else { return };
        for (side, h) in l.sockets {
            self.drop_stream(side, h);
        }
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

    /// After a packet has come in on `side`: every listener with room for
    /// another connection has a socket of that side listening.
    fn refill(&mut self, side: Side) {
        let Stack { listens, eth_sockets, lo_sockets, .. } = self;
        for l in listens.iter_mut().flatten() {
            let mut listening = false;
            let mut waiting = 0;
            for &(s, h) in &l.sockets {
                let set = match s {
                    Side::Eth => &*eth_sockets,
                    Side::Lo => &*lo_sockets,
                };
                match set.get::<tcp::Socket>(h).state() {
                    tcp::State::Listen => listening |= s == side,
                    _ => waiting += 1,
                }
            }
            if listening || waiting >= l.backlog {
                continue;
            }
            let mut t = new_stream();
            if t.listen(l.local).is_ok() {
                let set = match side {
                    Side::Eth => &mut *eth_sockets,
                    Side::Lo => &mut *lo_sockets,
                };
                l.sockets.push((side, set.add(t)));
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
        self.dns4 = dns;
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

    /// The card, its addresses, its ways out and its DNS servers, and `lo`'s
    /// addresses, as `netctl` shows them.
    pub fn describe(&self, out: &mut String) {
        let m = self.mac;
        let _ = writeln!(out, "eth0  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
        for a in self.eth.ip_addrs() {
            let _ = writeln!(out, "  {}", a);
        }
        let mut ways: Vec<IpAddress> = self.router.map(IpAddress::Ipv4).into_iter().collect();
        ways.extend(self.ndp.router().map(IpAddress::Ipv6));
        for (what, list) in [("way out by", ways), ("dns", self.dns())] {
            if !list.is_empty() {
                let _ = write!(out, "  {}", what);
                for (i, a) in list.iter().enumerate() {
                    let _ = write!(out, "{} {}", if i > 0 { "," } else { "" }, a);
                }
                out.push('\n');
            }
        }
        if self.unanswered > 0 || !self.given_up.is_empty() {
            let _ = write!(out, "  datagrams dropped for addresses nothing answered: {}", self.unanswered);
            for (a, _) in &self.given_up {
                let _ = write!(out, " {}", a);
            }
            out.push('\n');
        }
        let _ = writeln!(out, "lo");
        for a in self.lo.ip_addrs() {
            let _ = writeln!(out, "  {}", a);
        }
    }

    /// The DNS servers there are: DHCP's, then what routers advertised.
    pub fn dns(&self) -> Vec<IpAddress> {
        let mut all = self.dns4.clone();
        all.extend(self.ndp.servers.iter().map(|&(s, _)| IpAddress::Ipv6(s)));
        all
    }

    /// The card's IPv4 address, as one word, high byte first; 0 for none.
    pub fn ipv4_word(&self) -> u32 {
        self.ipv4.map_or(0, |c| u32::from_be_bytes(c.address().octets()))
    }
}
