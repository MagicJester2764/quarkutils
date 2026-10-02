#![no_std]
#![no_main]

//! Who is in a group.
//!
//! ```text
//! gpasswd [--root DIR] -a USER GROUP     put USER in GROUP
//! gpasswd [--root DIR] -d USER GROUP     and take USER out of it
//! ```
//!
//! Unix's command for it, with Unix's letters; the rest of what Unix's does
//! — a group with a password of its own — nothing here has a use for.
//!
//! A user's groups are read when a session begins, so somebody put in a
//! group is in it from their next login.

use quark_rt::accounts;
use quark_rt::nameserver;
use quark_rt::{args, println, syscall};

const WORK_AT: usize = 0x98_0000_0000;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: gpasswd [--root DIR] -a USER GROUP    put USER in GROUP");
    println!("       gpasswd [--root DIR] -d USER GROUP    take USER out of it");
    syscall::sys_exit_code(2);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut root: &[u8] = b"";
    let mut group: &[u8] = b"";
    let mut member: Option<(&[u8], bool)> = None;
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        let mut value = || {
            i += 1;
            args::argv(i).unwrap_or_else(|| usage())
        };
        match arg {
            b"--root" | b"-R" => root = value(),
            b"-a" => member = Some((value(), true)),
            b"-d" => member = Some((value(), false)),
            _ if arg.starts_with(b"-") || !group.is_empty() => usage(),
            _ => group = arg,
        }
        i += 1;
    }
    let Some((user, present)) = member else { usage() };
    if group.is_empty() {
        usage();
    }
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        println!("gpasswd: there is no file server");
        syscall::sys_exit_code(1);
    };
    if syscall::sys_mmap(WORK_AT, accounts::WORK / 4096).is_err() {
        println!("gpasswd: no memory");
        syscall::sys_exit_code(1);
    }
    let work = unsafe { core::slice::from_raw_parts_mut(WORK_AT as *mut u8, accounts::WORK) };
    // Somebody there is no account for is nobody to put in a group.
    let (passwd_buf, rest) = work.split_at_mut(accounts::WORK / 5);
    if present && accounts::read(vfs_tid, root, b"passwd", passwd_buf).is_some_and(|p| accounts::user_named(p, user).is_none())
    {
        println!("gpasswd: there is no user called {}", text(user));
        syscall::sys_exit_code(1);
    }
    let _ = rest;
    match accounts::set_member(vfs_tid, root, group, user, present, work) {
        Ok(()) => {
            println!(
                "{} is {} {}, from their next login.",
                text(user),
                if present { "in" } else { "out of" },
                text(group)
            );
            syscall::sys_exit_code(0);
        }
        Err(trouble) => {
            println!("gpasswd: {}: {}", text(group), trouble.words());
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("gpasswd: {}", info);
    syscall::sys_exit_code(255);
}
