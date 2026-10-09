//! How many calls a second the kernel answers, between pairs of threads.
//!
//! `callbench N SECS`: N pairs, each a server thread and a client thread
//! kept to one processor — the first pair to the first processor online,
//! the second to the next, round again past the last — the client calling
//! its server with a small message for SECS seconds, and the server
//! answering. Prints `callbench: N pairs: X calls/s (Y a pair)`.
//! `callbench sweep SECS` does 1, 2, 4 and 8 pairs.
//!
//! A pair on a processor of its own does not stop another: what it waits
//! for is the kernel. With one lock for all of the kernel, pairs on four
//! processors take turns in it and make about what one pair makes; taken
//! apart, they make nearly four times as much. This is what says which.
//!
//! Every answer is checked: one that does not come within a second, or
//! does not say what it should, is the end of the run, and a status of 1.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::{args, println, syscall, thread};

/// Pairs at most.
const MOST: usize = 64;

static GO: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
/// Each pair's server's task, once it is answering.
static SERVER: [AtomicUsize; MOST] = [const { AtomicUsize::new(0) }; MOST];
/// Each pair's client's calls, once it has stopped making them.
static CALLS: [AtomicU64; MOST] = [const { AtomicU64::new(u64::MAX) }; MOST];
/// The processors each pair is kept to, in turn.
static ONLINE: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
static NONLINE: AtomicUsize = AtomicUsize::new(1);

fn number(s: &[u8]) -> Option<usize> {
    if s.is_empty() || s.len() > 9 {
        return None;
    }
    s.iter().try_fold(0usize, |n, &b| b.is_ascii_digit().then(|| n * 10 + (b - b'0') as usize))
}

/// Keep the calling thread to the processor pair `i` is on.
fn keep(i: usize) {
    let cpu = ONLINE[i % NONLINE.load(SeqCst)].load(SeqCst);
    let mut set = [0u64; 4];
    set[cpu / 64] = 1 << (cpu % 64);
    let _ = syscall::sys_set_affinity(0, &set);
}

/// Answer calls until told to stop: a tag of nought.
extern "C" fn server(i: usize) -> ! {
    keep(i);
    let me = syscall::sys_getpid() as usize;
    // The right to call this thread, in the space its client shares.
    if syscall::mint_scratch(syscall::CAP_TYPE_ENDPOINT, me as u64, 0).is_err() {
        FAILED.store(true, SeqCst);
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

/// Call pair `i`'s server until told to stop, and say how many times.
extern "C" fn client(i: usize) -> ! {
    keep(i);
    let to = SERVER[i].load(SeqCst);
    while !GO.load(SeqCst) {
        syscall::sleep_ms(1);
    }
    let mut calls = 0u64;
    let mut reply = Message::empty();
    while !STOP.load(SeqCst) {
        calls += 1;
        let ask = Message { sender: 0, tag: calls, data: [calls, i as u64, 0, 0, 0, 0] };
        let answered = syscall::sys_call_timeout(to, &ask, &mut reply, 100);
        if !matches!(answered, syscall::CallOutcome::Replied) || reply.tag != calls + 1 || reply.data[0] != calls {
            if !FAILED.swap(true, SeqCst) {
                let how = match answered {
                    syscall::CallOutcome::Replied => "answered wrong",
                    syscall::CallOutcome::TimedOut => "not answered in a second",
                    syscall::CallOutcome::Failed => "refused",
                };
                println!(
                    "callbench: pair {}'s call {} to task {} {}: tag {} sender {} data {} {}",
                    i, calls, to, how, reply.tag, reply.sender, reply.data[0], reply.data[1]
                );
            }
            break;
        }
    }
    CALLS[i].store(calls, SeqCst);
    syscall::sys_exit_code(0);
}

/// `pairs` pairs for `secs` seconds: the calls they made, or `None` if one
/// went wrong.
fn run(pairs: usize, secs: u64) -> Option<u64> {
    GO.store(false, SeqCst);
    STOP.store(false, SeqCst);
    for i in 0..pairs {
        SERVER[i].store(0, SeqCst);
        CALLS[i].store(u64::MAX, SeqCst);
    }
    let mut servers = [const { None }; MOST];
    let mut clients = [const { None }; MOST];
    for i in 0..pairs {
        servers[i] = thread::spawn_with_arg(server, i, 8).ok();
        let began = syscall::sys_clock();
        while SERVER[i].load(SeqCst) == 0 && !FAILED.load(SeqCst) && syscall::sys_clock() - began < 2_000_000_000 {
            syscall::sleep_ms(1);
        }
        if SERVER[i].load(SeqCst) == 0 {
            println!("callbench: no server for pair {}", i);
            return None;
        }
        clients[i] = thread::spawn_with_arg(client, i, 8).ok();
        if clients[i].is_none() {
            println!("callbench: no client for pair {}", i);
            return None;
        }
    }
    GO.store(true, SeqCst);
    syscall::sleep_ms(secs * 1000);
    STOP.store(true, SeqCst);
    let mut total = 0u64;
    for (i, c) in clients.iter_mut().enumerate().take(pairs) {
        if let Some(t) = c.take() {
            let _ = t.join();
        }
        total += CALLS[i].load(SeqCst);
    }
    // Each server's last call: a tag of nought.
    let mut reply = Message::empty();
    for (i, s) in servers.iter_mut().enumerate().take(pairs) {
        let stop = Message::empty();
        let _ = syscall::sys_call_timeout(SERVER[i].load(SeqCst), &stop, &mut reply, 100);
        if let Some(t) = s.take() {
            let _ = t.join();
        }
    }
    (!FAILED.load(SeqCst)).then_some(total)
}

fn say(pairs: usize, secs: u64, calls: u64) {
    let rate = calls / secs.max(1);
    println!("callbench: {} pairs: {} calls/s ({} a pair)", pairs, rate, rate / pairs as u64);
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
    NONLINE.store(n.max(1), SeqCst);

    let first = args::argv(1).unwrap_or(b"");
    let secs = args::argv(2).and_then(number).unwrap_or(0) as u64;
    let sweep = first == b"sweep";
    let pairs = if sweep { Some(0) } else { number(first).filter(|&p| (1..=MOST).contains(&p)) };
    let (Some(pairs), true) = (pairs, secs > 0) else {
        println!("usage: callbench PAIRS SECS | callbench sweep SECS");
        syscall::sys_exit_program(2);
    };
    let counts: &[usize] = if sweep { &[1, 2, 4, 8] } else { core::slice::from_ref(&pairs) };
    for &p in counts {
        match run(p, secs) {
            Some(calls) => say(p, secs, calls),
            None => {
                println!("callbench: {} pairs: a call went unanswered, or wrong", p);
                syscall::sys_exit_program(1);
            }
        }
    }
    syscall::sys_exit_program(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("callbench: PANIC: {}", info);
    syscall::sys_exit_program(255);
}
