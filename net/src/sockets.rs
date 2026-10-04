//! Sockets as descriptors: what a C library's `socket` is, and Rust's.
//!
//! A socket is a descriptor this server serves (`SYS_FD_SERVE`, with the
//! flag that says this server says when it is ready), named by a cookie of
//! its own. Reading and writing it are calls the kernel makes for the task
//! that holds it, its buffer lent (`TAG_FD_READ`, `TAG_FD_WRITE`), told
//! whether the task may wait; everything else is a request naming the
//! cookie (`TAG_SOCKET`), believed of a task whose program holds it
//! (`SYS_FD_HOLDS`). What the socket is ready for is said to the kernel
//! whenever it changes, and a poll answers that. The last descriptor
//! closing is told as every served descriptor's is, and the socket goes
//! with it.
//!
//! A request that waits — for a connection to be made or to come, for bytes
//! or room for them, for a datagram — is held with its reply and answered
//! after the turn of the loop that brings what it waits for
//! ([`Sockets::settle`]). A task that asks something else, or dies, is
//! waiting for nothing.
//!
//! `data[0]` is the operation, with flags above it (`op | flags << 8`): 1,
//! the caller may not wait; 2, a receive only looks. An address is three
//! words: `family << 16 | port`, then the sixteen bytes of the address as
//! they are on the wire — IPv4's four first and nought after — read as two
//! little-endian words. A refusal is tag `u64::MAX` and Linux's errno.
//! `docs/net.md` has every request's words.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use quark_rt::ipc::{Message, TAG_FD_READ};
use quark_rt::syscall::{self, FD_READY_HANGUP, FD_READY_READ, FD_READY_WRITE};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{tcp, udp};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv6Address};

use crate::stack::{new_stream, now, Side, Stack, STREAM_BUF, TCP_TIMEOUT};

/// Everything about a socket that is not reading and writing it.
pub const TAG_SOCKET: u64 = 20;
const OP_CREATE: u64 = 0;
const OP_BIND: u64 = 1;
const OP_LISTEN: u64 = 2;
const OP_CONNECT: u64 = 3;
const OP_ACCEPT: u64 = 4;
const OP_SEND_TO: u64 = 5;
const OP_RECV_FROM: u64 = 6;
const OP_SHUTDOWN: u64 = 7;
const OP_NAME: u64 = 8;
const OP_OPTION: u64 = 9;
/// Beside the operation: the caller may not wait; a receive only looks.
const FLAG_DO_NOT_WAIT: u64 = 1;
const FLAG_PEEK: u64 = 2;

/// `OP_OPTION`'s options.
const OPT_ERROR: u64 = 1;
const OPT_KEEPALIVE: u64 = 2;
const OPT_NODELAY: u64 = 3;
const OPT_LISTENING: u64 = 4;
const OPT_TYPE: u64 = 5;
const OPT_V6ONLY: u64 = 6;
const OPT_RCVBUF: u64 = 7;
const OPT_SNDBUF: u64 = 8;
const OPT_PENDING: u64 = 10;

/// Linux's families, types and protocols, as a C library says them.
const AF_UNSPEC: u64 = 0;
const AF_INET: u64 = 2;
const AF_INET6: u64 = 10;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const IPPROTO_TCP: u64 = 6;
const IPPROTO_UDP: u64 = 17;

/// Linux's errnos, which is what a refusal says.
const EBADF: u64 = 9;
const EAGAIN: u64 = 11;
const EFAULT: u64 = 14;
const EINVAL: u64 = 22;
const ENFILE: u64 = 23;
const EMFILE: u64 = 24;
const EPIPE: u64 = 32;
const EDESTADDRREQ: u64 = 89;
const EMSGSIZE: u64 = 90;
const ENOPROTOOPT: u64 = 92;
const EPROTONOSUPPORT: u64 = 93;
const ESOCKTNOSUPPORT: u64 = 94;
const EOPNOTSUPP: u64 = 95;
const EAFNOSUPPORT: u64 = 97;
const EADDRINUSE: u64 = 98;
const EADDRNOTAVAIL: u64 = 99;
const ENETUNREACH: u64 = 101;
const ECONNRESET: u64 = 104;
const EISCONN: u64 = 106;
const ENOTCONN: u64 = 107;
const ETIMEDOUT: u64 = 110;
const ECONNREFUSED: u64 = 111;
const EALREADY: u64 = 114;
const EINPROGRESS: u64 = 115;

/// The bit every socket's cookie has.
pub const SOCKET_COOKIE: u64 = 1 << 41;
const MAX_SOCKETS: usize = 256;
/// The most one program may have open at once.
const PER_PROGRAM: usize = 128;
/// A datagram socket's buffers, each way: so many datagrams, so many bytes.
const DATAGRAMS: usize = 32;
const DATAGRAM_BUF: usize = 64 * 1024;
const MAX_BACKLOG: usize = 16;
/// The most read or written in one call here.
const IO_CHUNK: usize = 64 * 1024;
/// Where the ports chosen here come from: below the old protocol's.
const EPHEMERAL: (u16, u16) = (32768, 49151);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stream,
    Datagram,
}

/// A request held until it can be answered.
#[derive(Clone, Copy)]
enum Held {
    Connect { tid: usize },
    Accept { tid: usize },
    /// A read through the descriptor, or a receive (`asked`), with this much
    /// room; `peek` only looks.
    Read { tid: usize, room: usize, asked: bool, peek: bool },
    /// A write through the descriptor, or a send, of `len` bytes; a
    /// datagram to `to` if it said where.
    Write { tid: usize, len: usize, to: Option<IpEndpoint> },
}

impl Held {
    fn tid(&self) -> usize {
        match *self {
            Held::Connect { tid } | Held::Accept { tid } | Held::Read { tid, .. } | Held::Write { tid, .. } => tid,
        }
    }
}

struct Socket {
    kind: Kind,
    /// IPv6's family, which reaches IPv4 too, as `::ffff:a.b.c.d`.
    v6: bool,
    /// The program it is counted against: the one that made it, or took it.
    space: u64,
    /// A stream's connection, or a datagram socket's on the card.
    socket: Option<(Side, SocketHandle)>,
    /// A datagram socket's on `lo`.
    lo: Option<SocketHandle>,
    /// Where it is bound, said or chosen.
    local: Option<IpListenEndpoint>,
    /// A datagram socket's one correspondent.
    peer: Option<IpEndpoint>,
    /// The stack's id for what it listens with.
    listen: Option<usize>,
    /// What went wrong and has not been asked: SO_ERROR.
    error: u64,
    /// A read through the descriptor has failed for it — which is all such
    /// a read can say — and the error is kept for whoever asks which.
    error_told: bool,
    /// When the connection being made began to be.
    connecting: Option<Instant>,
    /// The connection is over by the other end's doing, or nobody's: said
    /// once, as an error.
    ended: bool,
    read_shut: bool,
    write_shut: bool,
    v6only: bool,
    nodelay: bool,
    keepalive: bool,
    held: Vec<Held>,
    /// What the kernel was last told it is ready for.
    said: Option<u32>,
}

impl Socket {
    fn new(kind: Kind, v6: bool, space: u64) -> Socket {
        Socket {
            kind,
            v6,
            space,
            socket: None,
            lo: None,
            local: None,
            peer: None,
            listen: None,
            error: 0,
            error_told: false,
            connecting: None,
            ended: false,
            read_shut: false,
            write_shut: false,
            v6only: false,
            nodelay: false,
            keepalive: false,
            held: Vec::new(),
            said: None,
        }
    }
}

pub struct Sockets {
    table: Vec<Option<Socket>>,
    next_port: u16,
}

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

fn ok(tid: usize, data: [u64; 6]) {
    reply(tid, 0, data);
}

fn fail(tid: usize, errno: u64) {
    reply(tid, u64::MAX, [errno, 0, 0, 0, 0, 0]);
}

/// A read or write through a descriptor that may not wait, and would.
fn nothing_yet(tid: usize) {
    ok(tid, [syscall::FD_IO_NOTHING_YET, 0, 0, 0, 0, 0]);
}

/// An address and a port, and the family they were said in, from three
/// words. An IPv4 address in IPv6's clothes is the IPv4 address.
fn endpoint_of(famport: u64, a0: u64, a1: u64) -> Option<(IpAddress, u16, u64)> {
    let port = famport as u16;
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&a0.to_le_bytes());
    b[8..].copy_from_slice(&a1.to_le_bytes());
    match famport >> 16 {
        AF_INET => Some((IpAddress::Ipv4(Ipv4Address::new(b[0], b[1], b[2], b[3])), port, AF_INET)),
        AF_INET6 => {
            let v6 = Ipv6Address::from(b);
            match v6.to_ipv4_mapped() {
                Some(v4) => Some((IpAddress::Ipv4(v4), port, AF_INET6)),
                None => Some((IpAddress::Ipv6(v6), port, AF_INET6)),
            }
        }
        _ => None,
    }
}

/// Three words for an address and a port, in the family the socket is.
fn words_of(addr: Option<IpAddress>, port: u16, v6: bool) -> [u64; 3] {
    let mut b = [0u8; 16];
    let family = match (addr, v6) {
        (Some(IpAddress::Ipv4(a)), false) => {
            b[..4].copy_from_slice(&a.octets());
            AF_INET
        }
        (Some(IpAddress::Ipv4(a)), true) => {
            b = a.to_ipv6_mapped().octets();
            AF_INET6
        }
        (Some(IpAddress::Ipv6(a)), _) => {
            b = a.octets();
            AF_INET6
        }
        (None, false) => AF_INET,
        (None, true) => AF_INET6,
    };
    [
        family << 16 | port as u64,
        u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])),
        u64::from_le_bytes(b[8..].try_into().unwrap_or([0; 8])),
    ]
}

/// The most a datagram to `to` can carry out of `side`: nothing is cut into
/// fragments here.
fn datagram_room(side: Side, to: IpAddress) -> usize {
    let mtu = match side {
        Side::Eth => quark_rt::nic::FRAME - 14,
        Side::Lo => 65535,
    };
    let header = match to {
        IpAddress::Ipv4(_) => 20,
        IpAddress::Ipv6(_) => 40,
    };
    mtu - header - 8
}

fn new_datagram() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; DATAGRAMS], vec![0; DATAGRAM_BUF]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; DATAGRAMS], vec![0; DATAGRAM_BUF]),
    )
}

fn stream(net: &mut Stack, side: Side, h: SocketHandle) -> &mut tcp::Socket<'static> {
    net.sockets(side).get_mut::<tcp::Socket>(h)
}

fn datagram(net: &mut Stack, side: Side, h: SocketHandle) -> &mut udp::Socket<'static> {
    net.sockets(side).get_mut::<udp::Socket>(h)
}

/// What came to datagram socket `h` from anybody but `peer`, which nothing
/// will receive, goes: so that what waits is what a receive would give, and
/// a poll says readable only for that.
fn drop_strangers(net: &mut Stack, side: Side, h: SocketHandle, peer: Option<IpEndpoint>) {
    let Some(peer) = peer else { return };
    let d = datagram(net, side, h);
    while d.peek().is_ok_and(|(_, meta)| meta.endpoint != peer) {
        let _ = d.recv();
    }
}

/// An end of a connection as `netctl` writes one: `*` for any address, and
/// IPv6's in brackets; `-` for none.
pub fn write_end(out: &mut String, end: Option<(Option<IpAddress>, u16)>) {
    match end {
        None => out.push('-'),
        Some((None, port)) => {
            let _ = write!(out, "*:{}", port);
        }
        Some((Some(IpAddress::Ipv6(a)), port)) => {
            let _ = write!(out, "[{}]:{}", a, port);
        }
        Some((Some(a), port)) => {
            let _ = write!(out, "{}:{}", a, port);
        }
    }
}

/// What stream `h` has come for `tid`, into its room — or only looked at;
/// or the end. False if there is nothing yet.
fn bytes_to(net: &mut Stack, tid: usize, side: Side, h: SocketHandle, room: usize, peek: bool) -> bool {
    let t = stream(net, side, h);
    if t.can_recv() {
        let mut buf = vec![0u8; room.min(IO_CHUNK)];
        let n = if peek { t.peek_slice(&mut buf) } else { t.recv_slice(&mut buf) }.unwrap_or(0);
        if syscall::sys_lent_write(tid, 0, &buf[..n]).is_err() {
            fail(tid, EFAULT);
        } else {
            ok(tid, [n as u64, 0, 0, 0, 0, 0]);
        }
        return true;
    }
    if !t.may_recv() {
        ok(tid, [0; 6]);
        return true;
    }
    false
}

/// As much of `tid`'s `len` bytes as stream `h` has room for. False if it
/// has none yet.
fn bytes_from(net: &mut Stack, tid: usize, side: Side, h: SocketHandle, len: usize) -> bool {
    let t = stream(net, side, h);
    if !t.may_send() {
        fail(tid, EPIPE);
        return true;
    }
    let room = t.send_capacity() - t.send_queue();
    if room == 0 && len > 0 {
        return false;
    }
    let n = len.min(room).min(IO_CHUNK);
    let mut buf = vec![0u8; n];
    if n > 0 && syscall::sys_lent_read(tid, 0, &mut buf) != Ok(n) {
        fail(tid, EFAULT);
        return true;
    }
    let sent = t.send_slice(&buf).unwrap_or(0);
    ok(tid, [sent as u64, 0, 0, 0, 0, 0]);
    true
}

impl Sockets {
    pub fn new() -> Sockets {
        let mut table = Vec::new();
        table.resize_with(MAX_SOCKETS, || None);
        Sockets { table, next_port: EPHEMERAL.0 }
    }

    pub fn is_ours(cookie: u64) -> bool {
        cookie & SOCKET_COOKIE != 0
    }

    fn index(cookie: u64) -> usize {
        (cookie & !SOCKET_COOKIE) as usize
    }

    fn sock(&mut self, i: usize) -> &mut Socket {
        self.table[i].as_mut().unwrap()
    }

    /// Whether a socket of `kind` other than `except` has `port`.
    fn port_taken(&self, net: &Stack, kind: Kind, port: u16, except: usize) -> bool {
        (kind == Kind::Stream && net.listening_on(port))
            || self.table.iter().enumerate().any(|(j, s)| {
                j != except && s.as_ref().is_some_and(|s| s.kind == kind && s.local.is_some_and(|l| l.port == port))
            })
    }

    fn ephemeral(&mut self, net: &Stack, kind: Kind) -> Option<u16> {
        for _ in EPHEMERAL.0..=EPHEMERAL.1 {
            let p = self.next_port;
            self.next_port = if p >= EPHEMERAL.1 { EPHEMERAL.0 } else { p + 1 };
            if !self.port_taken(net, kind, p, usize::MAX) {
                return Some(p);
            }
        }
        None
    }

    /// The socket `cookie` names, if `tid`'s program holds a descriptor for
    /// it.
    fn held_by(&mut self, tid: usize, cookie: u64) -> Option<usize> {
        let i = Self::index(cookie);
        (Self::is_ours(cookie) && self.table.get(i).is_some_and(|s| s.is_some()) && syscall::sys_fd_holds(tid, cookie))
            .then_some(i)
    }

    /// How many sockets program `space` has.
    fn count(&self, space: u64) -> usize {
        self.table.iter().flatten().filter(|s| s.space == space).count()
    }

    /// An address given for socket `i`, which must be of its family — or,
    /// where it is to be sent to (`mapped`), IPv4's to a datagram socket of
    /// IPv6's that has not asked for IPv6 alone, which reaches it as Linux
    /// has it.
    fn address(&self, i: usize, famport: u64, a0: u64, a1: u64, mapped: bool) -> Result<(IpAddress, u16), u64> {
        let s = self.table[i].as_ref().unwrap();
        let family = if s.v6 { AF_INET6 } else { AF_INET };
        match endpoint_of(famport, a0, a1) {
            Some((addr, port, f)) if f == family => Ok((addr, port)),
            Some((addr, port, AF_INET)) if mapped && s.v6 && s.kind == Kind::Datagram && !s.v6only => Ok((addr, port)),
            _ => Err(EAFNOSUPPORT),
        }
    }

    /// A `TAG_SOCKET` request.
    pub fn request(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let d = msg.data;
        let (op, flags) = (d[0] & 0xFF, d[0] >> 8);
        let may_wait = flags & FLAG_DO_NOT_WAIT == 0;
        if op == OP_CREATE {
            return self.create(net, tid, d[1], d[2], d[3]);
        }
        let Some(i) = self.held_by(tid, d[1]) else {
            return fail(tid, EBADF);
        };
        self.progress(net, i);
        match op {
            OP_BIND => self.bind(net, tid, i, d[2], d[3], d[4]),
            OP_LISTEN => self.listen(net, tid, i, d[2] as usize),
            OP_CONNECT => self.connect(net, tid, i, d[2], d[3], d[4], may_wait),
            OP_ACCEPT => self.accept(net, tid, i, may_wait),
            OP_SEND_TO => {
                // Nought for a family: to the one it is connected to.
                let to = if d[2] >> 16 == AF_UNSPEC {
                    None
                } else {
                    match self.address(i, d[2], d[3], d[4], true) {
                        Ok((addr, port)) => Some(IpEndpoint::new(addr, port)),
                        Err(e) => return fail(tid, e),
                    }
                };
                self.send(net, tid, i, to, d[5] as usize, may_wait, true);
            }
            OP_RECV_FROM => self.receive(net, tid, i, d[2] as usize, may_wait, true, flags & FLAG_PEEK != 0),
            OP_SHUTDOWN => self.shutdown(net, tid, i, d[2]),
            OP_NAME => self.name(net, tid, i, d[2] == 1),
            OP_OPTION => self.option(net, tid, i, d[2], d[3], d[4] == 1),
            _ => fail(tid, EOPNOTSUPP),
        }
        self.tell(net, i);
    }

    fn create(&mut self, net: &mut Stack, tid: usize, family: u64, kind: u64, protocol: u64) {
        let v6 = match family {
            AF_INET => false,
            AF_INET6 => true,
            _ => return fail(tid, EAFNOSUPPORT),
        };
        let kind = match (kind & 0xF, protocol) {
            (SOCK_STREAM, 0 | IPPROTO_TCP) => Kind::Stream,
            (SOCK_DGRAM, 0 | IPPROTO_UDP) => Kind::Datagram,
            (SOCK_STREAM | SOCK_DGRAM, _) => return fail(tid, EPROTONOSUPPORT),
            _ => return fail(tid, ESOCKTNOSUPPORT),
        };
        let space = syscall::sys_task_space(tid).unwrap_or(u64::MAX);
        if self.count(space) >= PER_PROGRAM {
            return fail(tid, EMFILE);
        }
        let Some(i) = self.table.iter().position(|s| s.is_none()) else {
            return fail(tid, ENFILE);
        };
        let fd = match syscall::sys_fd_serve_ready(tid, SOCKET_COOKIE | i as u64, syscall::ANY_FD) {
            Ok(fd) => fd,
            Err(()) => return fail(tid, EMFILE),
        };
        self.table[i] = Some(Socket::new(kind, v6, space));
        ok(tid, [fd as u64, 0, 0, 0, 0, 0]);
        self.tell(net, i);
    }

    /// A datagram socket's two, made and bound to `local`: one on the card
    /// and one on `lo`.
    fn datagrams(&mut self, net: &mut Stack, i: usize, local: IpListenEndpoint) -> Result<(), u64> {
        let mut eth = new_datagram();
        let mut lo = new_datagram();
        if eth.bind(local).is_err() || lo.bind(local).is_err() {
            return Err(EINVAL);
        }
        let s = self.table[i].as_mut().unwrap();
        s.socket = Some((Side::Eth, net.sockets(Side::Eth).add(eth)));
        s.lo = Some(net.sockets(Side::Lo).add(lo));
        s.local = Some(local);
        Ok(())
    }

    /// A datagram socket bound, to a port of its own if it was not.
    fn bound_datagram(&mut self, net: &mut Stack, i: usize) -> Result<(), u64> {
        if self.sock(i).socket.is_some() {
            return Ok(());
        }
        let local = match self.sock(i).local {
            Some(l) => l,
            None => IpListenEndpoint { addr: None, port: self.ephemeral(net, Kind::Datagram).ok_or(EAGAIN)? },
        };
        self.datagrams(net, i, local)
    }

    fn bind(&mut self, net: &mut Stack, tid: usize, i: usize, famport: u64, a0: u64, a1: u64) {
        let (addr, port) = match self.address(i, famport, a0, a1, false) {
            Ok(e) => e,
            Err(e) => return fail(tid, e),
        };
        let s = self.sock(i);
        if s.local.is_some() || s.socket.is_some() || s.listen.is_some() {
            return fail(tid, EINVAL);
        }
        let kind = s.kind;
        // Only an address this machine has.
        if !addr.is_unspecified() && net.side_for(addr) != Side::Lo {
            return fail(tid, EADDRNOTAVAIL);
        }
        let port = if port == 0 {
            match self.ephemeral(net, kind) {
                Some(p) => p,
                None => return fail(tid, EADDRINUSE),
            }
        } else if self.port_taken(net, kind, port, i) {
            return fail(tid, EADDRINUSE);
        } else {
            port
        };
        let local = IpListenEndpoint { addr: (!addr.is_unspecified()).then_some(addr), port };
        if kind == Kind::Datagram {
            if let Err(e) = self.datagrams(net, i, local) {
                return fail(tid, e);
            }
        } else {
            self.sock(i).local = Some(local);
        }
        ok(tid, [0; 6]);
    }

    fn listen(&mut self, net: &mut Stack, tid: usize, i: usize, backlog: usize) {
        let backlog = backlog.clamp(1, MAX_BACKLOG);
        let s = self.sock(i);
        if s.kind != Kind::Stream {
            return fail(tid, EOPNOTSUPP);
        }
        if s.socket.is_some() {
            return fail(tid, EINVAL);
        }
        if let Some(id) = s.listen {
            net.set_backlog(id, backlog);
            return ok(tid, [0; 6]);
        }
        let local = match s.local {
            Some(l) => l,
            None => match self.ephemeral(net, Kind::Stream) {
                Some(port) => IpListenEndpoint { addr: None, port },
                None => return fail(tid, EADDRINUSE),
            },
        };
        match net.listen(local, backlog) {
            Some(id) => {
                let s = self.sock(i);
                s.local = Some(local);
                s.listen = Some(id);
                ok(tid, [0; 6]);
            }
            None => fail(tid, EADDRINUSE),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn connect(&mut self, net: &mut Stack, tid: usize, i: usize, famport: u64, a0: u64, a1: u64, may_wait: bool) {
        if self.sock(i).kind == Kind::Datagram {
            // A datagram socket's one correspondent: where what is written
            // goes, and the only one what is read comes from. Nobody, for
            // AF_UNSPEC.
            if famport >> 16 == AF_UNSPEC {
                self.sock(i).peer = None;
                return ok(tid, [0; 6]);
            }
            let (addr, port) = match self.address(i, famport, a0, a1, true) {
                Ok(e) => e,
                Err(e) => return fail(tid, e),
            };
            if let Err(e) = self.bound_datagram(net, i) {
                return fail(tid, e);
            }
            self.sock(i).peer = Some(IpEndpoint::new(addr, port));
            return ok(tid, [0; 6]);
        }
        let (addr, port) = match self.address(i, famport, a0, a1, false) {
            Ok(e) => e,
            Err(e) => return fail(tid, e),
        };
        let s = self.sock(i);
        if s.connecting.is_some() {
            return fail(tid, EALREADY);
        }
        if s.socket.is_some() || s.listen.is_some() {
            return fail(tid, EISCONN);
        }
        // Nobody in particular is this machine, as Linux has it.
        let addr = match addr {
            IpAddress::Ipv4(a) if a.is_unspecified() => IpAddress::v4(127, 0, 0, 1),
            IpAddress::Ipv6(a) if a.is_unspecified() => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1),
            a => a,
        };
        let (nodelay, keepalive, bound) = (s.nodelay, s.keepalive, s.local);
        let local = match bound {
            Some(l) => l,
            None => match self.ephemeral(net, Kind::Stream) {
                Some(port) => IpListenEndpoint { addr: None, port },
                None => return fail(tid, EADDRNOTAVAIL),
            },
        };
        let side = net.side_for(addr);
        let (sockets, cx) = net.parts(side);
        let mut t = new_stream();
        t.set_nagle_enabled(!nodelay);
        if keepalive {
            t.set_keep_alive(Some(Duration::from_secs(75)));
        }
        let h = sockets.add(t);
        if sockets.get_mut::<tcp::Socket>(h).connect(cx, IpEndpoint::new(addr, port), local).is_err() {
            sockets.remove(h);
            return fail(tid, if port == 0 { ECONNREFUSED } else { ENETUNREACH });
        }
        let s = self.sock(i);
        s.socket = Some((side, h));
        s.local = Some(local);
        s.connecting = Some(now());
        if !may_wait {
            return fail(tid, EINPROGRESS);
        }
        let _ = syscall::sys_task_watch(tid);
        s.held.push(Held::Connect { tid });
    }

    fn accept(&mut self, net: &mut Stack, tid: usize, i: usize, may_wait: bool) {
        if self.sock(i).listen.is_none() {
            return fail(tid, EINVAL);
        }
        if self.give_connection(net, tid, i) {
            return;
        }
        if !may_wait {
            return fail(tid, EAGAIN);
        }
        let _ = syscall::sys_task_watch(tid);
        self.sock(i).held.push(Held::Accept { tid });
    }

    /// The connection that has waited longest on listener `i`, given to
    /// `tid` as a descriptor of its own. False if none has come; true if
    /// `tid` has been answered, with one or with a refusal.
    fn give_connection(&mut self, net: &mut Stack, tid: usize, i: usize) -> bool {
        let s = self.sock(i);
        let (Some(id), v6, v6only, nodelay, keepalive) = (s.listen, s.v6, s.v6only, s.nodelay, s.keepalive) else {
            return false;
        };
        if !net.pending(id) {
            return false;
        }
        let space = syscall::sys_task_space(tid).unwrap_or(u64::MAX);
        if self.count(space) >= PER_PROGRAM {
            fail(tid, EMFILE);
            return true;
        }
        let Some(j) = self.table.iter().position(|s| s.is_none()) else {
            fail(tid, ENFILE);
            return true;
        };
        loop {
            let Some((side, h)) = net.accept(id) else { return false };
            let t = stream(net, side, h);
            let (remote, local) = (t.remote_endpoint(), t.local_endpoint());
            // A socket of IPv4's family hears nothing from IPv6, and one that
            // asked for IPv6 alone nothing from IPv4.
            let from_v6 = remote.is_some_and(|e| matches!(e.addr, IpAddress::Ipv6(_)));
            if (from_v6 && !v6) || (!from_v6 && v6only) {
                net.drop_stream(side, h);
                continue;
            }
            let fd = match syscall::sys_fd_serve_ready(tid, SOCKET_COOKIE | j as u64, syscall::ANY_FD) {
                Ok(fd) => fd,
                Err(()) => {
                    net.drop_stream(side, h);
                    fail(tid, EMFILE);
                    return true;
                }
            };
            // What the listener was told of itself, its connections are.
            let t = stream(net, side, h);
            t.set_nagle_enabled(!nodelay);
            if keepalive {
                t.set_keep_alive(Some(Duration::from_secs(75)));
            }
            let mut sock = Socket::new(Kind::Stream, v6, space);
            sock.socket = Some((side, h));
            sock.local = local.map(|e| IpListenEndpoint { addr: Some(e.addr), port: e.port });
            sock.nodelay = nodelay;
            sock.keepalive = keepalive;
            self.table[j] = Some(sock);
            let w = words_of(remote.map(|e| e.addr), remote.map_or(0, |e| e.port), v6);
            ok(tid, [fd as u64, w[0], w[1], w[2], 0, 0]);
            self.tell(net, j);
            return true;
        }
    }

    /// `len` bytes `tid` lent, to socket `i`'s correspondent or to `to`;
    /// held if there is no room yet and it may wait. `asked`: as a request,
    /// which is refused rather than told "nothing yet".
    #[allow(clippy::too_many_arguments)]
    fn send(&mut self, net: &mut Stack, tid: usize, i: usize, to: Option<IpEndpoint>, len: usize, may_wait: bool, asked: bool) {
        if self.try_send(net, tid, i, to, len) {
            return;
        }
        if !may_wait {
            return if asked { fail(tid, EAGAIN) } else { nothing_yet(tid) };
        }
        let _ = syscall::sys_task_watch(tid);
        self.sock(i).held.push(Held::Write { tid, len, to });
    }

    /// Answer `tid`'s send if it can be answered: sent, or refused. False if
    /// there is no room for it yet.
    fn try_send(&mut self, net: &mut Stack, tid: usize, i: usize, to: Option<IpEndpoint>, len: usize) -> bool {
        let s = self.sock(i);
        if s.kind == Kind::Stream {
            if s.connecting.is_some() {
                return false;
            }
            let Some((side, h)) = s.socket else {
                fail(tid, EPIPE);
                return true;
            };
            return bytes_from(net, tid, side, h, len);
        }
        let Some(to) = to.or(s.peer) else {
            fail(tid, EDESTADDRREQ);
            return true;
        };
        if let Err(e) = self.bound_datagram(net, i) {
            fail(tid, e);
            return true;
        }
        let s = self.sock(i);
        let side = net.side_for(to.addr);
        let h = match side {
            Side::Eth => s.socket.unwrap().1,
            Side::Lo => s.lo.unwrap(),
        };
        if len > datagram_room(side, to.addr) {
            fail(tid, EMSGSIZE);
            return true;
        }
        let mut buf = vec![0u8; len];
        if len > 0 && syscall::sys_lent_read(tid, 0, &mut buf) != Ok(len) {
            fail(tid, EFAULT);
            return true;
        }
        match net.send_datagram(side, h, &buf, to) {
            Ok(()) => ok(tid, [len as u64, 0, 0, 0, 0, 0]),
            Err(udp::SendError::BufferFull) => return false,
            Err(udp::SendError::Unaddressable) => fail(tid, if to.port == 0 { EINVAL } else { ENETUNREACH }),
        }
        true
    }

    /// What socket `i` has for `tid`, into the room it lent; held if there
    /// is nothing yet and it may wait. `asked`: as a request, answered with
    /// where it came from and refused rather than told "nothing yet".
    #[allow(clippy::too_many_arguments)]
    fn receive(&mut self, net: &mut Stack, tid: usize, i: usize, room: usize, may_wait: bool, asked: bool, peek: bool) {
        if self.try_receive(net, tid, i, room, asked, peek) {
            return;
        }
        if !may_wait {
            return if asked { fail(tid, EAGAIN) } else { nothing_yet(tid) };
        }
        let _ = syscall::sys_task_watch(tid);
        self.sock(i).held.push(Held::Read { tid, room, asked, peek });
    }

    /// Answer `tid`'s receive if there is anything to answer it with: bytes,
    /// a datagram, the end, a failure. False if there is not yet.
    fn try_receive(&mut self, net: &mut Stack, tid: usize, i: usize, room: usize, asked: bool, peek: bool) -> bool {
        let s = self.sock(i);
        if s.kind == Kind::Datagram {
            return self.datagram_to(net, tid, i, room, asked, peek);
        }
        if s.read_shut {
            ok(tid, [0; 6]);
            return true;
        }
        if s.connecting.is_some() {
            return false;
        }
        let Some((side, h)) = s.socket else {
            fail(tid, ENOTCONN);
            return true;
        };
        // What went wrong is said once, when there is nothing left to read:
        // to a receive as its errno, which is taken; to a read through the
        // descriptor, which can only fail, as a failure, and the error is
        // kept for option 1 to say which.
        if s.error != 0 && !s.error_told && !stream(net, side, h).can_recv() {
            let e = if asked {
                core::mem::take(&mut s.error)
            } else {
                s.error_told = true;
                s.error
            };
            fail(tid, e);
            return true;
        }
        bytes_to(net, tid, side, h, room, peek)
    }

    /// A datagram for `tid` from either of socket `i`'s, into the room it
    /// lent — or only looked at — with where it came from and how long it
    /// was, if `asked`. False if none waits.
    fn datagram_to(&mut self, net: &mut Stack, tid: usize, i: usize, room: usize, asked: bool, peek: bool) -> bool {
        let s = self.sock(i);
        let (Some((_, eth)), Some(lo)) = (s.socket, s.lo) else { return false };
        let (v6, peer) = (s.v6, s.peer);
        for (side, h) in [(Side::Eth, eth), (Side::Lo, lo)] {
            // A socket with a correspondent hears only from it.
            drop_strangers(net, side, h, peer);
            let d = datagram(net, side, h);
            if let Ok((len, from)) = d.peek().map(|(data, meta)| (data.len(), meta.endpoint)) {
                let data = if peek { d.peek().map(|(data, _)| data) } else { d.recv().map(|(data, _)| data) };
                let n = len.min(room);
                if n > 0 && data.is_ok_and(|data| syscall::sys_lent_write(tid, 0, &data[..n]).is_err()) {
                    fail(tid, EFAULT);
                    return true;
                }
                if asked {
                    let w = words_of(Some(from.addr), from.port, v6);
                    ok(tid, [n as u64, w[0], w[1], w[2], len as u64, 0]);
                } else {
                    ok(tid, [n as u64, 0, 0, 0, 0, 0]);
                }
                return true;
            }
        }
        false
    }

    /// `how`: 0 reading, 1 writing, 2 both, as `SHUT_RD`, `SHUT_WR` and
    /// `SHUT_RDWR`.
    fn shutdown(&mut self, net: &mut Stack, tid: usize, i: usize, how: u64) {
        if how > 2 {
            return fail(tid, EINVAL);
        }
        let s = self.sock(i);
        let (Kind::Stream, Some((side, h)), None) = (s.kind, s.socket, s.connecting) else {
            return fail(tid, ENOTCONN);
        };
        if how != 1 {
            s.read_shut = true;
        }
        if how != 0 && !s.write_shut {
            // What was written still goes, and then the end.
            s.write_shut = true;
            stream(net, side, h).close();
        }
        ok(tid, [0; 6]);
    }

    /// Where socket `i` is, or who is at the other end (`peer`).
    fn name(&mut self, net: &mut Stack, tid: usize, i: usize, peer: bool) {
        let s = self.sock(i);
        let (v6, local) = (s.v6, s.local);
        let ends = match (s.kind, s.socket, peer) {
            (Kind::Stream, Some((side, h)), true) if s.connecting.is_none() => {
                stream(net, side, h).remote_endpoint().map(|e| (Some(e.addr), e.port))
            }
            (Kind::Stream, Some((side, h)), false) => {
                stream(net, side, h).local_endpoint().map(|e| (Some(e.addr), e.port)).or(local.map(|l| (l.addr, l.port)))
            }
            (Kind::Datagram, _, true) => s.peer.map(|p| (Some(p.addr), p.port)),
            (_, _, false) => Some(local.map_or((None, 0), |l| (l.addr, l.port))),
            _ => None,
        };
        match ends {
            Some((addr, port)) => {
                let w = words_of(addr, port, v6);
                ok(tid, [w[0], w[1], w[2], 0, 0, 0]);
            }
            None => fail(tid, ENOTCONN),
        }
    }

    fn option(&mut self, net: &mut Stack, tid: usize, i: usize, name: u64, value: u64, set: bool) {
        let s = self.sock(i);
        let stream_kind = s.kind == Kind::Stream;
        let answer = match (name, set) {
            (OPT_ERROR, false) => core::mem::take(&mut s.error),
            (OPT_LISTENING, false) => s.listen.is_some() as u64,
            (OPT_TYPE, false) => {
                if stream_kind {
                    SOCK_STREAM
                } else {
                    SOCK_DGRAM
                }
            }
            (OPT_KEEPALIVE, false) => s.keepalive as u64,
            (OPT_NODELAY, false) => s.nodelay as u64,
            (OPT_V6ONLY, false) => s.v6only as u64,
            // What a read would find: a stream's bytes, or the next
            // datagram's length.
            (OPT_PENDING, false) => match (s.kind, s.socket, s.lo) {
                (Kind::Stream, Some((side, h)), _) => stream(net, side, h).recv_queue() as u64,
                (Kind::Datagram, Some((_, eth)), Some(lo)) => {
                    let peer = s.peer;
                    [(Side::Eth, eth), (Side::Lo, lo)]
                        .into_iter()
                        .find_map(|(side, h)| {
                            drop_strangers(net, side, h, peer);
                            datagram(net, side, h).peek().ok().map(|(data, _)| data.len() as u64)
                        })
                        .unwrap_or(0)
                }
                _ => 0,
            },
            (OPT_RCVBUF | OPT_SNDBUF, false) => (if stream_kind { STREAM_BUF } else { DATAGRAM_BUF }) as u64,
            // The buffers are what they are; asking for others is not wrong.
            (OPT_RCVBUF | OPT_SNDBUF, true) => 0,
            (OPT_KEEPALIVE, true) => {
                s.keepalive = value != 0;
                if let (Kind::Stream, Some((side, h))) = (s.kind, s.socket) {
                    let interval = (value != 0).then(|| Duration::from_secs(75));
                    stream(net, side, h).set_keep_alive(interval);
                }
                0
            }
            (OPT_NODELAY, true) if stream_kind => {
                s.nodelay = value != 0;
                if let Some((side, h)) = s.socket {
                    stream(net, side, h).set_nagle_enabled(value == 0);
                }
                0
            }
            (OPT_V6ONLY, true) => {
                if !s.v6 {
                    return fail(tid, ENOPROTOOPT);
                }
                if s.local.is_some() {
                    return fail(tid, EINVAL);
                }
                s.v6only = value != 0;
                0
            }
            _ => return fail(tid, ENOPROTOOPT),
        };
        ok(tid, [answer, 0, 0, 0, 0, 0]);
    }

    /// The kernel reading or writing socket `cookie` for `msg.sender`:
    /// `[cookie, room or length, flags]`.
    pub fn io(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let Some(i) = self.held_by(tid, msg.data[0]) else {
            return fail(tid, EBADF);
        };
        self.progress(net, i);
        let len = msg.data[1] as usize;
        let may_wait = msg.data[2] & syscall::FD_IO_DO_NOT_WAIT == 0;
        if msg.tag == TAG_FD_READ {
            self.receive(net, tid, i, len, may_wait, false, false);
        } else {
            self.send(net, tid, i, None, len, may_wait, false);
        }
        self.tell(net, i);
    }

    /// What smoltcp says of stream `i` since it was last asked: a
    /// connection made, refused or never answered; one ended by the other
    /// end's reset, or by nobody answering.
    fn progress(&mut self, net: &mut Stack, i: usize) {
        let s = self.table[i].as_mut().unwrap();
        let (Kind::Stream, Some((side, h))) = (s.kind, s.socket) else { return };
        let state = stream(net, side, h).state();
        if let Some(began) = s.connecting {
            match state {
                tcp::State::SynSent | tcp::State::SynReceived => {}
                tcp::State::Closed => {
                    s.connecting = None;
                    s.ended = true;
                    s.error = if now() - began + Duration::from_secs(1) >= TCP_TIMEOUT { ETIMEDOUT } else { ECONNREFUSED };
                }
                _ => s.connecting = None,
            }
        } else if state == tcp::State::Closed && !s.ended && !s.write_shut {
            // Closed without having been closed from here: reset.
            s.ended = true;
            s.error = ECONNRESET;
        }
    }

    /// What socket `i` is ready for: readable, writable, hung up.
    fn readiness(&mut self, net: &mut Stack, i: usize) -> u32 {
        let s = self.table[i].as_ref().unwrap();
        if let Some(id) = s.listen {
            return if net.pending(id) { FD_READY_READ } else { 0 };
        }
        match (s.kind, s.socket) {
            // Nothing to wait for: as Linux says of a stream never connected.
            (Kind::Stream, None) => FD_READY_WRITE | FD_READY_HANGUP,
            (Kind::Stream, Some(_)) if s.connecting.is_some() => 0,
            (Kind::Stream, Some((side, h))) => {
                let failed = s.ended || s.error != 0;
                let (read_shut, write_shut) = (s.read_shut, s.write_shut);
                let t = stream(net, side, h);
                let mut bits = 0;
                if t.can_recv() || !t.may_recv() || read_shut || failed {
                    bits |= FD_READY_READ;
                }
                if !write_shut && (t.can_send() || failed) {
                    bits |= FD_READY_WRITE;
                }
                if failed || (!t.may_recv() && write_shut) {
                    bits |= FD_READY_HANGUP;
                }
                bits
            }
            (Kind::Datagram, Some((_, eth))) => {
                let (lo, peer) = (s.lo.unwrap(), s.peer);
                drop_strangers(net, Side::Eth, eth, peer);
                drop_strangers(net, Side::Lo, lo, peer);
                let waiting = datagram(net, Side::Eth, eth).can_recv() || datagram(net, Side::Lo, lo).can_recv();
                FD_READY_WRITE | if waiting { FD_READY_READ } else { 0 }
            }
            (Kind::Datagram, None) => FD_READY_WRITE,
        }
    }

    /// Tell the kernel what socket `i` is ready for, if that has changed.
    fn tell(&mut self, net: &mut Stack, i: usize) {
        if self.table.get(i).is_none_or(|s| s.is_none()) {
            return;
        }
        let bits = self.readiness(net, i);
        let s = self.sock(i);
        if s.said != Some(bits) {
            s.said = Some(bits);
            let _ = syscall::sys_fd_ready(SOCKET_COOKIE | i as u64, bits);
        }
    }

    /// After a turn of the loop: what each socket has come to, held
    /// requests answered where they can be, and the kernel told what
    /// changed.
    pub fn settle(&mut self, net: &mut Stack) {
        for i in 0..self.table.len() {
            if self.table[i].is_none() {
                continue;
            }
            self.progress(net, i);
            self.answer_held(net, i);
            self.tell(net, i);
        }
    }

    /// Answer what socket `i`'s held requests wait for, where it has come.
    fn answer_held(&mut self, net: &mut Stack, i: usize) {
        let held = core::mem::take(&mut self.sock(i).held);
        if held.is_empty() {
            return;
        }
        let mut keep = Vec::new();
        for h in held {
            let done = match h {
                Held::Connect { tid } => {
                    let s = self.sock(i);
                    if s.connecting.is_some() {
                        false
                    } else {
                        match core::mem::take(&mut s.error) {
                            0 => ok(tid, [0; 6]),
                            e => fail(tid, e),
                        }
                        true
                    }
                }
                Held::Accept { tid } => self.give_connection(net, tid, i),
                Held::Read { tid, room, asked, peek } => self.try_receive(net, tid, i, room, asked, peek),
                Held::Write { tid, len, to } => self.try_send(net, tid, i, to, len),
            };
            if !done {
                keep.push(h);
            }
        }
        let s = self.sock(i);
        keep.append(&mut s.held);
        s.held = keep;
    }

    /// The last descriptor for socket `cookie` has gone. A stream says
    /// goodbye — or, with what was sent it never read, resets, as Linux
    /// does — and goes once it has; the rest go at once.
    pub fn closed(&mut self, net: &mut Stack, cookie: u64) {
        let i = Self::index(cookie);
        let Some(s) = self.table.get_mut(i).and_then(|s| s.take()) else { return };
        for h in &s.held {
            fail(h.tid(), EBADF);
        }
        if let Some(id) = s.listen {
            net.unlisten(id);
        }
        match (s.kind, s.socket) {
            (Kind::Stream, Some((side, h))) => {
                let unread = stream(net, side, h).can_recv();
                net.let_go(side, h, unread);
            }
            (Kind::Datagram, Some((side, h))) => {
                net.remove_datagram(side, h);
                if let Some(lo) = s.lo {
                    net.remove_datagram(Side::Lo, lo);
                }
            }
            _ => {}
        }
    }

    /// A line for each socket: what it is, where it is, and where it goes
    /// or what it waits for.
    pub fn describe(&self, net: &mut Stack, out: &mut String) {
        for s in self.table.iter().flatten() {
            let _ = write!(out, "  {}{}  ", if s.kind == Kind::Stream { "tcp" } else { "udp" }, if s.v6 { "6" } else { "4" });
            match (s.kind, s.socket) {
                (Kind::Stream, Some((side, h))) => {
                    let t = net.sockets(side).get::<tcp::Socket>(h);
                    write_end(out, t.local_endpoint().map(|e| (Some(e.addr), e.port)));
                    out.push_str("  ");
                    write_end(out, t.remote_endpoint().map(|e| (Some(e.addr), e.port)));
                    let _ = write!(out, "  {}", t.state());
                }
                _ => {
                    write_end(out, s.local.map(|l| (l.addr, l.port)));
                    if s.listen.is_some() {
                        out.push_str("  listening");
                    } else if let Some(p) = s.peer {
                        out.push_str("  ");
                        write_end(out, Some((Some(p.addr), p.port)));
                    }
                }
            }
            out.push('\n');
        }
    }

    /// `tid` is not waiting for anything here any more: it asked something
    /// else, or died.
    pub fn forget(&mut self, tid: usize) {
        for s in self.table.iter_mut().flatten() {
            s.held.retain(|h| h.tid() != tid);
        }
    }
}
