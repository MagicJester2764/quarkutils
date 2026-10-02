#![no_std]
#![no_main]

//! Who this is, or who somebody is.
//!
//! ```text
//! id            the caller: what the kernel says it is
//! id USER       an account: what the files say it is
//! ```
//!
//! Printed the way Unix prints it — `uid=1000(nate) gid=1000(nate)
//! groups=1000(nate),10(wheel)` — because that line is read by scripts as
//! well as by people.

use quark_rt::accounts;
use quark_rt::nameserver;
use quark_rt::{args, print, println, syscall};

const TEXT_AT: usize = 0x98_0000_0000;
const TEXT_PAGES: usize = 8;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    if args::argc() > 2 || args::argv(1).is_some_and(|a| a.starts_with(b"-")) {
        println!("usage: id [USER]");
        syscall::sys_exit_code(2);
    }
    let vfs_tid = nameserver::lookup_retry(b"vfs", 20).unwrap_or(0);
    if syscall::sys_mmap(TEXT_AT, TEXT_PAGES).is_err() {
        println!("id: no memory");
        syscall::sys_exit_code(1);
    }
    let pages = unsafe { core::slice::from_raw_parts_mut(TEXT_AT as *mut u8, TEXT_PAGES * 4096) };
    let (passwd_buf, group_buf) = pages.split_at_mut(4 * 4096);
    // Without the files there are still numbers to print.
    let passwd = accounts::read(vfs_tid, b"", b"passwd", passwd_buf).unwrap_or(b"");
    let group = accounts::read(vfs_tid, b"", b"group", group_buf).unwrap_or(b"");

    let mut groups = [0u32; accounts::MAX_GROUPS];
    let (uid, gid, n) = match args::argv(1) {
        Some(name) => {
            let Some(user) = accounts::user_named(passwd, name) else {
                println!("id: there is no user called {}", text(name));
                syscall::sys_exit_code(1);
            };
            (user.uid, user.gid, accounts::groups_of(group, user.name, user.gid, &mut groups))
        }
        None => {
            let (uid, gid) = syscall::sys_get_uid();
            (uid, gid, syscall::sys_groups(0, &mut groups).unwrap_or(0).min(groups.len()))
        }
    };

    let named = |gid: u32| accounts::groups(group).find(|g| g.gid == gid).map(|g| g.name);
    print!("uid={}", uid);
    if let Some(user) = accounts::user_numbered(passwd, uid) {
        print!("({})", text(user.name));
    }
    print!(" gid={}", gid);
    if let Some(name) = named(gid) {
        print!("({})", text(name));
    }
    print!(" groups={}", gid);
    if let Some(name) = named(gid) {
        print!("({})", text(name));
    }
    for g in &groups[..n] {
        print!(",{}", g);
        if let Some(name) = named(*g) {
            print!("({})", text(name));
        }
    }
    println!();
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("id: {}", info);
    syscall::sys_exit_code(255);
}
