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
//! Exits 0 only if every check holds.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use quark_rt::{nameserver, net, println, syscall, thread};

/// 10.0.2.2, the host as QEMU's user network shows it.
const ECHO_IP: u32 = 0x0A00_0202;
const ECHO_PORT: u16 = 7007;
const LOCAL_PORT: u16 = 40007;
/// 127.0.0.1, and a port a thread of this program listens on there.
const LOOPBACK: u32 = 0x7F00_0001;
const LOOP_PORT: u16 = 40100;
/// Connections open at once: more than the eight the server once had.
const MANY: usize = 12;

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
    let failed = unsafe { FAILED };
    println!("nettest: {}", if failed == 0 { "ok" } else { "FAIL" });
    syscall::sys_exit_code(if failed == 0 { 0 } else { 1 });
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("nettest: PANIC: {}", info);
    syscall::sys_exit_code(255);
}
