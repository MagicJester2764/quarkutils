#![no_std]
#![no_main]

//! How much memory the machine has, how much of it is free, and how much
//! there is to write memory out to when that is not enough.
//!
//! Three lines at the most. The last is there only where a pager has been
//! started for it (`swapd`): how much room it has, how much of that is in
//! use, and how many pages have gone out and come back since the machine
//! was started — which says whether the machine is short of memory now or
//! was once.

use quark_rt::{println, syscall};

/// Megabytes, to one place: pages are four kilobytes.
fn megabytes(pages: usize) -> (usize, usize) {
    (pages / 256, (pages % 256) * 10 / 256)
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let (total, _) = syscall::sys_mem_total();
    let (free, _) = syscall::sys_mem_info();
    let (room, out) = syscall::sys_swap_room();
    let (t, tf) = megabytes(total);
    let (f, ff) = megabytes(free);
    println!("memory     {:6}.{} MiB, {}.{} free", t, tf, f, ff);
    if room == 0 {
        println!("nowhere to write memory out to");
    } else {
        let (r, rf) = megabytes(room);
        let (o, of) = megabytes(out);
        let (written, read) = syscall::sys_swap_traffic();
        println!("written out {:5}.{} MiB of {}.{}", o, of, r, rf);
        println!("            {} pages out and {} back since the machine was started", written, read);
    }
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("free: PANIC: {}", info);
    syscall::sys_exit_code(1);
}
