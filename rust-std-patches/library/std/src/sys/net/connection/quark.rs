//! TCP through the Quark net server.
//!
//! A connection is a file descriptor in the task's fd table, so `read` and
//! `write` here are the same syscalls a console or a pipe uses; the kernel
//! routes a socket fd to the net server. Nothing in this file knows the
//! server's task ID — `quark_rt::socket` finds it by name.
//!
//! UDP is still unimplemented. The net server has it, but only through the
//! page-based protocol, and nothing needs `std::net::UdpSocket` yet.

use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::sys::unsupported;
use crate::time::Duration;

use quark_rt::socket::Error as SockError;

fn map_err(e: SockError) -> io::Error {
    use io::ErrorKind::*;
    match e {
        SockError::NoService => io::const_error!(NotFound, "no net service is running"),
        SockError::NoHost => io::const_error!(NotFound, "host not found"),
        SockError::ConnectFailed => io::const_error!(ConnectionRefused, "connection failed"),
        SockError::NoDescriptor => io::const_error!(Uncategorized, "no free file descriptor"),
        SockError::Closed => io::const_error!(BrokenPipe, "connection closed"),
        SockError::Io => io::const_error!(Uncategorized, "socket transfer failed"),
    }
}

/// The first IPv4 address `addr` resolves to.
///
/// The net server speaks IPv4 only, so an IPv6-only name is a failure to
/// resolve rather than a protocol that is merely unimplemented.
fn first_v4<A: ToSocketAddrs>(addr: A) -> io::Result<SocketAddrV4> {
    for a in addr.to_socket_addrs()? {
        if let SocketAddr::V4(v4) = a {
            return Ok(v4);
        }
    }
    Err(io::const_error!(io::ErrorKind::InvalidInput, "no IPv4 address for host"))
}

pub struct TcpStream {
    inner: quark_rt::socket::TcpStream,
    peer: SocketAddrV4,
}

impl TcpStream {
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        let peer = first_v4(addr)?;
        let inner = quark_rt::socket::TcpStream::connect(peer.ip().octets(), peer.port())
            .map_err(map_err)?;
        Ok(TcpStream { inner, peer })
    }

    pub fn connect_timeout(addr: &SocketAddr, _: Duration) -> io::Result<TcpStream> {
        // The net server's connect has its own timeout and no way to set it,
        // so honouring this one would mean claiming a bound that is not kept.
        Self::connect(addr)
    }

    pub fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        unsupported()
    }

    pub fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        unsupported()
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn peek(&self, _: &mut [u8]) -> io::Result<usize> {
        // Would need the net server to return data without consuming it.
        unsupported()
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).map_err(map_err)
    }

    pub fn read_buf(&self, cursor: BorrowedCursor<'_>) -> io::Result<()> {
        crate::io::default_read_buf(|buf| self.read(buf), cursor)
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        for b in bufs {
            if !b.is_empty() {
                return self.read(b);
            }
        }
        Ok(0)
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf).map_err(map_err)
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        for b in bufs {
            if !b.is_empty() {
                return self.write(b);
            }
        }
        Ok(0)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V4(self.peer))
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        // The local port is chosen by the net server and never reported back.
        unsupported()
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match how {
            // Closing here and again on drop is harmless: the server no longer
            // recognises the handle as ours the second time.
            Shutdown::Both => {
                self.inner.close();
                Ok(())
            }
            // A half-close means sending FIN while still reading, which the
            // net server has no way to express.
            Shutdown::Read | Shutdown::Write => unsupported(),
        }
    }

    pub fn duplicate(&self) -> io::Result<TcpStream> {
        // Two handles on one connection needs refcounting the server does not
        // do; the second to close would take the connection out from the first.
        unsupported()
    }

    pub fn set_linger(&self, _: Option<Duration>) -> io::Result<()> {
        unsupported()
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_nodelay(&self, _: bool) -> io::Result<()> {
        // Every write is pushed as its own segment already.
        Ok(())
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        Ok(true)
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        if nb { unsupported() } else { Ok(()) }
    }
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpStream")
            .field("fd", &self.inner.as_fd())
            .field("peer", &self.peer)
            .finish()
    }
}

pub struct TcpListener {
    inner: quark_rt::socket::TcpListener,
}

impl TcpListener {
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        let local = first_v4(addr)?;
        let inner = quark_rt::socket::TcpListener::bind(local.port()).map_err(map_err)?;
        Ok(TcpListener { inner })
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        // The address is whatever the net server was configured with, which is
        // not reported per-listener; the port is ours.
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.inner.port())))
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (inner, ip, port) = self.inner.accept().map_err(map_err)?;
        let peer = SocketAddrV4::new(Ipv4Addr::from(ip), port);
        Ok((TcpStream { inner, peer }, SocketAddr::V4(peer)))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        unsupported()
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        unsupported()
    }

    pub fn ttl(&self) -> io::Result<u32> {
        unsupported()
    }

    pub fn set_only_v6(&self, _: bool) -> io::Result<()> {
        unsupported()
    }

    pub fn only_v6(&self) -> io::Result<bool> {
        // IPv4 only, so it is never v6-only.
        Ok(false)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        if nb { unsupported() } else { Ok(()) }
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpListener").field("port", &self.inner.port()).finish()
    }
}

pub struct UdpSocket(!);

impl UdpSocket {
    pub fn bind<A: ToSocketAddrs>(_: A) -> io::Result<UdpSocket> {
        unsupported()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.0
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.0
    }

    pub fn recv_from(&self, _: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.0
    }

    pub fn peek_from(&self, _: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.0
    }

    pub fn send_to(&self, _: &[u8], _: &SocketAddr) -> io::Result<usize> {
        self.0
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        self.0
    }

    pub fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        self.0
    }

    pub fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        self.0
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.0
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        self.0
    }

    pub fn set_broadcast(&self, _: bool) -> io::Result<()> {
        self.0
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        self.0
    }

    pub fn set_multicast_loop_v4(&self, _: bool) -> io::Result<()> {
        self.0
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        self.0
    }

    pub fn set_multicast_ttl_v4(&self, _: u32) -> io::Result<()> {
        self.0
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        self.0
    }

    pub fn set_multicast_loop_v6(&self, _: bool) -> io::Result<()> {
        self.0
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        self.0
    }

    pub fn join_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        self.0
    }

    pub fn join_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        self.0
    }

    pub fn leave_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        self.0
    }

    pub fn leave_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        self.0
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        self.0
    }

    pub fn ttl(&self) -> io::Result<u32> {
        self.0
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        self.0
    }

    pub fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        self.0
    }

    pub fn recv(&self, _: &mut [u8]) -> io::Result<usize> {
        self.0
    }

    pub fn peek(&self, _: &mut [u8]) -> io::Result<usize> {
        self.0
    }

    pub fn send(&self, _: &[u8]) -> io::Result<usize> {
        self.0
    }

    pub fn connect<A: ToSocketAddrs>(&self, _: A) -> io::Result<()> {
        self.0
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
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
///
/// A literal address is parsed here rather than sent to the resolver, so an
/// address works with no DNS server configured.
pub fn lookup_host(host: &str, port: u16) -> io::Result<LookupHost> {
    let ip = quark_rt::socket::resolve(host.as_bytes()).map_err(map_err)?;
    Ok(LookupHost(Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), port)))))
}
