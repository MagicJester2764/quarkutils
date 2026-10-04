#![no_std]
#![no_main]

//! The services, and what is done with them.
//!
//! ```text
//! svc                     every service: what it is doing, its process,
//!                         how many times it has been started
//! svc status NAME         one, said in full: what it runs, what it needs,
//!                         how it last ended
//! svc start NAME          start it, and what it needs
//! svc stop NAME           stop it: SIGTERM, and five seconds
//! svc restart NAME        both
//! svc log NAME            what it has printed lately
//! ```
//!
//! `init` is the service manager and answers all of these. Anybody may ask
//! what the services are doing; starting and stopping one is for an account
//! that may end anybody's programs (the `tasks` right).

use quark_rt::manifest::CapReq;
use quark_rt::services;
use quark_rt::{args, print, println, syscall};

// What starting and stopping a service takes: what ending a program takes.
// A session without it has a `svc` that may only look.
quark_rt::manifest!([CapReq::task_mgmt(0)]);

static mut TEXT: [u8; 32768] = [0; 32768];

fn usage() -> ! {
    println!("usage: svc");
    println!("       svc status|start|stop|restart|log NAME");
    syscall::sys_exit_code(2);
}

fn fail(code: u64) -> ! {
    println!("svc: {}", services::why(code));
    syscall::sys_exit_code(1);
}

fn print_text(said: Result<(usize, usize), u64>) -> ! {
    let text = unsafe { &*core::ptr::addr_of!(TEXT) };
    match said {
        Ok((n, _)) => {
            print!("{}", core::str::from_utf8(&text[..n]).unwrap_or(""));
            syscall::sys_exit_code(0);
        }
        Err(code) => fail(code),
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(init) = services::manager() else {
        println!("svc: the service manager is not answering");
        syscall::sys_exit_code(1);
    };
    let text = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };
    let (verb, name) = (args::argv(1), args::argv(2));
    if args::argv(3).is_some() {
        usage();
    }
    match (verb, name) {
        (None, _) | (Some(b"list"), None) => print_text(services::table(init, text)),
        (Some(b"status"), Some(name)) => print_text(services::describe(init, name, text)),
        (Some(b"log"), Some(name)) => print_text(services::log(init, name, text)),
        (Some(verb @ (b"start" | b"stop" | b"restart")), Some(name)) => {
            let done = match verb {
                b"start" => services::start(init, name),
                b"stop" => services::stop(init, name),
                _ => services::restart(init, name),
            };
            match done {
                Ok(()) => syscall::sys_exit_code(0),
                Err(code) => fail(code),
            }
        }
        _ => usage(),
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::sys_exit_code(255);
}
