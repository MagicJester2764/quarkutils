#![no_std]
#![no_main]

//! Add a user.
//!
//! ```text
//! useradd [--root DIR] [-m] [-u UID] [-g GROUP] [-G GROUP,...] [-c ABOUT]
//!         [-d HOME] [-s SHELL] NAME
//! ```
//!
//! Unix's command, with Unix's letters. The account is made locked — nobody
//! logs in to it — until `passwd NAME` gives it a password. `-m` makes its
//! home, which is its own and nobody else's to look in. With `--root` the
//! account is made in a system mounted at DIR: an installer's first user.
//!
//! Only root can: the files are root's.

use quark_rt::accounts::{self, NewUser};
use quark_rt::nameserver;
use quark_rt::{args, println, syscall};

const WORK_AT: usize = 0x98_0000_0000;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: useradd [--root DIR] [-m] [-u UID] [-g GROUP] [-G GROUP,...]");
    println!("               [-c ABOUT] [-d HOME] [-s SHELL] NAME");
    syscall::sys_exit_code(2);
}

fn number(text: &[u8]) -> Option<u32> {
    if text.is_empty() {
        return None;
    }
    text.iter().try_fold(0u32, |n, &c| {
        c.is_ascii_digit().then_some(())?;
        n.checked_mul(10)?.checked_add((c - b'0') as u32)
    })
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut root: &[u8] = b"";
    let mut new = NewUser { name: b"", uid: None, group: None, about: b"", home: None, shell: None, make_home: false };
    let mut also: &[u8] = b"";
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        let mut value = || {
            i += 1;
            args::argv(i).unwrap_or_else(|| usage())
        };
        match arg {
            b"--root" | b"-R" => root = value(),
            b"-m" => new.make_home = true,
            b"-u" => new.uid = Some(number(value()).unwrap_or_else(|| usage())),
            b"-g" => new.group = Some(value()),
            b"-G" => also = value(),
            b"-c" => new.about = value(),
            b"-d" => new.home = Some(value()),
            b"-s" => new.shell = Some(value()),
            _ if arg.starts_with(b"-") || !new.name.is_empty() => usage(),
            _ => new.name = arg,
        }
        i += 1;
    }
    if new.name.is_empty() {
        usage();
    }

    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        println!("useradd: there is no file server");
        syscall::sys_exit_code(1);
    };
    if syscall::sys_mmap(WORK_AT, accounts::WORK / 4096).is_err() {
        println!("useradd: no memory");
        syscall::sys_exit_code(1);
    }
    let work = unsafe { core::slice::from_raw_parts_mut(WORK_AT as *mut u8, accounts::WORK) };

    let (uid, gid) = match accounts::add_user(vfs_tid, root, &new, work) {
        Ok(ids) => ids,
        Err(trouble) => {
            println!("useradd: {}: {}", text(new.name), trouble.words());
            syscall::sys_exit_code(1);
        }
    };
    // The groups it is in besides its own.
    for group in also.split(|&b| b == b',').filter(|g| !g.is_empty()) {
        if let Err(trouble) = accounts::set_member(vfs_tid, root, group, new.name, true, work) {
            println!("useradd: group {}: {}", text(group), trouble.words());
            syscall::sys_exit_code(1);
        }
    }
    println!(
        "{} is user {} in group {}. It has no password, and nobody logs in to it until `passwd {}` gives it one.",
        text(new.name),
        uid,
        gid,
        text(new.name)
    );
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("useradd: {}", info);
    syscall::sys_exit_code(255);
}
