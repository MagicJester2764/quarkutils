#![no_std]
#![no_main]

//! A disk made of memory.
//!
//! It answers exactly what a disk driver answers (`quark_rt::block`) and
//! keeps its sectors in pages instead of on a platter, so it has volumes, a
//! partition table if somebody writes one, and claims. Nothing that uses a
//! disk can tell.
//!
//! There are two reasons to want one.
//!
//! **A system that runs from memory.** A bootloader can hand the kernel a
//! file, and a file can be a whole filesystem. `init` starts this on one —
//! `ramdisk module PHYS BYTES`, with the right to map exactly that memory —
//! and the file server takes it as its root. Nothing has to know how to read
//! the medium the system was booted from, which is what lets an installer
//! boot from a CD or a USB stick on a machine this system has no driver for.
//! What is written to it is written to memory, and gone at power-off.
//!
//! **A disk nothing depends on.** `ramdisk MEGABYTES` makes an empty one out
//! of ordinary memory: somewhere to make a partition table and a filesystem
//! and mount it, on a machine whose only real disk is the one it is running
//! from.
//!
//! Each registers under the first of `ram0`, `ram1`, … that nobody has.

use quark_rt::block::{self, Device};
use quark_rt::manifest::CapReq;
use quark_rt::{nameserver, println, syscall};

// A server's band: what calls it is a file server, and waiting on a disk is
// waiting. No memory is asked for here. The memory of a module is granted by
// whoever starts this on one, since only they know where it is; the memory
// of an empty disk is this program's own, given a page at a time as it is
// written.
quark_rt::manifest!([CapReq::priority(quark_rt::syscall::PRIO_SERVER)]);

const PAGE: usize = 4096;
/// The page every sector passes through on its way to or from a client.
const TRANSFER: usize = 0xA0_0000_0000;
/// Where the disk is. Room for sixty-four gigabytes, which is more memory
/// than there is.
const DISK: usize = 0xA1_0000_0000;
const MAX_BYTES: usize = 64 << 30;

struct Memory {
    bytes: usize,
}

impl Memory {
    fn at(&self, lba: u64, count: u32) -> Option<(usize, usize)> {
        let start = (lba as usize).checked_mul(block::SECTOR)?;
        let len = count as usize * block::SECTOR;
        (start.checked_add(len)? <= self.bytes).then_some((DISK + start, len))
    }
}

impl Device for Memory {
    fn sectors(&self) -> u64 {
        (self.bytes / block::SECTOR) as u64
    }

    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool {
        let Some((at, len)) = self.at(lba, count) else { return false };
        into[..len].copy_from_slice(unsafe { core::slice::from_raw_parts(at as *const u8, len) });
        true
    }

    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool {
        let Some((at, len)) = self.at(lba, count) else { return false };
        unsafe { core::slice::from_raw_parts_mut(at as *mut u8, len) }.copy_from_slice(&from[..len]);
        true
    }
}

/// A number, in decimal or after `0x` in hexadecimal.
fn number(arg: &[u8]) -> Option<usize> {
    let (digits, radix) = match arg {
        [b'0', b'x', rest @ ..] => (rest, 16),
        _ => (arg, 10),
    };
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0usize, |n, &c| {
        let d = (c as char).to_digit(radix)? as usize;
        n.checked_mul(radix as usize)?.checked_add(d)
    })
}

fn usage() -> ! {
    println!("usage: ramdisk MEGABYTES");
    println!("       ramdisk module PHYSICAL-ADDRESS BYTES");
    syscall::sys_exit_code(2);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let arg = |n| quark_rt::args::argv(n);
    let bytes = match (arg(1), arg(2), arg(3)) {
        // A file the bootloader loaded: mapped where it is.
        (Some(b"module"), Some(phys), Some(bytes)) => {
            let (Some(phys), Some(bytes)) = (number(phys), number(bytes)) else { usage() };
            // Whole sectors only: a file's last few bytes are not a sector.
            let bytes = bytes / block::SECTOR * block::SECTOR;
            let pages = bytes.div_ceil(PAGE);
            if phys % PAGE != 0 || bytes == 0 || bytes > MAX_BYTES {
                usage();
            }
            if syscall::sys_map_phys(phys, DISK, pages).is_err() {
                println!("[ramdisk] cannot map {} pages at {:#x}: not this program's to map", pages, phys);
                syscall::sys_exit_code(1);
            }
            bytes
        }
        // An empty one, of this program's own memory.
        (Some(megabytes), None, None) => {
            let Some(megabytes) = number(megabytes).filter(|&m| m > 0 && m <= MAX_BYTES >> 20) else {
                usage()
            };
            let bytes = megabytes << 20;
            // Refused outright if the machine has not got it, rather than
            // given and then missing when a sector is written.
            if syscall::sys_map_anon_accounted(DISK, bytes / PAGE).is_err() {
                println!("[ramdisk] no memory for {} MiB", megabytes);
                syscall::sys_exit_code(1);
            }
            bytes
        }
        _ => usage(),
    };
    if syscall::sys_map_anon(TRANSFER, 1, true).is_err() {
        println!("[ramdisk] no memory for a sector buffer");
        syscall::sys_exit_code(1);
    }

    // The first name nobody has. The nameserver refuses one a live task
    // holds, which is the whole of how two of these do not collide.
    let mut name = *b"ram0";
    let registered = (b'0'..=b'7').any(|digit| {
        name[3] = digit;
        nameserver::register(&name).is_ok()
    });
    if !registered {
        println!("[ramdisk] ram0 to ram7 are all taken");
        syscall::sys_exit_code(1);
    }
    println!(
        "[ramdisk] {}: {} MiB of memory",
        core::str::from_utf8(&name).unwrap_or("?"),
        bytes >> 20
    );

    block::serve(&mut Memory { bytes }, TRANSFER)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[ramdisk] PANIC: {}", info);
    syscall::sys_exit_code(255);
}
