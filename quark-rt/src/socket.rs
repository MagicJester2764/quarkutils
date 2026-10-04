//! Sockets as descriptors: TCP and UDP, IPv4 and IPv6.
//!
//! A socket is a descriptor the net server serves. Reading and writing one
//! are the same calls as for a pipe or a file — the kernel lends the buffer
//! to the server — and a poll watches one by what the server says it is
//! ready for. Everything else is a request to the server naming the socket
//! (`TAG_SOCKET`), which believes it only of a program that holds the
//! descriptor; the server is found by name, once, to make a socket.
//! Closing the last descriptor closes the socket, so a socket is shared by
//! `dup` and `fork` as any descriptor is.
//!
//! A request's first word is the operation with flags above it; an address
//! goes as three words: the family and the port, then the sixteen bytes of
//! the address as they are on the wire, read as two little-endian words. A
//! refusal says Linux's errno, which a C library hands on as it is; here it
//! is [`Error`], coarser. The net server's `docs/net.md` has the words of
//! every request.

use core::cell::Cell;

use crate::ipc::Message;
use crate::nameserver;
use crate::net;
use crate::syscall;

/// Anything that went wrong, as finely as a program can do something about
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// No net server is registered.
    NoService,
    /// The name could not be resolved.
    NoHost,
    /// The connection was refused, or there is no way to where it was going.
    ConnectFailed,
    /// The fd table is full, or the server's.
    NoDescriptor,
    /// The connection is over: nothing more can be sent.
    Closed,
    /// The transfer failed.
    Io,
    /// Nothing yet, and the socket was not to wait.
    WouldBlock,
    /// It did not happen in the time there was.
    TimedOut,
    /// Somebody else is at that address and port.
    AddrInUse,
    /// The address is not one of this machine's.
    AddrNotAvailable,
    /// The other end reset the connection.
    Reset,
    /// The socket is not connected.
    NotConnected,
    /// Not something this socket can be asked, or not in its family.
    Invalid,
}

impl Error {
    /// What the net server's errno means.
    pub fn of(errno: u64) -> Error {
        match errno {
            11 | 115 => Error::WouldBlock,
            110 => Error::TimedOut,
            98 => Error::AddrInUse,
            99 => Error::AddrNotAvailable,
            104 => Error::Reset,
            107 => Error::NotConnected,
            32 => Error::Closed,
            23 | 24 => Error::NoDescriptor,
            101 | 111 | 113 => Error::ConnectFailed,
            22 | 89 | 90 | 92 | 93 | 94 | 95 | 97 | 106 | 114 => Error::Invalid,
            _ => Error::Io,
        }
    }
}

/// How long to wait for the net server to register itself.
const SERVICE_ATTEMPTS: usize = 50;

const TAG_SOCKET: u64 = 20;
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
/// Beside an operation: the caller is not to wait; a receive only looks.
const DO_NOT_WAIT: u64 = 1 << 8;
const PEEK: u64 = 2 << 8;
const OPT_ERROR: u64 = 1;
const OPT_NODELAY: u64 = 3;
const OPT_V6ONLY: u64 = 6;
const AF_INET: u64 = 2;
const AF_INET6: u64 = 10;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
/// As many connections waiting to be accepted as the server keeps.
const BACKLOG: u64 = 128;

fn net_tid() -> Result<usize, Error> {
    nameserver::lookup_retry(b"net", SERVICE_ATTEMPTS).ok_or(Error::NoService)
}

/// An address on the network.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addr {
    V4([u8; 4]),
    V6([u8; 16]),
}

/// An address and a port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    pub addr: Addr,
    pub port: u16,
}

impl Endpoint {
    pub fn v4(ip: [u8; 4], port: u16) -> Endpoint {
        Endpoint { addr: Addr::V4(ip), port }
    }

    pub fn v6(ip: [u8; 16], port: u16) -> Endpoint {
        Endpoint { addr: Addr::V6(ip), port }
    }

    fn family(&self) -> u64 {
        match self.addr {
            Addr::V4(_) => AF_INET,
            Addr::V6(_) => AF_INET6,
        }
    }

    fn words(&self) -> [u64; 3] {
        let mut b = [0u8; 16];
        match self.addr {
            Addr::V4(a) => b[..4].copy_from_slice(&a),
            Addr::V6(a) => b = a,
        }
        [
            self.family() << 16 | self.port as u64,
            u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])),
            u64::from_le_bytes(b[8..].try_into().unwrap_or([0; 8])),
        ]
    }

    fn from_words(famport: u64, a0: u64, a1: u64) -> Endpoint {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&a0.to_le_bytes());
        b[8..].copy_from_slice(&a1.to_le_bytes());
        let port = famport as u16;
        if famport >> 16 == AF_INET6 {
            Endpoint { addr: Addr::V6(b), port }
        } else {
            Endpoint { addr: Addr::V4([b[0], b[1], b[2], b[3]]), port }
        }
    }

    /// The IPv4 address this is, said either way: an IPv6 address that maps
    /// one is that one.
    pub fn ipv4(&self) -> Option<[u8; 4]> {
        match self.addr {
            Addr::V4(a) => Some(a),
            Addr::V6(a) if a[..10] == [0; 10] && a[10..12] == [0xFF, 0xFF] => Some([a[12], a[13], a[14], a[15]]),
            Addr::V6(_) => None,
        }
    }
}

/// A new socket of `family` and `kind`: its descriptor.
fn create(family: u64, kind: u64) -> Result<usize, Error> {
    let net = net_tid()?;
    let msg = Message { sender: 0, tag: TAG_SOCKET, data: [OP_CREATE, family, kind, 0, 0, 0] };
    let mut reply = Message::empty();
    syscall::sys_call(net, &msg, &mut reply).map_err(|()| Error::NoService)?;
    if reply.tag == u64::MAX { Err(Error::of(reply.data[0])) } else { Ok(reply.data[0] as usize) }
}

/// A descriptor for a socket, and how this object uses it: whether it
/// waits, and for how long.
struct Sock {
    fd: usize,
    nonblocking: Cell<bool>,
    /// How long a read or a write may wait, in nanoseconds: 0 for as long
    /// as it takes.
    read_timeout: Cell<u64>,
    write_timeout: Cell<u64>,
    closed: Cell<bool>,
}

impl Sock {
    fn new(fd: usize) -> Sock {
        Sock {
            fd,
            nonblocking: Cell::new(false),
            read_timeout: Cell::new(0),
            write_timeout: Cell::new(0),
            closed: Cell::new(false),
        }
    }

    /// The operation's flag for whether it may wait.
    fn waiting(&self) -> u64 {
        if self.nonblocking.get() { DO_NOT_WAIT } else { 0 }
    }

    /// Ask the server something about this socket, lending it `lend` to read
    /// from or `fill` to write into.
    fn call(&self, op: u64, words: [u64; 4], lend: Option<&[u8]>, fill: Option<&mut [u8]>) -> Result<Message, Error> {
        let (server, cookie) = syscall::sys_fd_served(self.fd).map_err(|()| Error::Invalid)?;
        let msg = Message { sender: 0, tag: TAG_SOCKET, data: [op, cookie, words[0], words[1], words[2], words[3]] };
        let mut reply = Message::empty();
        let sent = match (lend, fill) {
            (Some(data), _) => syscall::sys_call_lend(server, &msg, &mut reply, data),
            (None, Some(buf)) => syscall::sys_call_lend_mut(server, &msg, &mut reply, buf),
            (None, None) => syscall::sys_call(server, &msg, &mut reply),
        };
        sent.map_err(|()| Error::NoService)?;
        if reply.tag == u64::MAX { Err(Error::of(reply.data[0])) } else { Ok(reply) }
    }

    fn ask(&self, op: u64, words: [u64; 4]) -> Result<Message, Error> {
        self.call(op, words, None, None)
    }

    /// Whether the descriptor became ready for `events` within `nanos`.
    fn ready_within(&self, events: u32, nanos: u64) -> bool {
        let mut fds = [syscall::PollFd::new(self.fd, events)];
        matches!(syscall::sys_poll(&mut fds, syscall::ns(nanos)), Ok(n) if n > 0)
    }

    /// Why a read or a write through the descriptor failed: the server keeps
    /// the error for whoever asks.
    fn failure(&self) -> Error {
        match self.ask(OP_OPTION, [OPT_ERROR, 0, 0, 0]) {
            Ok(r) if r.data[0] != 0 => Error::of(r.data[0]),
            _ => Error::Closed,
        }
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        if self.nonblocking.get() {
            return match syscall::sys_fd_read_nb(self.fd, buf) {
                syscall::WOULD_BLOCK => Err(Error::WouldBlock),
                u64::MAX => Err(self.failure()),
                n => Ok(n as usize),
            };
        }
        let t = self.read_timeout.get();
        if t != 0 && !self.ready_within(syscall::POLL_READABLE, t) {
            return Err(Error::WouldBlock);
        }
        match syscall::sys_fd_read(self.fd, buf) {
            u64::MAX => Err(self.failure()),
            n => Ok(n as usize),
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, Error> {
        if self.nonblocking.get() {
            return match syscall::sys_fd_write_nb(self.fd, buf) {
                syscall::WOULD_BLOCK => Err(Error::WouldBlock),
                u64::MAX => Err(self.failure()),
                n => Ok(n as usize),
            };
        }
        let t = self.write_timeout.get();
        if t != 0 && !self.ready_within(syscall::POLL_WRITABLE, t) {
            return Err(Error::WouldBlock);
        }
        match syscall::sys_fd_write(self.fd, buf) {
            u64::MAX => Err(self.failure()),
            n => Ok(n as usize),
        }
    }

    /// Bytes or a datagram, and where they came from, without taking them
    /// if `peek`.
    fn receive(&self, buf: &mut [u8], peek: bool) -> Result<(usize, Endpoint), Error> {
        let t = self.read_timeout.get();
        if !self.nonblocking.get() && t != 0 && !self.ready_within(syscall::POLL_READABLE, t) {
            return Err(Error::WouldBlock);
        }
        let op = OP_RECV_FROM | self.waiting() | if peek { PEEK } else { 0 };
        let r = self.call(op, [buf.len() as u64, 0, 0, 0], None, Some(buf))?;
        Ok((r.data[0] as usize, Endpoint::from_words(r.data[1], r.data[2], r.data[3])))
    }

    fn name(&self, peer: bool) -> Result<Endpoint, Error> {
        let r = self.ask(OP_NAME, [peer as u64, 0, 0, 0])?;
        Ok(Endpoint::from_words(r.data[0], r.data[1], r.data[2]))
    }

    fn take_error(&self) -> Result<Option<Error>, Error> {
        let r = self.ask(OP_OPTION, [OPT_ERROR, 0, 0, 0])?;
        Ok((r.data[0] != 0).then(|| Error::of(r.data[0])))
    }

    fn option(&self, name: u64) -> Result<u64, Error> {
        Ok(self.ask(OP_OPTION, [name, 0, 0, 0])?.data[0])
    }

    fn set_option(&self, name: u64, value: u64) -> Result<(), Error> {
        self.ask(OP_OPTION, [name, value, 1, 0]).map(|_| ())
    }

    /// Another descriptor for the same socket.
    fn duplicate(&self) -> Result<Sock, Error> {
        syscall::sys_fd_dup_self(self.fd, 3).map(Sock::new).map_err(|()| Error::NoDescriptor)
    }

    fn close(&self) {
        if !self.closed.replace(true) {
            let _ = syscall::sys_fd_close(self.fd);
        }
    }
}

impl Drop for Sock {
    fn drop(&mut self) {
        self.close();
    }
}

/// How much of a connection to shut down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    Read,
    Write,
    Both,
}

/// A connected TCP stream.
///
/// Closes its descriptor when dropped, and the connection goes when the
/// last descriptor for it does.
pub struct TcpStream {
    sock: Sock,
}

impl TcpStream {
    /// Connect to `ip:port`.
    pub fn connect(ip: [u8; 4], port: u16) -> Result<TcpStream, Error> {
        Self::connect_to(Endpoint::v4(ip, port))
    }

    /// Connect to `to`, IPv4 or IPv6.
    pub fn connect_to(to: Endpoint) -> Result<TcpStream, Error> {
        let sock = Sock::new(create(to.family(), SOCK_STREAM)?);
        let w = to.words();
        sock.ask(OP_CONNECT, [w[0], w[1], w[2], 0])?;
        Ok(TcpStream { sock })
    }

    /// Connect to `to`, giving up after `nanos` nanoseconds.
    pub fn connect_timeout(to: Endpoint, nanos: u64) -> Result<TcpStream, Error> {
        let sock = Sock::new(create(to.family(), SOCK_STREAM)?);
        let w = to.words();
        match sock.ask(OP_CONNECT | DO_NOT_WAIT, [w[0], w[1], w[2], 0]) {
            Ok(_) => return Ok(TcpStream { sock }),
            Err(Error::WouldBlock) => {}
            Err(e) => return Err(e),
        }
        if !sock.ready_within(syscall::POLL_WRITABLE, nanos) {
            return Err(Error::TimedOut);
        }
        match sock.take_error()? {
            None => Ok(TcpStream { sock }),
            Some(e) => Err(e),
        }
    }

    /// Connect to `host:port`, resolving `host` through the net server's DNS.
    ///
    /// A dotted quad is parsed directly rather than being sent to the
    /// resolver, so an address works with no DNS server configured.
    pub fn connect_host(host: &[u8], port: u16) -> Result<TcpStream, Error> {
        Self::connect(resolve(host)?, port)
    }

    /// Read into `buf`, returning 0 at end of stream.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Error> {
        self.sock.read(buf)
    }

    /// What `read` would give, left to be read.
    pub fn peek(&self, buf: &mut [u8]) -> Result<usize, Error> {
        self.sock.receive(buf, true).map(|(n, _)| n)
    }

    /// Write `buf`, waiting until some of it is taken: how much.
    pub fn write(&self, buf: &[u8]) -> Result<usize, Error> {
        self.sock.write(buf)
    }

    /// Keep writing until everything is sent.
    pub fn write_all(&self, buf: &[u8]) -> Result<(), Error> {
        let mut sent = 0;
        while sent < buf.len() {
            match self.write(&buf[sent..])? {
                0 => return Err(Error::Closed),
                n => sent += n,
            }
        }
        Ok(())
    }

    /// Who is at the other end.
    pub fn peer(&self) -> Result<Endpoint, Error> {
        self.sock.name(true)
    }

    /// Where this end is.
    pub fn local(&self) -> Result<Endpoint, Error> {
        self.sock.name(false)
    }

    /// No more reading, no more writing — the other end is told — or both.
    pub fn shutdown(&self, how: Shutdown) -> Result<(), Error> {
        let how = match how {
            Shutdown::Read => 0,
            Shutdown::Write => 1,
            Shutdown::Both => 2,
        };
        self.sock.ask(OP_SHUTDOWN, [how, 0, 0, 0]).map(|_| ())
    }

    /// Send what is written at once, small or not (`TCP_NODELAY`).
    pub fn set_nodelay(&self, on: bool) -> Result<(), Error> {
        self.sock.set_option(OPT_NODELAY, on as u64)
    }

    pub fn nodelay(&self) -> Result<bool, Error> {
        self.sock.option(OPT_NODELAY).map(|v| v != 0)
    }

    /// What went wrong and has not been said, taking it.
    pub fn take_error(&self) -> Result<Option<Error>, Error> {
        self.sock.take_error()
    }

    /// Whether a read or a write that would wait fails instead.
    pub fn set_nonblocking(&self, on: bool) {
        self.sock.nonblocking.set(on);
    }

    /// How long a read may wait, in nanoseconds; `None` for as long as it
    /// takes. One that waits so long fails with [`Error::WouldBlock`].
    pub fn set_read_timeout(&self, nanos: Option<u64>) {
        self.sock.read_timeout.set(nanos.unwrap_or(0));
    }

    pub fn read_timeout(&self) -> Option<u64> {
        Some(self.sock.read_timeout.get()).filter(|&t| t != 0)
    }

    pub fn set_write_timeout(&self, nanos: Option<u64>) {
        self.sock.write_timeout.set(nanos.unwrap_or(0));
    }

    pub fn write_timeout(&self) -> Option<u64> {
        Some(self.sock.write_timeout.get()).filter(|&t| t != 0)
    }

    /// Another stream for the same connection, on a descriptor of its own.
    pub fn duplicate(&self) -> Result<TcpStream, Error> {
        Ok(TcpStream { sock: self.sock.duplicate()? })
    }

    /// The underlying descriptor, for code that takes an fd.
    pub fn as_fd(&self) -> usize {
        self.sock.fd
    }

    /// Give up ownership of the descriptor without closing it.
    pub fn into_fd(self) -> usize {
        let fd = self.sock.fd;
        core::mem::forget(self);
        fd
    }

    /// Close the descriptor now rather than on drop. Idempotent.
    pub fn close(&self) {
        self.sock.close();
    }
}

/// A socket listening for connections.
pub struct TcpListener {
    sock: Sock,
    port: u16,
}

impl TcpListener {
    /// Listen on `port` of every address this machine has, IPv4's and
    /// IPv6's alike.
    pub fn bind(port: u16) -> Result<TcpListener, Error> {
        Self::bind_to(Endpoint::v6([0; 16], port))
    }

    /// Listen at `at`.
    pub fn bind_to(at: Endpoint) -> Result<TcpListener, Error> {
        let sock = Sock::new(create(at.family(), SOCK_STREAM)?);
        let w = at.words();
        sock.ask(OP_BIND, [w[0], w[1], w[2], 0])?;
        sock.ask(OP_LISTEN, [BACKLOG, 0, 0, 0])?;
        let port = sock.name(false).map_or(at.port, |e| e.port);
        Ok(TcpListener { sock, port })
    }

    /// Wait for a client, returning the stream and its IPv4 address and
    /// port — nought for one that came by IPv6.
    pub fn accept(&self) -> Result<(TcpStream, [u8; 4], u16), Error> {
        let (stream, from) = self.accept_from()?;
        Ok((stream, from.ipv4().unwrap_or([0; 4]), from.port))
    }

    /// Wait for a client, returning the stream and where it came from.
    pub fn accept_from(&self) -> Result<(TcpStream, Endpoint), Error> {
        let r = self.sock.ask(OP_ACCEPT | self.sock.waiting(), [0; 4])?;
        let stream = TcpStream { sock: Sock::new(r.data[0] as usize) };
        Ok((stream, Endpoint::from_words(r.data[1], r.data[2], r.data[3])))
    }

    /// The port being listened on.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn local(&self) -> Result<Endpoint, Error> {
        self.sock.name(false)
    }

    /// Whether an accept with nobody waiting fails instead.
    pub fn set_nonblocking(&self, on: bool) {
        self.sock.nonblocking.set(on);
    }

    pub fn only_v6(&self) -> Result<bool, Error> {
        self.sock.option(OPT_V6ONLY).map(|v| v != 0)
    }

    pub fn take_error(&self) -> Result<Option<Error>, Error> {
        self.sock.take_error()
    }

    pub fn duplicate(&self) -> Result<TcpListener, Error> {
        Ok(TcpListener { sock: self.sock.duplicate()?, port: self.port })
    }

    pub fn as_fd(&self) -> usize {
        self.sock.fd
    }
}

/// A UDP socket.
pub struct UdpSocket {
    sock: Sock,
}

impl UdpSocket {
    /// A socket on `at`: a port of every address, or of one; port nought
    /// for one of the server's choosing.
    pub fn bind(at: Endpoint) -> Result<UdpSocket, Error> {
        let sock = Sock::new(create(at.family(), SOCK_DGRAM)?);
        let w = at.words();
        sock.ask(OP_BIND, [w[0], w[1], w[2], 0])?;
        Ok(UdpSocket { sock })
    }

    /// Send `data` to `to`, as one datagram: how much went, which is all of
    /// it.
    pub fn send_to(&self, data: &[u8], to: Endpoint) -> Result<usize, Error> {
        let t = self.sock.write_timeout.get();
        if !self.sock.nonblocking.get() && t != 0 && !self.sock.ready_within(syscall::POLL_WRITABLE, t) {
            return Err(Error::WouldBlock);
        }
        let w = to.words();
        let op = OP_SEND_TO | self.sock.waiting();
        let r = self.sock.call(op, [w[0], w[1], w[2], data.len() as u64], Some(data), None)?;
        Ok(r.data[0] as usize)
    }

    /// Wait for a datagram, into `buf`: how much of it there was room for —
    /// the rest of it is gone — and where it came from.
    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, Endpoint), Error> {
        self.sock.receive(buf, false)
    }

    /// What `recv_from` would give, left to be received.
    pub fn peek_from(&self, buf: &mut [u8]) -> Result<(usize, Endpoint), Error> {
        self.sock.receive(buf, true)
    }

    /// Talk to `to` and nobody else: what is sent goes there, and only what
    /// comes from there is received.
    pub fn connect(&self, to: Endpoint) -> Result<(), Error> {
        let w = to.words();
        self.sock.ask(OP_CONNECT, [w[0], w[1], w[2], 0]).map(|_| ())
    }

    /// Send to the one it is connected to.
    pub fn send(&self, data: &[u8]) -> Result<usize, Error> {
        self.sock.write(data)
    }

    /// Receive from the one it is connected to.
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, Error> {
        self.sock.read(buf)
    }

    pub fn peek(&self, buf: &mut [u8]) -> Result<usize, Error> {
        self.sock.receive(buf, true).map(|(n, _)| n)
    }

    /// The one it is connected to.
    pub fn peer(&self) -> Result<Endpoint, Error> {
        self.sock.name(true)
    }

    pub fn local(&self) -> Result<Endpoint, Error> {
        self.sock.name(false)
    }

    /// The port it is on.
    pub fn port(&self) -> Result<u16, Error> {
        self.local().map(|e| e.port)
    }

    pub fn set_nonblocking(&self, on: bool) {
        self.sock.nonblocking.set(on);
    }

    pub fn set_read_timeout(&self, nanos: Option<u64>) {
        self.sock.read_timeout.set(nanos.unwrap_or(0));
    }

    pub fn read_timeout(&self) -> Option<u64> {
        Some(self.sock.read_timeout.get()).filter(|&t| t != 0)
    }

    pub fn set_write_timeout(&self, nanos: Option<u64>) {
        self.sock.write_timeout.set(nanos.unwrap_or(0));
    }

    pub fn write_timeout(&self) -> Option<u64> {
        Some(self.sock.write_timeout.get()).filter(|&t| t != 0)
    }

    pub fn take_error(&self) -> Result<Option<Error>, Error> {
        self.sock.take_error()
    }

    pub fn duplicate(&self) -> Result<UdpSocket, Error> {
        Ok(UdpSocket { sock: self.sock.duplicate()? })
    }

    pub fn as_fd(&self) -> usize {
        self.sock.fd
    }
}

/// Resolve `host` to an address: a dotted quad directly, anything else
/// through the net server's resolver.
pub fn resolve(host: &[u8]) -> Result<[u8; 4], Error> {
    if let Some(ip) = parse_ipv4(host) {
        return Ok(ip);
    }
    let net_tid = net_tid()?;
    net::dns_resolve(net_tid, host)
        .map(|ip| ip.to_be_bytes())
        .map_err(|_| Error::NoHost)
}

/// Parse a dotted quad. Returns None for anything else, including a hostname.
pub fn parse_ipv4(s: &[u8]) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0;
    let mut value: u32 = 0;
    let mut digits = 0;

    for &b in s {
        match b {
            b'0'..=b'9' => {
                value = value * 10 + (b - b'0') as u32;
                if value > 255 {
                    return None;
                }
                digits += 1;
            }
            b'.' => {
                if digits == 0 || idx >= 3 {
                    return None;
                }
                octets[idx] = value as u8;
                idx += 1;
                value = 0;
                digits = 0;
            }
            _ => return None,
        }
    }

    if idx != 3 || digits == 0 {
        return None;
    }
    octets[3] = value as u8;
    Some(octets)
}
