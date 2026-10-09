//! Every processor hammering the kernel at once, and every operation
//! checked.
//!
//! `kstress KIND SECS`, KIND one of `calls`, `faults`, `futex`, `pipes`, or
//! `mix` for all four at once. For each processor online:
//!
//! - **calls**: a server and a client kept to it, the client calling with a
//!   numbered message and the server answering with the number after —
//!   every call answered, and with what it should be;
//! - **faults**: a thread that maps memory, writes every page of it — each
//!   a fault the kernel serves — reads each back and gives it back — every
//!   page holds what was written;
//! - **futex**: two threads, on it and on the next, handing a word back and
//!   forth, each waiting for its turn — every wait ended by the other's
//!   wake, and a wait that outlasts a second with its word changed is a
//!   wake lost;
//! - **pipes**: a writer on it and a reader on the next, through a pipe of
//!   their own, numbered words — every word read, in order, as written.
//!
//! At SECS seconds everything stops, the counts are printed a kind a line,
//! and the status is 0 only if nothing went wrong.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::{args, println, syscall, thread};

const MOST: usize = 256;
const KINDS: [&[u8]; 4] = [b"calls", b"faults", b"futex", b"pipes"];
const CALLS: usize = 0;
const FAULTS: usize = 1;
const FUTEX: usize = 2;
const PIPES: usize = 3;

static STOP: AtomicBool = AtomicBool::new(false);
/// Each kind's operations, and what went wrong with them.
static DONE: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static WRONG: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// The processors online, in order.
static ONLINE: [AtomicUsize; MOST] = [const { AtomicUsize::new(0) }; MOST];
static NONLINE: AtomicUsize = AtomicUsize::new(1);

/// Each processor's server, once it is answering.
static SERVER: [AtomicUsize; MOST] = [const { AtomicUsize::new(0) }; MOST];
/// Each pair's word, handed back and forth.
static WORD: [AtomicU32; MOST] = [const { AtomicU32::new(0) }; MOST];
/// Each pipe's two descriptors, and what went down it and came out.
static PIPE: [(AtomicUsize, AtomicUsize); MOST] = [const { (AtomicUsize::new(0), AtomicUsize::new(0)) }; MOST];
static WRITTEN: [AtomicU64; MOST] = [const { AtomicU64::new(0) }; MOST];
static READ: [AtomicU64; MOST] = [const { AtomicU64::new(0) }; MOST];

/// Where each processor's faults are taken: sixteen pages each, well above
/// the heap and below the threads' stacks.
const FAULTS_AT: usize = 0xB0_0000_0000;
const FAULT_STRIDE: usize = 0x100_0000;
const FAULT_PAGES: usize = 16;
const PAGE: usize = 4096;

fn number(s: &[u8]) -> Option<usize> {
    if s.is_empty() || s.len() > 9 {
        return None;
    }
    s.iter().try_fold(0usize, |n, &b| b.is_ascii_digit().then(|| n * 10 + (b - b'0') as usize))
}

fn online() -> usize {
    NONLINE.load(SeqCst)
}

/// Keep the calling thread to the `i`th processor online, round again past
/// the last.
fn keep(i: usize) {
    let cpu = ONLINE[i % online()].load(SeqCst);
    let mut set = [0u64; 4];
    set[cpu / 64] = 1 << (cpu % 64);
    let _ = syscall::sys_set_affinity(0, &set);
}

fn wrong(kind: usize) {
    WRONG[kind].fetch_add(1, SeqCst);
}

// --- calls ---------------------------------------------------------------

extern "C" fn server(i: usize) -> ! {
    keep(i);
    let me = syscall::sys_getpid() as usize;
    if syscall::mint_scratch(syscall::CAP_TYPE_ENDPOINT, me as u64, 0).is_err() {
        wrong(CALLS);
        syscall::sys_exit_code(1);
    }
    SERVER[i].store(me, SeqCst);
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        let answer = Message { sender: 0, tag: msg.tag.wrapping_add(1), data: msg.data };
        let _ = syscall::sys_reply(msg.sender, &answer);
        if msg.tag == 0 {
            syscall::sys_exit_code(0);
        }
    }
}

extern "C" fn client(i: usize) -> ! {
    keep(i);
    let to = SERVER[i].load(SeqCst);
    let mut n = 0u64;
    let mut reply = Message::empty();
    while !STOP.load(SeqCst) {
        n += 1;
        let ask = Message { sender: 0, tag: n, data: [n, i as u64, !n, 0, 0, 0] };
        let answered = syscall::sys_call_timeout(to, &ask, &mut reply, 100);
        if !matches!(answered, syscall::CallOutcome::Replied) || reply.tag != n + 1 || reply.data[0] != n || reply.data[2] != !n {
            wrong(CALLS);
            let how = match answered {
                syscall::CallOutcome::Replied => "answered wrong",
                syscall::CallOutcome::TimedOut => "not answered in a second",
                syscall::CallOutcome::Failed => "refused",
            };
            println!(
                "kstress: call {} of processor {}'s client to task {} {}: tag {} sender {} data {} {} {}",
                n, i, to, how, reply.tag, reply.sender, reply.data[0], reply.data[1], reply.data[2]
            );
            break;
        }
        DONE[CALLS].fetch_add(1, SeqCst);
    }
    syscall::sys_exit_code(0);
}

// --- faults --------------------------------------------------------------

extern "C" fn faulter(i: usize) -> ! {
    keep(i);
    let base = FAULTS_AT + i * FAULT_STRIDE;
    let mut round = 0u64;
    while !STOP.load(SeqCst) {
        if syscall::sys_mmap(base, FAULT_PAGES).is_err() {
            wrong(FAULTS);
            break;
        }
        let mark = |p: usize| (i as u64) << 48 | round << 8 | p as u64;
        for p in 0..FAULT_PAGES {
            let at = (base + p * PAGE) as *mut u64;
            unsafe {
                at.write_volatile(mark(p));
                at.add(PAGE / 8 - 1).write_volatile(!mark(p));
            }
        }
        for p in 0..FAULT_PAGES {
            let at = (base + p * PAGE) as *const u64;
            let (first, last) = unsafe { (at.read_volatile(), at.add(PAGE / 8 - 1).read_volatile()) };
            if first != mark(p) || last != !mark(p) {
                wrong(FAULTS);
            }
        }
        if syscall::sys_munmap(base, FAULT_PAGES).is_err() {
            wrong(FAULTS);
            break;
        }
        DONE[FAULTS].fetch_add(FAULT_PAGES as u64, SeqCst);
        round += 1;
    }
    syscall::sys_exit_code(0);
}

// --- futex ---------------------------------------------------------------

/// One side of pair `pair`: the even side hands the word on when it is
/// even, the odd side when it is odd.
fn hand_on(pair: usize, odd: u32) -> ! {
    keep(pair + odd as usize);
    let word = &WORD[pair];
    while !STOP.load(SeqCst) {
        let w = word.load(SeqCst);
        if w % 2 == odd {
            word.store(w.wrapping_add(1), SeqCst);
            syscall::sys_futex_wake(word.as_ptr(), 1);
            DONE[FUTEX].fetch_add(1, SeqCst);
            continue;
        }
        // Its turn is the other's; a second is far longer than a turn.
        let ended = syscall::sys_futex_wait_timeout(word.as_ptr(), w, syscall::ns(1_000_000_000));
        if ended == syscall::FUTEX_TIMED_OUT && !STOP.load(SeqCst) && word.load(SeqCst) != w {
            wrong(FUTEX);
        }
    }
    syscall::sys_exit_code(0);
}

extern "C" fn even(pair: usize) -> ! {
    hand_on(pair, 0)
}

extern "C" fn odd(pair: usize) -> ! {
    hand_on(pair, 1)
}

// --- pipes ---------------------------------------------------------------

extern "C" fn writer(i: usize) -> ! {
    keep(i);
    let fd = PIPE[i].1.load(SeqCst);
    let mut next = 0u64;
    let mut words = [0u64; 64];
    'out: while !STOP.load(SeqCst) {
        for w in words.iter_mut() {
            *w = next;
            next += 1;
        }
        let bytes = unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, 512) };
        let mut sent = 0;
        while sent < bytes.len() {
            let n = syscall::sys_fd_write(fd, &bytes[sent..]);
            if n == 0 || n >= u64::MAX - 16 {
                wrong(PIPES);
                break 'out;
            }
            sent += n as usize;
        }
        WRITTEN[i].store(next, SeqCst);
    }
    let _ = syscall::sys_fd_close(fd);
    syscall::sys_exit_code(0);
}

extern "C" fn reader(i: usize) -> ! {
    keep(i + 1);
    let fd = PIPE[i].0.load(SeqCst);
    let mut expect = 0u64;
    let mut buf = [0u8; 4096 + 8];
    let mut have = 0usize;
    loop {
        let n = syscall::sys_fd_read(fd, &mut buf[have..have + 4096]);
        if n == 0 {
            break;
        }
        if n >= u64::MAX - 16 {
            wrong(PIPES);
            break;
        }
        have += n as usize;
        let whole = have / 8 * 8;
        for chunk in buf[..whole].chunks_exact(8) {
            let word = u64::from_le_bytes(chunk.try_into().unwrap());
            if word != expect {
                wrong(PIPES);
            }
            expect = word.wrapping_add(1);
            DONE[PIPES].fetch_add(1, SeqCst);
        }
        buf.copy_within(whole..have, 0);
        have -= whole;
    }
    READ[i].store(expect, SeqCst);
    let _ = syscall::sys_fd_close(fd);
    syscall::sys_exit_code(0);
}

/// A pipe's two ends, at whatever descriptors are free.
fn pipe() -> Option<(usize, usize)> {
    let h = syscall::sys_pipe_create().ok()?;
    let (me, any) = (syscall::sys_getpid(), u64::MAX - 1);
    let r = unsafe { syscall::syscall4(syscall::SYS_PIPE_FD_SET, me, any, h as u64, 0) };
    let w = unsafe { syscall::syscall4(syscall::SYS_PIPE_FD_SET, me, any, h as u64, 1) };
    (r < u64::MAX - 16 && w < u64::MAX - 16).then_some((r as usize, w as usize))
}

// --- the run -------------------------------------------------------------

/// Every thread started, to be joined: seven a processor at most.
static mut THREADS: [Option<thread::Thread>; 7 * MOST] = [const { None }; 7 * MOST];
static STARTED: AtomicUsize = AtomicUsize::new(0);

fn start(entry: extern "C" fn(usize) -> !, arg: usize) {
    let t = STARTED.fetch_add(1, SeqCst);
    let made = thread::spawn_with_arg(entry, arg, 8).ok();
    if made.is_none() {
        println!("kstress: no thread to start");
        syscall::sys_exit_program(1);
    }
    unsafe { (*(&raw mut THREADS))[t] = made };
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut set = [0u64; 4];
    let _ = syscall::sys_cpu_online(&mut set);
    let mut n = 0;
    for cpu in (0..256).filter(|&c| set[c / 64] >> (c % 64) & 1 == 1) {
        ONLINE[n].store(cpu, SeqCst);
        n += 1;
    }
    let n = n.max(1);
    NONLINE.store(n, SeqCst);

    let kind = args::argv(1).unwrap_or(b"");
    let secs = args::argv(2).and_then(number).unwrap_or(0) as u64;
    let mut doing = [false; 4];
    match KINDS.iter().position(|&k| k == kind) {
        Some(k) => doing[k] = true,
        None if kind == b"mix" => doing = [true; 4],
        None => {}
    }
    if !doing.contains(&true) || secs == 0 {
        println!("usage: kstress calls|faults|futex|pipes|mix SECS");
        syscall::sys_exit_program(2);
    }

    for i in 0..n {
        if doing[CALLS] {
            start(server, i);
            let began = syscall::sys_clock();
            while SERVER[i].load(SeqCst) == 0 && syscall::sys_clock() - began < 2_000_000_000 {
                syscall::sleep_ms(1);
            }
            start(client, i);
        }
        if doing[FAULTS] {
            start(faulter, i);
        }
        if doing[FUTEX] {
            start(even, i);
            start(odd, i);
        }
        if doing[PIPES] {
            let Some((r, w)) = pipe() else {
                println!("kstress: no pipe");
                syscall::sys_exit_program(1);
            };
            PIPE[i].0.store(r, SeqCst);
            PIPE[i].1.store(w, SeqCst);
            start(writer, i);
            start(reader, i);
        }
    }

    syscall::sleep_ms(secs * 1000);
    STOP.store(true, SeqCst);
    for word in WORD.iter().take(n) {
        word.fetch_add(2, SeqCst);
        syscall::sys_futex_wake(word.as_ptr(), 2);
    }
    // Each server's last call, a tag of nought; then everybody joined.
    let mut reply = Message::empty();
    for server in SERVER.iter().take(n) {
        let to = server.load(SeqCst);
        if to != 0 {
            let _ = syscall::sys_call_timeout(to, &Message::empty(), &mut reply, 100);
        }
    }
    for t in 0..STARTED.load(SeqCst) {
        if let Some(thread) = unsafe { (*(&raw mut THREADS))[t].take() } {
            let _ = thread.join();
        }
    }

    if doing[PIPES] {
        for i in 0..n {
            if WRITTEN[i].load(SeqCst) != READ[i].load(SeqCst) {
                wrong(PIPES);
            }
        }
    }
    let said = [b"answered" as &[u8], b"pages", b"handed on", b"words"];
    for (k, kind) in KINDS.iter().enumerate().filter(|&(k, _)| doing[k]) {
        println!(
            "kstress: {}: {} {}, {} wrong",
            core::str::from_utf8(kind).unwrap_or("?"),
            DONE[k].load(SeqCst),
            core::str::from_utf8(said[k]).unwrap_or("?"),
            WRONG[k].load(SeqCst)
        );
    }
    let bad = WRONG.iter().any(|w| w.load(SeqCst) != 0) || doing.iter().zip(&DONE).any(|(&d, n)| d && n.load(SeqCst) == 0);
    println!("kstress: {} on {} processors for {} s: {}", core::str::from_utf8(kind).unwrap_or("?"), n, secs, if bad { "FAILED" } else { "passed" });
    syscall::sys_exit_program(bad as i32);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("kstress: PANIC: {}", info);
    syscall::sys_exit_program(255);
}
