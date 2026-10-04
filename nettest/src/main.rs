#![no_std]
#![no_main]

//! The network server's data path, end to end.
//!
//! A datagram and a TCP stream to the echo server on the machine QEMU runs on —
//! 10.0.2.2:7007 from here, which `boot-test.sh` provides — and what goes out
//! must come back; a dozen connections to it at once, each its own; and a
//! connection to this machine itself, 127.0.0.1, which a thread of this
//! program listens for. Every call lends its buffer to the server rather than
//! naming a page for it to map, which is why this asks for no capability.
//!
//! Then sockets that are descriptors (`quark_rt::socket`), over this
//! machine's own addresses: a stream — connected without waiting, accepted,
//! a quarter of a megabyte through it, half closed — and datagrams, each
//! said ready by a poll; IPv6's loopback beside IPv4's, a refusal, and a
//! port free again once its socket is closed. And IPv6 on the wire: a
//! connection to the host at fec0::2, which QEMU's user network is, from
//! the address a router's advertisement gave this machine on fec0::/64.
//!
//! Exits 0 only if every check holds.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use quark_rt::socket::{self, Addr, Endpoint, Shutdown, TcpListener, TcpStream, UdpSocket};
use quark_rt::{nameserver, net, println, syscall, thread};

/// 10.0.2.2, the host as QEMU's user network shows it; and fec0::2, as it
/// shows it over IPv6.
const ECHO_IP: u32 = 0x0A00_0202;
const ECHO6: [u8; 16] = [0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
const ECHO_PORT: u16 = 7007;
const LOCAL_PORT: u16 = 40007;
/// 127.0.0.1, and a port a thread of this program listens on there.
const LOOPBACK: u32 = 0x7F00_0001;
const LOOP_PORT: u16 = 40100;
/// Connections open at once: more than the eight the server once had.
const MANY: usize = 12;
const LO4: [u8; 4] = [127, 0, 0, 1];
const LO6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
/// Through the server's 64 KiB buffers four times over.
const BULK: usize = 256 * 1024;
const CHUNK: usize = 16 * 1024;
static mut OUT: [u8; CHUNK] = [0; CHUNK];
static mut IN: [u8; CHUNK] = [0; CHUNK];

static mut FAILED: u32 = 0;

fn check(what: &str, ok: bool) {
    println!("  {}  {}", if ok { "ok  " } else { "FAIL" }, what);
    if !ok {
        unsafe { FAILED += 1 };
    }
}

fn udp(net_tid: usize) {
    // The first datagram to an address the server has no hardware address for
    // goes to ARP instead, and is refused; one sent after the answer goes.
    let mut sent = Err(0);
    for _ in 0..10 {
        sent = net::udp_send(net_tid, b"quark-udp", ECHO_IP, ECHO_PORT, LOCAL_PORT);
        if sent.is_ok() {
            break;
        }
        syscall::sleep_ticks(10);
    }
    check("a datagram goes out", sent.is_ok());
    if sent.is_err() {
        return;
    }
    let mut buf = [0u8; 64];
    match net::udp_recv(net_tid, &mut buf, LOCAL_PORT) {
        Ok((n, ip, port, _)) => {
            check("and comes back", &buf[..n] == b"quark-udp");
            check("from the echo server", ip == ECHO_IP && port == ECHO_PORT);
        }
        Err(_) => check("and comes back", false),
    }
}

fn tcp(net_tid: usize) {
    let Ok(handle) = net::tcp_connect(net_tid, ECHO_IP, ECHO_PORT, 0) else {
        check("a connection opens", false);
        return;
    };
    check("a connection opens", true);
    check("bytes go out", net::tcp_send(net_tid, handle, b"quark-tcp") == Ok(9));
    let mut got = [0u8; 64];
    let mut n = 0;
    while n < 9 {
        match net::tcp_recv(net_tid, handle, &mut got[n..]) {
            Ok(k) if k > 0 => n += k,
            _ => break,
        }
    }
    check("and come back", &got[..n] == b"quark-tcp");
    let _ = net::tcp_close(net_tid, handle);
}

static NET: AtomicUsize = AtomicUsize::new(0);
/// Where the listening thread's connection came from.
static CAME_FROM: AtomicU32 = AtomicU32::new(0);

/// Listen on 127.0.0.1's port, take one connection, send back what came.
extern "C" fn listener() -> ! {
    let net_tid = NET.load(Ordering::SeqCst);
    if let Ok((handle, ip, _)) = net::tcp_listen(net_tid, LOOP_PORT) {
        CAME_FROM.store(ip, Ordering::SeqCst);
        let mut buf = [0u8; 32];
        if let Ok(n) = net::tcp_recv(net_tid, handle, &mut buf) {
            let _ = net::tcp_send(net_tid, handle, &buf[..n]);
        }
        syscall::sleep_ms(200);
        let _ = net::tcp_close(net_tid, handle);
    }
    syscall::sys_exit_code(0);
}

fn loopback(net_tid: usize) {
    NET.store(net_tid, Ordering::SeqCst);
    CAME_FROM.store(0, Ordering::SeqCst);
    let Ok(t) = thread::spawn_with_stack(listener, 8) else {
        check("a thread to listen", false);
        return;
    };
    syscall::sleep_ms(100);
    let Ok(handle) = net::tcp_connect(net_tid, LOOPBACK, LOOP_PORT, 0) else {
        check("a connection to this machine, at 127.0.0.1, opens", false);
        return;
    };
    check("a connection to this machine, at 127.0.0.1, opens", true);
    let sent = net::tcp_send(net_tid, handle, b"quark-loop") == Ok(10);
    let mut got = [0u8; 32];
    let mut n = 0;
    while sent && n < 10 {
        match net::tcp_recv(net_tid, handle, &mut got[n..]) {
            Ok(k) if k > 0 => n += k,
            _ => break,
        }
    }
    check("what goes out comes back through the listener", &got[..n] == b"quark-loop");
    let _ = net::tcp_close(net_tid, handle);
    t.join();
    check("which saw it come from 127.0.0.1", CAME_FROM.load(Ordering::SeqCst) == LOOPBACK);
}

fn many(net_tid: usize) {
    let mut handles = [0usize; MANY];
    let mut open = 0;
    while open < MANY {
        match net::tcp_connect(net_tid, ECHO_IP, ECHO_PORT, 0) {
            Ok(h) => {
                handles[open] = h;
                open += 1;
            }
            Err(_) => break,
        }
    }
    check("twelve connections are open at once", open == MANY);
    let mut each = 0;
    for (i, &h) in handles[..open].iter().enumerate() {
        let said = [b'm', b'a', b'n', b'y', b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
        if net::tcp_send(net_tid, h, &said) != Ok(6) {
            continue;
        }
        let mut got = [0u8; 16];
        let mut n = 0;
        while n < 6 {
            match net::tcp_recv(net_tid, h, &mut got[n..]) {
                Ok(k) if k > 0 => n += k,
                _ => break,
            }
        }
        each += (got[..n] == said) as usize;
    }
    check("and each is its own", open > 0 && each == open);
    for &h in &handles[..open] {
        let _ = net::tcp_close(net_tid, h);
    }
}

/// The byte at offset `i` of what goes through a stream.
fn pattern(i: usize) -> u8 {
    (i * 7 + i / 251) as u8
}

/// Whether `fd` is ready for `events` within `ms` milliseconds.
fn ready(fd: usize, events: u32, ms: u64) -> bool {
    let mut fds = [syscall::PollFd::new(fd, events)];
    matches!(syscall::sys_poll(&mut fds, syscall::ns(ms * 1_000_000)), Ok(n) if n > 0) && fds[0].revents & events != 0
}

fn stream_over_loopback() {
    let Ok(listener) = TcpListener::bind_to(Endpoint::v4(LO4, 0)) else {
        check("a socket listens on 127.0.0.1", false);
        return;
    };
    let port = listener.port();
    check("a socket listens on 127.0.0.1, on a port of the server's choosing", port != 0);
    listener.set_nonblocking(true);
    check("an accept that may not wait, with nobody there, would block", matches!(listener.accept_from(), Err(socket::Error::WouldBlock)));
    check("and a poll says the listener has nothing", !ready(listener.as_fd(), syscall::POLL_READABLE, 20));
    let client = TcpStream::connect_timeout(Endpoint::v4(LO4, port), 2_000_000_000);
    check("a connection that does not wait is made, and said to be when it is", client.is_ok());
    let Ok(client) = client else { return };
    check("the listener is then readable", ready(listener.as_fd(), syscall::POLL_READABLE, 1000));
    let Ok((server, from)) = listener.accept_from() else {
        check("and accepts it", false);
        return;
    };
    check("and accepts it, from where the client is", client.local() == Ok(from) && from.addr == Addr::V4(LO4));
    check("whose other end is the listener's port", client.peer().is_ok_and(|p| p.port == port));

    check("what one end writes", client.write(b"over lo") == Ok(7));
    check("makes the other readable", ready(server.as_fd(), syscall::POLL_READABLE, 1000));
    let mut buf = [0u8; 32];
    check("and is read there", server.read(&mut buf) == Ok(7) && &buf[..7] == b"over lo");
    server.set_nonblocking(true);
    check("a read with nothing to read, that may not wait, would block", server.read(&mut buf) == Err(socket::Error::WouldBlock));
    check("and the stream is not readable", !ready(server.as_fd(), syscall::POLL_READABLE, 20));
    let _ = server.write(b"peek");
    let looked = ready(client.as_fd(), syscall::POLL_READABLE, 1000) && client.peek(&mut buf) == Ok(4) && &buf[..4] == b"peek";
    check("what is peeked at is left to be read", looked && client.read(&mut buf) == Ok(4) && &buf[..4] == b"peek");

    // A quarter of a megabyte, neither end waiting: what does not fit waits
    // its turn, and comes in order.
    client.set_nonblocking(true);
    let (out, inb) = unsafe { (&mut *core::ptr::addr_of_mut!(OUT), &mut *core::ptr::addr_of_mut!(IN)) };
    let (mut sent, mut got, mut good) = (0usize, 0usize, true);
    let began = syscall::sys_clock();
    while got < BULK && syscall::sys_clock() - began < 20_000_000_000 {
        if sent < BULK {
            let n = (BULK - sent).min(CHUNK);
            for (i, b) in out[..n].iter_mut().enumerate() {
                *b = pattern(sent + i);
            }
            match client.write(&out[..n]) {
                Ok(k) => sent += k,
                Err(socket::Error::WouldBlock) => {}
                Err(_) => break,
            }
        }
        match server.read(inb) {
            Ok(0) => break,
            Ok(k) => {
                good &= inb[..k].iter().enumerate().all(|(i, &b)| b == pattern(got + i));
                got += k;
            }
            Err(socket::Error::WouldBlock) => {
                let _ = ready(server.as_fd(), syscall::POLL_READABLE, 50);
            }
            Err(_) => break,
        }
    }
    check("a quarter of a megabyte goes through, in order", got == BULK && good);

    check("one end finishes writing", client.shutdown(Shutdown::Write).is_ok());
    let ended = ready(server.as_fd(), syscall::POLL_READABLE, 1000) && server.read(&mut buf) == Ok(0);
    check("and that is the end of what the other reads", ended);
    let _ = server.write(b"after");
    let after = ready(client.as_fd(), syscall::POLL_READABLE, 1000) && client.read(&mut buf) == Ok(5) && &buf[..5] == b"after";
    check("while what the other sends still comes", after);
    drop(server);
    let gone = ready(client.as_fd(), syscall::POLL_READABLE, 1000) && client.read(&mut buf) == Ok(0);
    check("and the other end closing is the end of it", gone);
    drop(client);
    drop(listener);
    check("a port is free again once its socket is closed", TcpListener::bind_to(Endpoint::v4(LO4, port)).is_ok());
}

fn datagrams_over_loopback() {
    let (Ok(a), Ok(b)) = (UdpSocket::bind(Endpoint::v4(LO4, 0)), UdpSocket::bind(Endpoint::v4(LO4, 0))) else {
        check("two datagram sockets", false);
        return;
    };
    let (Ok(ae), Ok(be)) = (a.local(), b.local()) else {
        check("two datagram sockets, with ports", false);
        return;
    };
    check("datagram sockets are given ports of their own", ae.port != 0 && be.port != 0 && ae.port != be.port);
    b.set_nonblocking(true);
    let mut buf = [0u8; 64];
    check("a receive with nothing come, that may not wait, would block", matches!(b.recv_from(&mut buf), Err(socket::Error::WouldBlock)));
    check("a datagram is sent", a.send_to(b"ping", be) == Ok(4));
    check("its receiver is readable", ready(b.as_fd(), syscall::POLL_READABLE, 1000));
    check("and it comes, saying where from", matches!(b.recv_from(&mut buf), Ok((4, from)) if from == ae) && &buf[..4] == b"ping");
    check("a socket connected to one correspondent", b.connect(ae).is_ok());
    check("sends to it by writing", b.send(b"pong") == Ok(4));
    a.set_read_timeout(Some(1_000_000_000));
    check("which receives it", matches!(a.recv_from(&mut buf), Ok((4, from)) if from == be) && &buf[..4] == b"pong");
    let other = UdpSocket::bind(Endpoint::v4(LO4, 0));
    let stranger = other.is_ok_and(|o| o.send_to(b"who", be) == Ok(3)) && !ready(b.as_fd(), syscall::POLL_READABLE, 50);
    check("and hears nobody else", stranger);
    let _ = a.send_to(b"0123456789", be);
    let cut = ready(b.as_fd(), syscall::POLL_READABLE, 1000) && matches!(b.recv_from(&mut buf[..4]), Ok((4, _)));
    check("a datagram is cut to the room there is, and the rest is gone", cut && matches!(b.recv_from(&mut buf), Err(socket::Error::WouldBlock)));
}

fn over_ipv6() {
    let Ok(listener) = TcpListener::bind(0) else {
        check("a socket listens on every address, IPv6's and IPv4's", false);
        return;
    };
    let port = listener.port();
    let client = TcpStream::connect_to(Endpoint::v6(LO6, port));
    let accepted = listener.accept_from();
    check("a connection over IPv6's loopback, ::1", client.is_ok() && matches!(accepted, Ok((_, from)) if from.addr == Addr::V6(LO6)));
    let client4 = TcpStream::connect(LO4, port);
    let accepted4 = listener.accept_from();
    let mapped = matches!(accepted4, Ok((_, from)) if matches!(from.addr, Addr::V6(_)) && from.ipv4() == Some(LO4));
    check("and over IPv4's, which IPv6's family hears as ::ffff:127.0.0.1", client4.is_ok() && mapped);
    let refused = TcpStream::connect(LO4, 9);
    check("a connection to a port nobody listens on is refused", matches!(refused, Err(socket::Error::ConnectFailed)));
}

fn host_over_ipv6() {
    let stream = TcpStream::connect_timeout(Endpoint::v6(ECHO6, ECHO_PORT), 5_000_000_000);
    check("a connection to the host over IPv6, at fec0::2", stream.is_ok());
    let Ok(stream) = stream else { return };
    let on_prefix = matches!(stream.local(), Ok(Endpoint { addr: Addr::V6(a), .. }) if a[..8] == ECHO6[..8]);
    check("from this machine's address on fec0::/64", on_prefix);
    let mut got = [0u8; 16];
    let mut n = 0;
    let sent = stream.write_all(b"quark-v6").is_ok();
    while sent && n < 8 {
        match stream.read(&mut got[n..]) {
            Ok(k) if k > 0 => n += k,
            _ => break,
        }
    }
    check("and what goes out comes back", &got[..n] == b"quark-v6");
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("nettest: echo at 10.0.2.2:{}", ECHO_PORT);
    let Some(net_tid) = nameserver::lookup_retry(b"net", 100) else {
        println!("nettest: no network service");
        syscall::sys_exit_code(1);
    };
    udp(net_tid);
    tcp(net_tid);
    many(net_tid);
    loopback(net_tid);
    stream_over_loopback();
    datagrams_over_loopback();
    over_ipv6();
    host_over_ipv6();
    let failed = unsafe { FAILED };
    println!("nettest: {}", if failed == 0 { "ok" } else { "FAIL" });
    syscall::sys_exit_code(if failed == 0 { 0 } else { 1 });
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("nettest: PANIC: {}", info);
    syscall::sys_exit_code(255);
}
