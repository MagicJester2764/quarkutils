#![feature(restricted_std)]

// A thread needs a task to run in and pages for its stack.
quark_rt::manifest!([
    quark_rt::manifest::CapReq::task_mgmt(0),
    quark_rt::manifest::CapReq::phys_alloc(64),
]);

thread_local! {
    static SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn bump_seen() -> u64 {
    SEEN.with(|c| {
        c.set(c.get() + 1);
        c.get()
    })
}

fn main() {
    // Print program arguments
    let args: Vec<String> = std::env::args().collect();
    println!("argc={}", args.len());
    for (i, arg) in args.iter().enumerate() {
        println!("  argv[{}] = \"{}\"", i, arg);
    }

    println!("Hello from user space!");

    // Test Vec
    let v: Vec<u32> = (0..10).map(|i| i * i).collect();
    println!("Vec: {:?}", v);

    // Test String
    let mut s = String::from("Quark");
    s.push_str(" has a heap!");
    println!("{}", s);

    // Test larger allocation
    let big: Vec<u8> = (0..8192u16).map(|i| (i & 0xFF) as u8).collect();
    println!("Big vec len: {}", big.len());

    // Test deallocation + reuse
    drop(big);
    let reuse: Vec<u64> = (0..100).collect();
    println!("Reuse vec len: {}", reuse.len());

    // A HashMap seeds its hasher from the kernel's random numbers.
    let mut map = std::collections::HashMap::new();
    map.insert("quark", 1);
    println!("HashMap: {:?}", map.get("quark"));
    match std::env::current_dir() {
        Ok(dir) => println!("current dir: {}", dir.display()),
        Err(e) => println!("current dir: {e}"),
    }

    let handle = std::thread::spawn(|| 40u64 + 2);
    match handle.join() {
        Ok(v) => println!("thread returned {}", v),
        Err(_) => println!("thread: join failed"),
    }

    // A closure that captures moves through the global allocator, while the
    // thread's own control block goes through `System`. Both allocators are
    // exercised at once, which is the case that used to corrupt the heap.
    let owned = String::from("captured");
    let h = std::thread::spawn(move || format!("{} by the thread", owned));
    match h.join() {
        Ok(s) => println!("thread said: {}", s),
        Err(_) => println!("thread: join failed"),
    }

    // Thread-local storage: the same static, read from two threads, must give
    // each its own copy.
    println!("main sees SEEN = {}", bump_seen());
    let t = std::thread::spawn(|| bump_seen());
    let from_thread = t.join().unwrap_or(0);
    println!("thread sees SEEN = {}", from_thread);
    println!("main sees SEEN = {}", bump_seen());

    // Several at once, each with its own stack and thread-locals.
    let mut hs = Vec::new();
    for i in 0..4u64 {
        hs.push(std::thread::spawn(move || {
            for _ in 0..=i {
                bump_seen();
            }
            SEEN.with(|c| c.get()) * 1000 + i
        }));
    }
    let results: Vec<u64> = hs.into_iter().map(|h| h.join().unwrap_or(0)).collect();
    println!("four threads: {:?}", results);

    println!("Heap test passed!");

    // Timed waits. The kernel blocks for these now; it used to have no timed
    // futex, so std's wait_timeout was polling and yielding until the deadline.
    {
        use std::sync::{Arc, Condvar, Mutex};
        let pair = Arc::new((Mutex::new(false), Condvar::new()));

        // Nobody signals: this must come back as a timeout, near the deadline.
        let t0 = std::time::Instant::now();
        let (lock, cv) = &*pair;
        let guard = lock.lock().unwrap();
        let (_g, r) = cv
            .wait_timeout(guard, std::time::Duration::from_millis(300))
            .unwrap();
        println!(
            "condvar timeout: {} after {}ms",
            r.timed_out(),
            t0.elapsed().as_millis()
        );
    }
    {
        use std::sync::{Arc, Condvar, Mutex};
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let signaller = Arc::clone(&pair);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let (lock, cv) = &*signaller;
            *lock.lock().unwrap() = true;
            cv.notify_one();
        });

        // Signalled well inside the deadline: this must return early.
        let t0 = std::time::Instant::now();
        let (lock, cv) = &*pair;
        let mut guard = lock.lock().unwrap();
        while !*guard {
            let (g, r) = cv
                .wait_timeout(guard, std::time::Duration::from_millis(2000))
                .unwrap();
            guard = g;
            if r.timed_out() {
                break;
            }
        }
        println!(
            "condvar notify: signalled={} after {}ms",
            *guard,
            t0.elapsed().as_millis()
        );
    }

    // Test sleep
    let t0 = std::time::Instant::now();
    println!("Sleeping 500ms...");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let elapsed = t0.elapsed();
    println!("Woke up ({}ms elapsed)", elapsed.as_millis());

    // And a short one. The clock is in nanoseconds, and a sleep is as long
    // as was asked: this used to come back in ten or twenty milliseconds,
    // having been rounded up to the kernel's tick, by a clock that could
    // only say "0ms" or "10ms" about it.
    let t0 = std::time::Instant::now();
    std::thread::sleep(std::time::Duration::from_micros(1500));
    println!("A sleep of 1500us took {}us", t0.elapsed().as_micros());
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    println!("It is {}.{:09} seconds since 1970", since.as_secs(), since.subsec_nanos());

    let wrong = network();
    if wrong.is_empty() {
        println!("std::net: ok");
    } else {
        println!("std::net: {}", wrong.join("; "));
    }
}

/// The network, as std has it, over this machine's own addresses: what went
/// wrong, if anything did.
fn network() -> Vec<String> {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
    use std::time::Duration;
    let mut wrong = Vec::new();

    // A stream: a thread accepts, reads to the end and answers in capitals.
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || -> std::io::Result<SocketAddr> {
                let (mut s, from) = listener.accept()?;
                let mut got = Vec::new();
                s.read_to_end(&mut got)?;
                s.write_all(&got.to_ascii_uppercase())?;
                Ok(from)
            });
            let asked = (|| -> std::io::Result<(String, SocketAddr)> {
                let mut c = TcpStream::connect(addr)?;
                c.set_nodelay(true)?;
                c.write_all(b"quark over lo")?;
                c.shutdown(Shutdown::Write)?;
                let mut back = String::new();
                c.read_to_string(&mut back)?;
                Ok((back, c.local_addr()?))
            })();
            match (asked, server.join()) {
                (Ok((back, mine)), Ok(Ok(from))) if back == "QUARK OVER LO" && mine == from => {}
                (a, s) => wrong.push(format!("a stream: {a:?}, {s:?}")),
            }
        }
        Err(e) => wrong.push(format!("listening: {e}")),
    }

    // IPv6, with a time limit; and nobody there.
    match TcpListener::bind("[::1]:0") {
        Ok(listener) => {
            let addr = listener.local_addr().unwrap();
            let c = TcpStream::connect_timeout(&addr, Duration::from_secs(2));
            match (c, listener.accept()) {
                (Ok(_), Ok((_, from))) if from.ip() == Ipv6Addr::LOCALHOST => {}
                (c, a) => wrong.push(format!("IPv6: {c:?}, {a:?}")),
            }
        }
        Err(e) => wrong.push(format!("listening on ::1: {e}")),
    }
    match TcpStream::connect("127.0.0.1:9") {
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
        r => wrong.push(format!("nobody there: {r:?}")),
    }

    // Datagrams, waited for and not.
    match (UdpSocket::bind("127.0.0.1:0"), UdpSocket::bind("127.0.0.1:0")) {
        (Ok(a), Ok(b)) => {
            let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
            let _ = b.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 16];
            match (a.send_to(b"ping", ba), b.recv_from(&mut buf)) {
                (Ok(4), Ok((4, from))) if from == aa && &buf[..4] == b"ping" => {}
                (s, r) => wrong.push(format!("a datagram: {s:?}, {r:?}")),
            }
            let _ = b.set_nonblocking(true);
            match b.recv_from(&mut buf) {
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                r => wrong.push(format!("nothing to receive: {r:?}")),
            }
        }
        (a, b) => wrong.push(format!("datagram sockets: {a:?}, {b:?}")),
    }
    wrong
}
