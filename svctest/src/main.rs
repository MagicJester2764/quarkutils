#![no_std]
#![no_main]

//! A service for the service manager's tests, in four ways.
//!
//! ```text
//! svctest serve NAME [MS]     after MS milliseconds, register NAME and
//!                             answer whoever calls, for ever
//! svctest needs WANT NAME     WANT must be registered already when this
//!                             starts — it does not wait for it — and then
//!                             as serve; ends with status 3 if it was not
//! svctest crash [MS [STATUS]] end with STATUS (3) after MS (200)
//! svctest writer PATH         as serve, unregistered; SIGTERM has it write
//!                             PATH a moment later, and end
//! ```
//!
//! What it does it says on its standard output, which is the service
//! manager's log.

use core::sync::atomic::{AtomicBool, Ordering};
use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::{args, nameserver, println, signal, syscall, vfs};

static STOP: AtomicBool = AtomicBool::new(false);

fn number(text: Option<&[u8]>, or: u64) -> u64 {
    match text {
        Some(t) if !t.is_empty() && t.iter().all(u8::is_ascii_digit) => {
            t.iter().fold(0u64, |n, &d| n.saturating_mul(10).saturating_add(u64::from(d - b'0')))
        }
        _ => or,
    }
}

fn name(text: &[u8]) -> &str {
    core::str::from_utf8(text).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: svctest serve NAME [MS]");
    println!("       svctest needs WANT NAME");
    println!("       svctest crash [MS [STATUS]]");
    println!("       svctest writer PATH");
    syscall::sys_exit_code(2);
}

/// Answer whoever calls, until SIGTERM, if it is being listened for.
fn serve() {
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() || msg.sender == 0 {
            if STOP.load(Ordering::SeqCst) {
                return;
            }
            continue;
        }
        let _ = syscall::sys_reply(msg.sender, &Message { sender: 0, tag: 0, data: [0; 6] });
    }
}

fn stopped(_: &mut signal::Frame) {
    STOP.store(true, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    match args::argv(1) {
        Some(b"serve") => {
            let Some(own) = args::argv(2) else { usage() };
            syscall::sleep_ms(number(args::argv(3), 0));
            if nameserver::register(own).is_err() {
                println!("svctest: {} could not register", name(own));
                syscall::sys_exit_code(1);
            }
            println!("svctest: {} is up", name(own));
            serve();
            syscall::sys_exit_code(0);
        }
        Some(b"needs") => {
            let (Some(want), Some(own)) = (args::argv(2), args::argv(3)) else { usage() };
            if nameserver::lookup(want).is_none() {
                println!("svctest: {} started before {} was there", name(own), name(want));
                syscall::sys_exit_code(3);
            }
            if nameserver::register(own).is_err() {
                println!("svctest: {} could not register", name(own));
                syscall::sys_exit_code(1);
            }
            println!("svctest: {} found {}", name(own), name(want));
            serve();
            syscall::sys_exit_code(0);
        }
        Some(b"crash") => {
            let after = number(args::argv(2), 200);
            let status = number(args::argv(3), 3) as i32;
            println!("svctest: ending with status {} in {} ms", status, after);
            syscall::sleep_ms(after);
            syscall::sys_exit_code(status);
        }
        Some(b"writer") => {
            let Some(path) = args::argv(2) else { usage() };
            if signal::handle(syscall::SIGTERM, stopped, 0, 0).is_err() {
                println!("svctest: SIGTERM cannot be handled");
                syscall::sys_exit_code(1);
            }
            println!("svctest: writing {} when stopped", name(path));
            serve();
            // Slower than a server that only has to go: whoever stops this
            // has to wait for it.
            syscall::sleep_ms(300);
            let wrote = nameserver::lookup(b"vfs")
                .and_then(|v| vfs::open_fd(v, path, vfs::OPEN_WRITE | vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE, 0o644).ok())
                .map(|fd| {
                    let n = syscall::sys_fd_write(fd, b"svctest stopped\n");
                    let _ = syscall::sys_fd_close(fd);
                    n == 16
                })
                .unwrap_or(false);
            println!("svctest: {} {}", if wrote { "wrote" } else { "could not write" }, name(path));
            syscall::sys_exit_code(if wrote { 0 } else { 1 });
        }
        _ => usage(),
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::sys_exit_code(255);
}
