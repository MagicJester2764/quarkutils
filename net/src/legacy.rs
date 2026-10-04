//! The protocol programs speak to this server — `quark_rt::net`, and a TCP
//! connection's descriptor (`FdKind::Socket`) — kept as it was, on the
//! protocols smoltcp keeps.
//!
//! A connection is a number in a table here, the program that asked for it
//! its only user, and a socket in one of the stack's sets. What a program
//! waits for — a connection to be made or come, bytes to come, room for its
//! bytes, a datagram, an echo, a name — is held with its reply and answered
//! when the stack has it ([`Clients::settle`], after every turn of the
//! loop), or when it gives up waiting. A program that asks anything else
//! is not waiting any more; one that dies takes its connections with it.

use alloc::vec;
use alloc::vec::Vec;
use quark_rt::ipc::Message;
use quark_rt::syscall;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{dns, icmp, tcp, udp};
use smoltcp::time::Duration;
use smoltcp::wire::{DnsQueryType, Icmpv4Packet, Icmpv4Repr, IpAddress, IpEndpoint, Ipv4Address};

use crate::stack::{Side, Stack};

pub const TAG_UDP_SEND: u64 = 1;
pub const TAG_UDP_RECV: u64 = 2;
pub const TAG_NET_INFO: u64 = 4;
pub const TAG_ICMP_PING: u64 = 5;
pub const TAG_DNS_RESOLVE: u64 = 7;
pub const TAG_TCP_CONNECT: u64 = 10;
pub const TAG_TCP_LISTEN: u64 = 11;
pub const TAG_TCP_SEND: u64 = 13;
pub const TAG_TCP_RECV: u64 = 14;
pub const TAG_TCP_CLOSE: u64 = 15;
/// Read and write for a connection reached through a descriptor: the
/// kernel's fd path packs a chunk of the program's buffer into the message,
/// and the connection travels in the tag's upper half.
pub const TAG_SOCK_WRITE: u64 = 16;
pub const TAG_SOCK_READ: u64 = 17;
const SOCK_TAG_MASK: u64 = 0xFFFF_FFFF;
const SOCK_HANDLE_SHIFT: u32 = 32;
const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;

/// Connections there may be at once.
const MAX_CONNS: usize = 64;
/// Of which one program may hold.
const MAX_CONNS_PER_TASK: usize = MAX_CONNS / 2;
/// Each way, a connection's buffer.
const TCP_BUF: usize = 64 * 1024;
/// A connection nothing is heard on for this long is given up: a connect
/// to somewhere that never answers, too.
const TCP_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a datagram, an echo and a name are waited for.
const UDP_WAIT_MS: u64 = 10_000;
const PING_WAIT_MS: u64 = 3_000;
const DNS_WAIT_MS: u64 = 5_000;
/// What the kernel's fd path packs into one message.
const SOCK_CHUNK: usize = 40;
/// The longest datagram a program sends or receives here.
const MAX_DATAGRAM: usize = 1472;
/// What a program sends or receives through `TAG_TCP_SEND` and `RECV` at once.
const MAX_SEGMENT_IO: usize = 4096;
/// The error for a request that has to wait behind another task's.
const ERR_BUSY: u64 = 4;
/// Names remembered, and for how long.
const DNS_CACHE: usize = 8;
const DNS_KEEP_MS: u64 = 300_000;
/// Where the ports this server chooses come from.
const EPHEMERAL: (u16, u16) = (49152, 65535);

/// What a program waits for on one of its connections.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wait {
    None,
    /// The connection to be made.
    Connect,
    /// A connection to come to a listener.
    Accept,
    /// Bytes, or the end: into the buffer it lent, or inline.
    Recv { max: usize, inline: bool },
    /// Room for the bytes it sent inline, which are held here.
    Send,
}

struct Conn {
    owner: usize,
    side: Side,
    socket: SocketHandle,
    /// A listener waiting for its first connection is a socket in each set;
    /// this is the other.
    twin: Option<SocketHandle>,
    wait: Wait,
    held: [u8; SOCK_CHUNK],
    held_len: usize,
}

struct UdpReader {
    tid: usize,
    port: u16,
    max: usize,
    since: u64,
}

struct Ping {
    tid: usize,
    id: u16,
    seq: u16,
    since: u64,
    since_ticks: u64,
}

struct Lookup {
    tid: usize,
    name: [u8; 48],
    len: usize,
    query: dns::QueryHandle,
    since: u64,
}

struct Cached {
    name: [u8; 48],
    len: usize,
    addr: Ipv4Address,
    until: u64,
}

pub struct Clients {
    conns: Vec<Option<Conn>>,
    /// A UDP socket for each port a program has used, eth's and lo's.
    udp: Vec<(u16, SocketHandle, SocketHandle)>,
    reader: Option<UdpReader>,
    icmp: Option<SocketHandle>,
    ping: Option<Ping>,
    dns: Option<SocketHandle>,
    lookup: Option<Lookup>,
    cache: Vec<Cached>,
    next_port: u16,
}

fn ms() -> u64 {
    syscall::sys_clock() / 1_000_000
}

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

fn ok(tid: usize, words: [u64; 6]) {
    reply(tid, TAG_OK, words);
}

fn refuse(tid: usize, code: u64) {
    reply(tid, TAG_ERROR, [code, 0, 0, 0, 0, 0]);
}

fn v4(word: u64) -> Ipv4Address {
    let b = (word as u32).to_be_bytes();
    Ipv4Address::new(b[0], b[1], b[2], b[3])
}

fn word_of(addr: IpAddress) -> u64 {
    match addr {
        IpAddress::Ipv4(a) => u32::from_be_bytes(a.octets()) as u64,
        IpAddress::Ipv6(_) => 0,
    }
}

/// Bytes into the layout the kernel's fd path unpacks: the count in the
/// first word, the bytes after it.
fn pack(src: &[u8]) -> [u64; 6] {
    let n = src.len().min(SOCK_CHUNK);
    let mut words = [0u64; 6];
    words[0] = n as u64;
    let mut bytes = [0u8; SOCK_CHUNK];
    bytes[..n].copy_from_slice(&src[..n]);
    for i in 0..5 {
        words[i + 1] = u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
    }
    words
}

fn unpack(words: &[u64; 6], out: &mut [u8; SOCK_CHUNK]) -> usize {
    let n = (words[0] as usize).min(SOCK_CHUNK);
    for i in 0..5 {
        out[i * 8..i * 8 + 8].copy_from_slice(&words[i + 1].to_le_bytes());
    }
    n
}

fn new_tcp() -> tcp::Socket<'static> {
    let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0; TCP_BUF]), tcp::SocketBuffer::new(vec![0; TCP_BUF]));
    s.set_timeout(Some(TCP_TIMEOUT));
    s
}

fn new_udp() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16 * MAX_DATAGRAM]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 16], vec![0; 16 * MAX_DATAGRAM]),
    )
}

impl Clients {
    pub fn new() -> Clients {
        let mut conns = Vec::new();
        conns.resize_with(MAX_CONNS, || None);
        Clients {
            conns,
            udp: Vec::new(),
            reader: None,
            icmp: None,
            ping: None,
            dns: None,
            lookup: None,
            cache: Vec::new(),
            next_port: EPHEMERAL.0,
        }
    }

    fn ephemeral(&mut self) -> u16 {
        let p = self.next_port;
        self.next_port = if p >= EPHEMERAL.1 { EPHEMERAL.0 } else { p + 1 };
        p
    }

    fn owned(&self, handle: usize, tid: usize) -> bool {
        self.conns.get(handle).is_some_and(|c| c.as_ref().is_some_and(|c| c.owner == tid))
    }

    fn free_slot(&self, tid: usize) -> Option<usize> {
        if self.conns.iter().flatten().filter(|c| c.owner == tid).count() >= MAX_CONNS_PER_TASK {
            return None;
        }
        self.conns.iter().position(|c| c.is_none())
    }

    fn tcp<'a>(net: &'a mut Stack, side: Side, socket: SocketHandle) -> &'a mut tcp::Socket<'static> {
        net.sockets(side).get_mut::<tcp::Socket>(socket)
    }

    /// A request from `msg.sender`.
    pub fn request(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        if msg.tag == quark_rt::ipc::TAG_PING {
            reply(tid, quark_rt::ipc::TAG_PING, [0; 6]);
            return;
        }
        let handle = (msg.tag >> SOCK_HANDLE_SHIFT) as usize;
        match msg.tag & SOCK_TAG_MASK {
            TAG_UDP_SEND => self.udp_send(net, msg),
            TAG_UDP_RECV => self.udp_recv(net, msg),
            TAG_ICMP_PING => self.ping(net, msg),
            TAG_NET_INFO => {
                let mac = net.mac;
                let packed = mac.iter().enumerate().fold(0u64, |w, (i, &b)| w | (b as u64) << (8 * i));
                ok(tid, [packed, net.ipv4_word() as u64, 0, 0, 0, 0]);
            }
            TAG_DNS_RESOLVE => self.resolve(net, msg),
            TAG_TCP_CONNECT => self.connect(net, msg),
            TAG_TCP_LISTEN => self.listen(net, msg),
            TAG_TCP_SEND => self.tcp_send(net, msg),
            TAG_TCP_RECV => self.tcp_recv(net, msg),
            TAG_SOCK_WRITE => self.sock_write(net, tid, handle, &msg.data),
            TAG_SOCK_READ => self.sock_read(net, tid, handle, msg.data[0] as usize),
            TAG_TCP_CLOSE => self.close(net, tid, msg.data[0] as usize),
            _ => refuse(tid, 0xFF),
        }
    }

    // --- TCP ---

    fn connect(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let dst = v4(msg.data[0]);
        let dst_port = (msg.data[1] >> 16) as u16;
        let src_port = match (msg.data[1] & 0xFFFF) as u16 {
            0 => self.ephemeral(),
            p => p,
        };
        let Some(i) = self.free_slot(tid) else {
            return refuse(tid, 1);
        };
        let side = net.side_for(IpAddress::Ipv4(dst));
        let (sockets, cx) = net.parts(side);
        let socket = sockets.add(new_tcp());
        if sockets.get_mut::<tcp::Socket>(socket).connect(cx, (IpAddress::Ipv4(dst), dst_port), src_port).is_err() {
            sockets.remove(socket);
            return refuse(tid, 2);
        }
        let _ = syscall::sys_task_watch(tid);
        self.conns[i] = Some(Conn {
            owner: tid,
            side,
            socket,
            twin: None,
            wait: Wait::Connect,
            held: [0; SOCK_CHUNK],
            held_len: 0,
        });
    }

    /// A listener for one connection on a port: a socket in each set, and
    /// the program waits until one of them has been connected to.
    fn listen(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let port = msg.data[0] as u16;
        let Some(i) = self.free_slot(tid) else {
            return refuse(tid, 1);
        };
        let mut eth = new_tcp();
        let mut lo = new_tcp();
        if port == 0 || eth.listen(port).is_err() || lo.listen(port).is_err() {
            return refuse(tid, 2);
        }
        let socket = net.sockets(Side::Eth).add(eth);
        let twin = net.sockets(Side::Lo).add(lo);
        let _ = syscall::sys_task_watch(tid);
        self.conns[i] = Some(Conn {
            owner: tid,
            side: Side::Eth,
            socket,
            twin: Some(twin),
            wait: Wait::Accept,
            held: [0; SOCK_CHUNK],
            held_len: 0,
        });
    }

    fn tcp_send(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let handle = msg.data[0] as usize;
        if !self.owned(handle, tid) {
            return refuse(tid, 1);
        }
        let c = self.conns[handle].as_ref().unwrap();
        let s = Self::tcp(net, c.side, c.socket);
        if !s.may_send() {
            return refuse(tid, 2);
        }
        let len = (msg.data[2] as usize).min(MAX_SEGMENT_IO).min(s.send_capacity() - s.send_queue());
        let mut buf = [0u8; MAX_SEGMENT_IO];
        let sent = if len > 0 && syscall::sys_lent_read(tid, 0, &mut buf[..len]) == Ok(len) {
            s.send_slice(&buf[..len]).unwrap_or(0)
        } else {
            0
        };
        ok(tid, [sent as u64, 0, 0, 0, 0, 0]);
    }

    fn tcp_recv(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let handle = msg.data[0] as usize;
        if !self.owned(handle, tid) {
            return refuse(tid, 1);
        }
        let max = (msg.data[2] as usize).min(MAX_SEGMENT_IO);
        self.conns[handle].as_mut().unwrap().wait = Wait::Recv { max, inline: false };
        self.deliver(net, handle);
    }

    fn sock_write(&mut self, net: &mut Stack, tid: usize, handle: usize, data: &[u64; 6]) {
        if !self.owned(handle, tid) {
            return refuse(tid, 1);
        }
        let mut bytes = [0u8; SOCK_CHUNK];
        let n = unpack(data, &mut bytes);
        let c = self.conns[handle].as_mut().unwrap();
        let s = Self::tcp(net, c.side, c.socket);
        if !s.may_send() {
            return refuse(tid, 2);
        }
        // Whole or not at all: the kernel's fd path reports what it handed
        // over, not what was taken. No room is a write that waits.
        if s.send_capacity() - s.send_queue() >= n {
            let sent = s.send_slice(&bytes[..n]).unwrap_or(0);
            ok(tid, [sent as u64, 0, 0, 0, 0, 0]);
        } else {
            c.held = bytes;
            c.held_len = n;
            c.wait = Wait::Send;
        }
    }

    fn sock_read(&mut self, net: &mut Stack, tid: usize, handle: usize, max: usize) {
        if !self.owned(handle, tid) {
            return refuse(tid, 1);
        }
        self.conns[handle].as_mut().unwrap().wait = Wait::Recv { max: max.min(SOCK_CHUNK), inline: true };
        self.deliver(net, handle);
    }

    /// What connection `handle`'s program waits for, if it has come: bytes
    /// or the end for a read, room for a write.
    fn deliver(&mut self, net: &mut Stack, handle: usize) {
        let Some(c) = self.conns[handle].as_mut() else { return };
        let tid = c.owner;
        match c.wait {
            Wait::Recv { max, inline } => {
                let s = Self::tcp(net, c.side, c.socket);
                if s.can_recv() {
                    let mut buf = [0u8; MAX_SEGMENT_IO];
                    let n = s.recv_slice(&mut buf[..max]).unwrap_or(0);
                    c.wait = Wait::None;
                    if inline {
                        reply(tid, TAG_OK, pack(&buf[..n]));
                    } else if n == 0 || syscall::sys_lent_write(tid, 0, &buf[..n]).is_ok() {
                        ok(tid, [n as u64, 0, 0, 0, 0, 0]);
                    } else {
                        refuse(tid, 3);
                    }
                } else if !s.may_recv() {
                    // The other end has finished, or the connection is gone:
                    // the end of the stream.
                    c.wait = Wait::None;
                    if inline {
                        reply(tid, TAG_OK, [0; 6]);
                    } else {
                        ok(tid, [0; 6]);
                    }
                }
            }
            Wait::Send => {
                let s = Self::tcp(net, c.side, c.socket);
                if !s.may_send() {
                    c.wait = Wait::None;
                    refuse(tid, 2);
                } else if s.send_capacity() - s.send_queue() >= c.held_len {
                    let sent = s.send_slice(&c.held[..c.held_len]).unwrap_or(0);
                    c.wait = Wait::None;
                    ok(tid, [sent as u64, 0, 0, 0, 0, 0]);
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, net: &mut Stack, tid: usize, handle: usize) {
        // A close of what is not the caller's is nothing to do, and says
        // nothing about whether the number is in use.
        if self.owned(handle, tid) {
            self.drop_conn(net, handle);
        }
        ok(tid, [0; 6]);
    }

    /// Connection `handle` is its program's no more: its socket says
    /// goodbye, and goes once it has; a listener's sockets go at once.
    fn drop_conn(&mut self, net: &mut Stack, handle: usize) {
        let Some(c) = self.conns[handle].take() else { return };
        if let Some(twin) = c.twin {
            net.drop_stream(Side::Lo, twin);
            net.drop_stream(c.side, c.socket);
            return;
        }
        Self::tcp(net, c.side, c.socket).close();
        net.retire(c.side, c.socket);
    }

    // --- UDP ---

    /// The UDP socket on `port`, eth's and lo's, made if there is none.
    fn udp_on(&mut self, net: &mut Stack, port: u16) -> Option<(SocketHandle, SocketHandle)> {
        if let Some(&(_, e, l)) = self.udp.iter().find(|(p, _, _)| *p == port) {
            return Some((e, l));
        }
        let mut eth = new_udp();
        let mut lo = new_udp();
        if eth.bind(port).is_err() || lo.bind(port).is_err() {
            return None;
        }
        let e = net.sockets(Side::Eth).add(eth);
        let l = net.sockets(Side::Lo).add(lo);
        self.udp.push((port, e, l));
        Some((e, l))
    }

    fn udp_send(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let len = (msg.data[1] as usize).min(MAX_DATAGRAM);
        let dst = IpAddress::Ipv4(v4(msg.data[2]));
        let dst_port = (msg.data[3] >> 16) as u16;
        let src_port = match (msg.data[3] & 0xFFFF) as u16 {
            0 => self.ephemeral(),
            p => p,
        };
        let mut payload = [0u8; MAX_DATAGRAM];
        if len == 0 || syscall::sys_lent_read(tid, 0, &mut payload[..len]) != Ok(len) {
            return refuse(tid, 1);
        }
        let Some((e, l)) = self.udp_on(net, src_port) else {
            return refuse(tid, 1);
        };
        let side = net.side_for(dst);
        let socket = if side == Side::Eth { e } else { l };
        let sent = net.sockets(side).get_mut::<udp::Socket>(socket).send_slice(&payload[..len], IpEndpoint::new(dst, dst_port));
        if sent.is_ok() { ok(tid, [0; 6]) } else { refuse(tid, 1) }
    }

    fn udp_recv(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        if self.reader.is_some() {
            return refuse(tid, ERR_BUSY);
        }
        let port = msg.data[2] as u16;
        if self.udp_on(net, port).is_none() {
            return refuse(tid, 1);
        }
        let _ = syscall::sys_task_watch(tid);
        self.reader = Some(UdpReader { tid, port, max: msg.data[1] as usize, since: ms() });
    }

    fn deliver_datagram(&mut self, net: &mut Stack) {
        let Some(r) = self.reader.as_ref() else { return };
        let Some(&(_, e, l)) = self.udp.iter().find(|(p, _, _)| *p == r.port) else { return };
        for (side, socket) in [(Side::Eth, e), (Side::Lo, l)] {
            let s = net.sockets(side).get_mut::<udp::Socket>(socket);
            if !s.can_recv() {
                continue;
            }
            let mut buf = [0u8; MAX_DATAGRAM];
            let Ok((n, meta)) = s.recv_slice(&mut buf) else { continue };
            let r = self.reader.take().unwrap();
            let n = n.min(r.max);
            if syscall::sys_lent_write(r.tid, 0, &buf[..n]).is_err() {
                return refuse(r.tid, 2);
            }
            let ports = (meta.endpoint.port as u64) << 16 | r.port as u64;
            return ok(r.tid, [n as u64, word_of(meta.endpoint.addr), ports, 0, 0, 0]);
        }
        if ms() - r.since > UDP_WAIT_MS {
            let r = self.reader.take().unwrap();
            refuse(r.tid, 3);
        }
    }

    // --- echo ---

    fn ping(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        if self.ping.is_some() {
            return refuse(tid, ERR_BUSY);
        }
        let dst = IpAddress::Ipv4(v4(msg.data[0]));
        let id = msg.data[1] as u16;
        let seq = msg.data[2] as u16;
        let side = net.side_for(dst);
        let socket = match self.icmp {
            Some(s) => s,
            None => {
                let s = icmp::Socket::new(
                    icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 8 * 256]),
                    icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 8 * 256]),
                );
                let h = net.sockets(Side::Eth).add(s);
                self.icmp = Some(h);
                h
            }
        };
        // Echo to this machine is answered without a socket on lo: the
        // stack answers an echo itself, and what comes back is the reply.
        let _ = side;
        let s = net.sockets(Side::Eth).get_mut::<icmp::Socket>(socket);
        if !s.is_open() {
            let _ = s.bind(icmp::Endpoint::Ident(id));
        }
        let repr = Icmpv4Repr::EchoRequest { ident: id, seq_no: seq, data: b"quark ping......" };
        let sent = s.send(repr.buffer_len(), dst).map(|buf| {
            let mut packet = Icmpv4Packet::new_unchecked(buf);
            repr.emit(&mut packet, &smoltcp::phy::ChecksumCapabilities::default());
        });
        if sent.is_err() {
            return refuse(tid, 2);
        }
        let _ = syscall::sys_task_watch(tid);
        self.ping = Some(Ping { tid, id, seq, since: ms(), since_ticks: syscall::sys_ticks() });
    }

    fn deliver_echo(&mut self, net: &mut Stack) {
        let (Some(p), Some(socket)) = (self.ping.as_ref(), self.icmp) else { return };
        let s = net.sockets(Side::Eth).get_mut::<icmp::Socket>(socket);
        while s.can_recv() {
            let Ok((data, _from)) = s.recv() else { break };
            let packet = Icmpv4Packet::new_unchecked(data);
            if let Ok(Icmpv4Repr::EchoReply { ident, seq_no, data }) =
                Icmpv4Repr::parse(&packet, &smoltcp::phy::ChecksumCapabilities::default())
            {
                if ident == p.id && seq_no == p.seq {
                    let p = self.ping.take().unwrap();
                    let rtt = syscall::sys_ticks() - p.since_ticks;
                    // How many hops it came: not said by the socket, so the
                    // most an answer starts with.
                    return ok(p.tid, [rtt, 64, data.len() as u64 + 8, 0, 0, 0]);
                }
            }
        }
        if ms() - p.since > PING_WAIT_MS {
            let p = self.ping.take().unwrap();
            refuse(p.tid, 1);
        }
    }

    // --- names ---

    fn resolve(&mut self, net: &mut Stack, msg: &Message) {
        let tid = msg.sender;
        let mut name = [0u8; 48];
        for i in 0..6 {
            name[i * 8..i * 8 + 8].copy_from_slice(&msg.data[i].to_le_bytes());
        }
        let len = name.iter().position(|&b| b == 0).unwrap_or(48);
        // A name that is not one is refused here rather than asked of the
        // network: a query leaves the machine, and whatever is upstream
        // reads it.
        if !quark_rt::net::valid_hostname(&name[..len]) {
            return refuse(tid, 1);
        }
        let now_ms = ms();
        self.cache.retain(|c| c.until > now_ms);
        if let Some(c) = self.cache.iter().find(|c| c.name[..c.len] == name[..len]) {
            return ok(tid, [u32::from_be_bytes(c.addr.octets()) as u64, 0, 0, 0, 0, 0]);
        }
        if self.lookup.is_some() {
            return refuse(tid, 4);
        }
        let socket = match self.dns {
            Some(s) => s,
            None => {
                let servers = net.dns();
                let s = net.sockets(Side::Eth).add(dns::Socket::new(&servers, vec![]));
                self.dns = Some(s);
                s
            }
        };
        let servers = net.dns();
        let (sockets, cx) = net.parts(Side::Eth);
        let d = sockets.get_mut::<dns::Socket>(socket);
        d.update_servers(&servers);
        let Ok(text) = core::str::from_utf8(&name[..len]) else {
            return refuse(tid, 1);
        };
        match d.start_query(cx, text, DnsQueryType::A) {
            Ok(query) => {
                let _ = syscall::sys_task_watch(tid);
                self.lookup = Some(Lookup { tid, name, len, query, since: now_ms });
            }
            Err(_) => refuse(tid, 5),
        }
    }

    fn deliver_name(&mut self, net: &mut Stack) {
        let (Some(l), Some(socket)) = (self.lookup.as_ref(), self.dns) else { return };
        let d = net.sockets(Side::Eth).get_mut::<dns::Socket>(socket);
        match d.get_query_result(l.query) {
            Err(dns::GetQueryResultError::Pending) => {
                if ms() - l.since > DNS_WAIT_MS {
                    d.cancel_query(l.query);
                    let l = self.lookup.take().unwrap();
                    refuse(l.tid, 3);
                }
            }
            Err(_) => {
                let l = self.lookup.take().unwrap();
                refuse(l.tid, 2);
            }
            Ok(addrs) => {
                let l = self.lookup.take().unwrap();
                match addrs.iter().find_map(|a| match a {
                    IpAddress::Ipv4(v) => Some(*v),
                    _ => None,
                }) {
                    Some(addr) => {
                        if self.cache.len() >= DNS_CACHE {
                            self.cache.remove(0);
                        }
                        self.cache.push(Cached { name: l.name, len: l.len, addr, until: ms() + DNS_KEEP_MS });
                        ok(l.tid, [u32::from_be_bytes(addr.octets()) as u64, 0, 0, 0, 0, 0]);
                    }
                    None => refuse(l.tid, 2),
                }
            }
        }
    }

    // --- after every turn ---

    /// Answer whoever waits for what the stack now has.
    pub fn settle(&mut self, net: &mut Stack) {
        for i in 0..self.conns.len() {
            let Some(c) = self.conns[i].as_mut() else { continue };
            let tid = c.owner;
            match c.wait {
                Wait::Connect => {
                    let state = Self::tcp(net, c.side, c.socket).state();
                    match state {
                        tcp::State::Established => {
                            c.wait = Wait::None;
                            ok(tid, [i as u64, 0, 0, 0, 0, 0]);
                        }
                        tcp::State::Closed => {
                            // Refused, or nobody answered in time.
                            let c = self.conns[i].take().unwrap();
                            net.sockets(c.side).remove(c.socket);
                            refuse(tid, 3);
                        }
                        _ => {}
                    }
                }
                Wait::Accept => {
                    let twin = c.twin.unwrap();
                    let eth = Self::tcp(net, Side::Eth, c.socket).state();
                    let lo = Self::tcp(net, Side::Lo, twin).state();
                    let came = if !matches!(eth, tcp::State::Listen | tcp::State::SynReceived) {
                        Some((Side::Eth, c.socket, Side::Lo, twin))
                    } else if !matches!(lo, tcp::State::Listen | tcp::State::SynReceived) {
                        Some((Side::Lo, twin, Side::Eth, c.socket))
                    } else {
                        None
                    };
                    if let Some((side, socket, other_side, other)) = came {
                        net.drop_stream(other_side, other);
                        c.side = side;
                        c.socket = socket;
                        c.twin = None;
                        c.wait = Wait::None;
                        let remote = Self::tcp(net, side, socket).remote_endpoint();
                        let (addr, port) = remote.map_or((0, 0), |e| (word_of(e.addr), e.port as u64));
                        ok(tid, [i as u64, addr, port, 0, 0, 0]);
                    }
                }
                Wait::Recv { .. } | Wait::Send => self.deliver(net, i),
                Wait::None => {}
            }
        }
        self.deliver_datagram(net);
        self.deliver_echo(net);
        self.deliver_name(net);
    }

    /// `tid` is not waiting for anything here any more: a connection it was
    /// waiting to be made, or to come, is not wanted, and nor is a name.
    pub fn abandon(&mut self, net: &mut Stack, tid: usize) {
        if let (Some(l), Some(socket)) = (self.lookup.as_ref(), self.dns) {
            if l.tid == tid {
                net.sockets(Side::Eth).get_mut::<dns::Socket>(socket).cancel_query(l.query);
                self.lookup = None;
            }
        }
        for i in 0..self.conns.len() {
            let Some(c) = self.conns[i].as_mut() else { continue };
            if c.owner != tid {
                continue;
            }
            match c.wait {
                Wait::Connect | Wait::Accept => self.drop_conn(net, i),
                _ => c.wait = Wait::None,
            }
        }
        if self.reader.as_ref().is_some_and(|r| r.tid == tid) {
            self.reader = None;
        }
        if self.ping.as_ref().is_some_and(|p| p.tid == tid) {
            self.ping = None;
        }
    }

    /// `tid` has died: its connections go, and anything it waited for.
    pub fn gone(&mut self, net: &mut Stack, tid: usize) {
        self.abandon(net, tid);
        for i in 0..self.conns.len() {
            if self.conns[i].as_ref().is_some_and(|c| c.owner == tid) {
                self.drop_conn(net, i);
            }
        }
    }
}
