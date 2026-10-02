#![no_std]
#![no_main]

//! Take a user away.
//!
//! ```text
//! userdel [--root DIR] NAME
//! ```
//!
//! The account goes from `/etc/passwd`, `/etc/shadow` and every group, and a
//! group of its own name that nobody else is in goes with it. Its home is
//! left where it is: what is in it is somebody's work, and removing that is
//! a decision for whoever is typing. Root is not removed.

use quark_rt::accounts;
use quark_rt::nameserver;
use quark_rt::{args, println, syscall};

const WORK_AT: usize = 0x98_0000_0000;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: userdel [--root DIR] NAME");
    syscall::sys_exit_code(2);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut root: &[u8] = b"";
    let mut name: &[u8] = b"";
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        match arg {
            b"--root" | b"-R" => {
                i += 1;
                root = args::argv(i).unwrap_or_else(|| usage());
            }
            _ if arg.starts_with(b"-") || !name.is_empty() => usage(),
            _ => name = arg,
        }
        i += 1;
    }
    if name.is_empty() {
        usage();
    }
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        println!("userdel: there is no file server");
        syscall::sys_exit_code(1);
    };
    if syscall::sys_mmap(WORK_AT, accounts::WORK / 4096).is_err() {
        println!("userdel: no memory");
        syscall::sys_exit_code(1);
    }
    let work = unsafe { core::slice::from_raw_parts_mut(WORK_AT as *mut u8, accounts::WORK) };
    match accounts::remove_user(vfs_tid, root, name, work) {
        Ok(()) => {
            println!("{} is gone. Its home is where it was.", text(name));
            syscall::sys_exit_code(0);
        }
        Err(accounts::Trouble::Taken) => {
            println!("userdel: {} is the one user a system cannot be without", text(name));
            syscall::sys_exit_code(1);
        }
        Err(trouble) => {
            println!("userdel: {}: {}", text(name), trouble.words());
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("userdel: {}", info);
    syscall::sys_exit_code(255);
}
