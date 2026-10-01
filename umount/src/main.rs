#![no_std]
#![no_main]

//! Take a mounted filesystem away.
//!
//! ```text
//! umount DIR
//! ```
//!
//! The file server serving what is mounted on `DIR` is told to stop: it lets
//! its volume go and ends, and `DIR` is the directory it was before. Not
//! while anything in the filesystem is open, a program is in it, or another
//! filesystem is mounted inside it — each of those is somebody still using
//! it.

use quark_rt::{args, nameserver, println, syscall, vfs};

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let (Some(dir), None) = (args::argv(1), args::argv(2)) else {
        println!("usage: umount DIR");
        syscall::sys_exit_code(2);
    };
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        println!("umount: there is no file server");
        syscall::sys_exit_code(1);
    };
    let dir_text = text(dir);
    match vfs::unmount(vfs_tid, dir) {
        Ok(()) => {
            let _ = vfs::write_mtab(vfs_tid);
            syscall::sys_exit_code(0);
        }
        Err(vfs::ERR_BUSY) => println!("umount: {} is in use", dir_text),
        Err(vfs::ERR_PERMISSION) => println!("umount: only root takes a filesystem away"),
        Err(vfs::ERR_NOT_FOUND) => println!("umount: there is no {}", dir_text),
        Err(vfs::ERR_INVALID_PATH) => println!("umount: nothing is mounted on {}", dir_text),
        Err(code) => println!("umount: {} could not be unmounted ({})", dir_text, code),
    }
    syscall::sys_exit_code(1);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("umount: {}", info);
    syscall::sys_exit_code(255);
}
