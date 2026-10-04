#![no_std]
#![no_main]

//! The network server's data path, end to end.
//!
//! A datagram and a TCP stream to the echo server on the machine QEMU runs on —
//! 10.0.2.2:7007 from here, which `boot-test.sh` provides — and what goes out
//! must come back; a dozen connections to it at once, each its own; and a
//! connection to this machine itself, 127.0.0.1, which a thread of this
//! program listens for. Every call lends its buffer to the server rather than
//! naming a page for it to map.
//!
//! Then sockets that are descriptors (`quark_rt::socket`), over this
//! machine's own addresses: a stream — connected without waiting, accepted,
//! a quarter of a megabyte through it, half closed — and datagrams, each
//! said ready by a poll; IPv6's loopback beside IPv4's, a refusal, and a
//! port free again once its socket is closed. And IPv6 on the wire: a
//! connection to the host at fec0::2, which QEMU's user network is, from
//! the address a router's advertisement gave this machine on fec0::/64.
//! And the machine's resolver, at 127.0.0.1:53 and [::1]:53: a name only a
//! DNS server knows — quark.localhost, which a resolver says is this
//! machine (RFC 6761), as the host's does that QEMU's DNS asks — of both
//! families; and a name with a lifetime, asked again and answered from
//! what was kept. That one is example.com, which only the internet knows:
//! a host that cannot reach it fails that check, and says so.
//!
//! And the filter: a rule that drops a port keeps a connection to it over
//! 127.0.0.1 unanswered, the stack says so, and taking it out lets one in —
//! which needs the right to run the network (`NetAdmin`), asked for here
//! and held by root's session; and without it, the same change is refused.
//!
//! Exits 0 only if every check holds.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use quark_rt::manifest::CapReq;
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

quark_rt::manifest!([CapReq::net_admin()]);

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

/// A DNS question for `name`, of `qtype`, with `id`: its length in `out`.
fn dns_question(id: u16, name: &[u8], qtype: u16, out: &mut [u8]) -> usize {
    out[..2].copy_from_slice(&id.to_be_bytes());
    out[2..12].copy_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    let mut at = 12;
    for label in name.split(|&b| b == b'.') {
        out[at] = label.len() as u8;
        out[at + 1..at + 1 + label.len()].copy_from_slice(label);
        at += 1 + label.len();
    }
    out[at] = 0;
    out[at + 1..at + 5].copy_from_slice(&[(qtype >> 8) as u8, qtype as u8, 0, 1]);
    at + 5
}

/// Whether an answer of `n` bytes with `id` gives `addr` for a record of
/// `qtype`.
fn dns_says(a: &[u8], n: usize, id: u16, qtype: u16, addr: &[u8]) -> bool {
    if n < 12 || a[..2] != id.to_be_bytes() || a[2] & 0x80 == 0 || a[3] & 0x0F != 0 {
        return false;
    }
    let answers = u16::from_be_bytes([a[6], a[7]]);
    // Past the question, a name of labels, then its type and class.
    let mut at = 12;
    while at < n && a[at] != 0 {
        at += 1 + a[at] as usize;
    }
    at += 5;
    for _ in 0..answers {
        // A name: a pointer, or labels.
        while at < n && a[at] != 0 && a[at] & 0xC0 != 0xC0 {
            at += 1 + a[at] as usize;
        }
        at += if at < n && a[at] & 0xC0 == 0xC0 { 2 } else { 1 };
        if at + 10 > n {
            return false;
        }
        let rtype = u16::from_be_bytes([a[at], a[at + 1]]);
        let len = u16::from_be_bytes([a[at + 8], a[at + 9]]) as usize;
        at += 10;
        if at + len > n {
            return false;
        }
        if rtype == qtype && &a[at..at + len] == addr {
            return true;
        }
        at += len;
    }
    false
}

fn resolver(net_tid: usize) {
    let (Ok(s4), Ok(s6)) = (UdpSocket::bind(Endpoint::v4(LO4, 0)), UdpSocket::bind(Endpoint::v6(LO6, 0))) else {
        check("sockets to ask the resolver with", false);
        return;
    };
    s4.set_read_timeout(Some(8_000_000_000));
    s6.set_read_timeout(Some(8_000_000_000));
    let (mut q, mut a) = ([0u8; 64], [0u8; 512]);
    let n = dns_question(0x1234, b"quark.localhost", 1, &mut q);
    let sent = s4.send_to(&q[..n], Endpoint::v4(LO4, 53)).is_ok();
    let got = s4.recv_from(&mut a);
    let from53 = matches!(got, Ok((_, from)) if from == Endpoint::v4(LO4, 53));
    let len = got.map_or(0, |(k, _)| k);
    check("the resolver at 127.0.0.1:53 answers", sent && from53 && len >= 12);
    check("that quark.localhost is 127.0.0.1", dns_says(&a, len, 0x1234, 1, &LO4));
    let n = dns_question(0x4321, b"quark.localhost", 28, &mut q);
    let sent = s6.send_to(&q[..n], Endpoint::v6(LO6, 53)).is_ok();
    let len = s6.recv_from(&mut a).map_or(0, |(k, _)| k);
    check("and the one at [::1]:53, that it is ::1", sent && dns_says(&a, len, 0x4321, 28, &LO6));
    check("the old protocol's lookup goes through it too", net::dns_resolve(net_tid, b"quark.localhost") == Ok(LOOPBACK));
    // quark.localhost is said with a lifetime of nothing, which is not
    // kept. A name with one is asked twice, and the second time answered
    // from what was kept, with nothing asked of a server.
    let n = dns_question(0x5678, b"example.com", 1, &mut q);
    let _ = s4.send_to(&q[..n], Endpoint::v4(LO4, 53));
    let first = s4.recv_from(&mut a).map_or(0, |(k, _)| k);
    let answered = first >= 12 && a[3] & 0x0F == 0 && u16::from_be_bytes([a[6], a[7]]) > 0;
    if !answered {
        println!("  (example.com had no answer: can the host reach the internet?)");
    }
    let before = net::resolver_counts(net_tid);
    let n = dns_question(0x8765, b"example.com", 1, &mut q);
    let _ = s4.send_to(&q[..n], Endpoint::v4(LO4, 53));
    let len = s4.recv_from(&mut a).map_or(0, |(k, _)| k);
    let after = net::resolver_counts(net_tid);
    let kept = matches!((before, after), (Ok(b), Ok(a)) if a[2] == b[2] + 1 && a[1] == b[1]);
    let same = len == first && a[..2] == 0x8765u16.to_be_bytes() && a[3] & 0x0F == 0;
    check("a name asked again is answered from what was kept", answered && kept && same);
}

fn contains(text: &[u8], what: &[u8]) -> bool {
    text.windows(what.len()).any(|w| w == what)
}

fn filtering(net_tid: usize) {
    let Ok(listener) = TcpListener::bind_to(Endpoint::v4(LO4, 0)) else {
        check("a listener to keep out", false);
        return;
    };
    let port = listener.port();
    let rule = net::Rule { drop: true, protocol: 6, ports: (port, port), from: None };
    let added = net::filter_add(net_tid, &rule);
    check("a holder of the right adds a rule that drops a port", added.is_ok());
    let kept_out = TcpStream::connect_timeout(Endpoint::v4(LO4, port), 1_000_000_000);
    check("and a connection to it over 127.0.0.1 is not answered", matches!(kept_out, Err(socket::Error::TimedOut)));
    let mut text = [0u8; 4096];
    let mut line = *b"drop tcp port 00000";
    let mut digits = [0u8; 5];
    let mut at = digits.len();
    let mut n = port;
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let len = 14 + digits.len() - at;
    line[14..len].copy_from_slice(&digits[at..]);
    let shown = net::status(net_tid, &mut text)
        .is_ok_and(|(n, _)| contains(&text[..n], &line[..len]) && !contains(&text[..n], b" 0 dropped"));
    check("which the stack says, with what it dropped", shown);
    check("taking it out", added.is_ok_and(|n| net::filter_remove(net_tid, n).is_ok()));
    let let_in = TcpStream::connect_timeout(Endpoint::v4(LO4, port), 2_000_000_000);
    check("lets one in again", let_in.is_ok() && listener.accept_from().is_ok());
    // Without the right: the same change asked for, and refused.
    let me = syscall::sys_getpid() as usize;
    for slot in 0..64 {
        if syscall::sys_cap_read(me, slot).is_ok_and(|c| c.cap_type == syscall::CAP_TYPE_NET_ADMIN) {
            let _ = syscall::sys_cap_delete(slot);
        }
    }
    check("a program without the right is refused a change", net::filter_add(net_tid, &rule) == Err(1));
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
    resolver(net_tid);
    filtering(net_tid);
    let failed = unsafe { FAILED };
    println!("nettest: {}", if failed == 0 { "ok" } else { "FAIL" });
    syscall::sys_exit_code(if failed == 0 { 0 } else { 1 });
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("nettest: PANIC: {}", info);
    syscall::sys_exit_code(255);
}
