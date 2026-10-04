//! TCP and UDP through the Quark net server.
//!
//! A socket is a descriptor the net server serves, so `read` and `write`
//! here are the calls a pipe or a file is read and written with, and the
//! rest — connecting, listening, addresses, options — is `quark_rt::socket`
//! asking the server about the descriptor. Nothing in this file knows the
//! server's task ID. IPv4 and IPv6 alike: a socket of IPv6's family reaches
//! IPv4 too, as `::ffff:a.b.c.d`, as on Linux.

use super::each_addr;
use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs};
use crate::sys::unsupported;
use crate::time::Duration;

use quark_rt::socket::{self as rt, Addr, Endpoint, Error as SockError};

fn map_err(e: SockError) -> io::Error {
    use io::ErrorKind::*;
    match e {
        SockError::NoService => io::const_error!(NotFound, "no net service is running"),
        SockError::NoHost => io::const_error!(NotFound, "host not found"),
        SockError::ConnectFailed => io::const_error!(ConnectionRefused, "connection refused"),
        SockError::NoDescriptor => io::const_error!(Uncategorized, "no free file descriptor"),
        SockError::Closed => io::const_error!(BrokenPipe, "connection closed"),
        SockError::Io => io::const_error!(Uncategorized, "socket transfer failed"),
        SockError::WouldBlock => io::const_error!(WouldBlock, "the operation would block"),
        SockError::TimedOut => io::const_error!(TimedOut, "timed out"),
        SockError::AddrInUse => io::const_error!(AddrInUse, "address in use"),
        SockError::AddrNotAvailable => io::const_error!(AddrNotAvailable, "address not available"),
        SockError::Reset => io::const_error!(ConnectionReset, "connection reset"),
        SockError::NotConnected => io::const_error!(NotConnected, "not connected"),
        SockError::Invalid => io::const_error!(InvalidInput, "invalid argument"),
    }
}

fn endpoint(addr: &SocketAddr) -> Endpoint {
    match addr {
        SocketAddr::V4(a) => Endpoint::v4(a.ip().octets(), a.port()),
        SocketAddr::V6(a) => Endpoint::v6(a.ip().octets(), a.port()),
    }
}

fn socket_addr(e: Endpoint) -> SocketAddr {
    match e.addr {
        Addr::V4(a) => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(a), e.port)),
        Addr::V6(a) => SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(a), e.port, 0, 0)),
    }
}

/// A timeout as the runtime takes one, in nanoseconds. Nought is refused,
/// as everywhere: it would mean "for ever" to some and "not at all" to
/// others.
fn nanos(timeout: Option<Duration>) -> io::Result<Option<u64>> {
    match timeout {
        Some(d) if d.is_zero() => {
            Err(io::const_error!(io::ErrorKind::InvalidInput, "cannot set a 0 duration timeout"))
        }
        Some(d) => Ok(Some(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))),
        None => Ok(None),
    }
}

/// The first non-empty buffer of several, which is all one call here reads
/// or writes.
fn first_mut<'a>(bufs: &'a mut [IoSliceMut<'_>]) -> Option<&'a mut [u8]> {
    bufs.iter_mut().find(|b| !b.is_empty()).map(|b| &mut **b)
}

fn first<'a>(bufs: &'a [IoSlice<'_>]) -> Option<&'a [u8]> {
    bufs.iter().find(|b| !b.is_empty()).map(|b| &**b)
}

pub struct TcpStream {
    inner: rt::TcpStream,
}

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        each_addr(addr, |a| {
            rt::TcpStream::connect_to(endpoint(a)).map(|inner| TcpStream { inner }).map_err(map_err)
        })
    }

    pub fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        let nanos = nanos(Some(timeout))?.unwrap_or(u64::MAX);
        rt::TcpStream::connect_timeout(endpoint(addr), nanos)
            .map(|inner| TcpStream { inner })
            .map_err(map_err)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(nanos(timeout)?);
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(nanos(timeout)?);
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.inner.read_timeout().map(Duration::from_nanos))
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.inner.write_timeout().map(Duration::from_nanos))
    }

    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.peek(buf).map_err(map_err)
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(map_err)
    }

    pub fn read_buf(&self, cursor: BorrowedCursor<'_>) -> io::Result<()> {
        crate::io::default_read_buf(|buf| self.read(buf), cursor)
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        match first_mut(bufs) {
            Some(b) => self.read(b),
            None => Ok(0),
        }
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf).map_err(map_err)
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match first(bufs) {
            Some(b) => self.write(b),
            None => Ok(0),
        }
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer().map(socket_addr).map_err(map_err)
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local().map(socket_addr).map_err(map_err)
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Read => rt::Shutdown::Read,
            Shutdown::Write => rt::Shutdown::Write,
            Shutdown::Both => rt::Shutdown::Both,
        };
        self.inner.shutdown(how).map_err(map_err)
    }

    pub fn duplicate(&self) -> io::Result<TcpStream> {
        self.inner.duplicate().map(|inner| TcpStream { inner }).map_err(map_err)
    }

    pub fn set_linger(&self, _: Option<Duration>) -> io::Result<()> {
        unsupported()
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.inner.set_nodelay(nodelay).map_err(map_err)
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        self.inner.nodelay().map_err(map_err)
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.inner.take_error().map(|e| e.map(map_err)).map_err(map_err)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking);
        Ok(())
    }
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("TcpStream");
        if let Ok(addr) = self.socket_addr() {
            d.field("addr", &addr);
        }
        if let Ok(peer) = self.peer_addr() {
            d.field("peer", &peer);
        }
        d.field("fd", &self.inner.as_fd()).finish()
    }
}

pub struct TcpListener {
    inner: rt::TcpListener,
}

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        each_addr(addr, |a| {
            rt::TcpListener::bind_to(endpoint(a)).map(|inner| TcpListener { inner }).map_err(map_err)
        })
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local().map(socket_addr).map_err(map_err)
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (inner, from) = self.inner.accept_from().map_err(map_err)?;
        Ok((TcpStream { inner }, socket_addr(from)))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        self.inner.duplicate().map(|inner| TcpListener { inner }).map_err(map_err)
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn set_only_v6(&self, _: bool) -> io::Result<()> {
        // Said before a socket is bound, and std's listener is bound when it
        // is made.
        unsupported()
    }

    pub fn only_v6(&self) -> io::Result<bool> {
        self.inner.only_v6().map_err(map_err)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.inner.take_error().map(|e| e.map(map_err)).map_err(map_err)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking);
        Ok(())
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("TcpListener");
        if let Ok(addr) = self.socket_addr() {
            d.field("addr", &addr);
        }
        d.field("fd", &self.inner.as_fd()).finish()
    }
}

pub struct UdpSocket {
    inner: rt::UdpSocket,
}

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        each_addr(addr, |a| rt::UdpSocket::bind(endpoint(a)).map(|inner| UdpSocket { inner }).map_err(map_err))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer().map(socket_addr).map_err(map_err)
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local().map(socket_addr).map_err(map_err)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.inner.recv_from(buf).map(|(n, from)| (n, socket_addr(from))).map_err(map_err)
    }

    pub fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.inner.peek_from(buf).map(|(n, from)| (n, socket_addr(from))).map_err(map_err)
    }

    pub fn send_to(&self, buf: &[u8], addr: &SocketAddr) -> io::Result<usize> {
        self.inner.send_to(buf, endpoint(addr)).map_err(map_err)
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        self.inner.duplicate().map(|inner| UdpSocket { inner }).map_err(map_err)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(nanos(timeout)?);
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.set_write_timeout(nanos(timeout)?);
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.inner.read_timeout().map(Duration::from_nanos))
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.inner.write_timeout().map(Duration::from_nanos))
    }

    pub fn set_broadcast(&self, _: bool) -> io::Result<()> {
        unsupported()
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_multicast_loop_v4(&self, _: bool) -> io::Result<()> {
        unsupported()
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        unsupported()
    }

    pub fn set_multicast_ttl_v4(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn set_multicast_loop_v6(&self, _: bool) -> io::Result<()> {
        unsupported()
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        unsupported()
    }

    pub fn join_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn join_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn leave_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn leave_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.inner.take_error().map(|e| e.map(map_err)).map_err(map_err)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.inner.set_nonblocking(nonblocking);
        Ok(())
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.recv(buf).map_err(map_err)
    }

    pub fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.peek(buf).map_err(map_err)
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        self.inner.send(buf).map_err(map_err)
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        each_addr(addr, |a| self.inner.connect(endpoint(a)).map_err(map_err))
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("UdpSocket");
        if let Ok(addr) = self.socket_addr() {
            d.field("addr", &addr);
        }
        d.field("fd", &self.inner.as_fd()).finish()
    }
}

/// One address, or none. The net server's resolver answers with a single A
/// record, so there is never a list to walk.
pub struct LookupHost(Option<SocketAddr>);

impl Iterator for LookupHost {
    type Item = SocketAddr;
    fn next(&mut self) -> Option<SocketAddr> {
        self.0.take()
    }
}

/// Resolve `host`, which is what makes `TcpStream::connect("name:80")` work.
/// A literal address never comes here: std parses those itself.
pub fn lookup_host(host: &str, port: u16) -> io::Result<LookupHost> {
    let ip = rt::resolve(host.as_bytes()).map_err(map_err)?;
    Ok(LookupHost(Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)))))
}
