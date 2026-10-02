#![no_std]
#![no_main]

//! Add a group.
//!
//! ```text
//! groupadd [--root DIR] [-g GID] NAME
//! ```
//!
//! Who is in it is `gpasswd`'s to say.

use quark_rt::accounts;
use quark_rt::nameserver;
use quark_rt::{args, println, syscall};

const WORK_AT: usize = 0x98_0000_0000;

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: groupadd [--root DIR] [-g GID] NAME");
    syscall::sys_exit_code(2);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut root: &[u8] = b"";
    let mut name: &[u8] = b"";
    let mut gid = None;
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        match arg {
            b"--root" | b"-R" => {
                i += 1;
                root = args::argv(i).unwrap_or_else(|| usage());
            }
            b"-g" => {
                i += 1;
                let digits = args::argv(i).unwrap_or_else(|| usage());
                gid = digits.iter().try_fold(0u32, |n, &c| {
                    c.is_ascii_digit().then_some(())?;
                    n.checked_mul(10)?.checked_add((c - b'0') as u32)
                });
                if gid.is_none() || digits.is_empty() {
                    usage();
                }
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
        println!("groupadd: there is no file server");
        syscall::sys_exit_code(1);
    };
    if syscall::sys_mmap(WORK_AT, accounts::WORK / 4096).is_err() {
        println!("groupadd: no memory");
        syscall::sys_exit_code(1);
    }
    let work = unsafe { core::slice::from_raw_parts_mut(WORK_AT as *mut u8, accounts::WORK) };
    match accounts::add_group(vfs_tid, root, name, gid, work) {
        Ok(gid) => {
            println!("{} is group {}.", text(name), gid);
            syscall::sys_exit_code(0);
        }
        Err(trouble) => {
            println!("groupadd: {}: {}", text(name), trouble.words());
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("groupadd: {}", info);
    syscall::sys_exit_code(255);
}
