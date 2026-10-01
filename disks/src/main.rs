#![no_std]
#![no_main]

//! What disks there are, and what is on them.
//!
//! ```text
//! NAME           SIZE  WHAT          HOLDS  HELD BY
//! disk0      1024 MiB  disk          GPT
//! disk0p1     256 MiB  EFI system    FAT
//! disk0p2     767 MiB  filesystem    ext4   process 71
//! ram0        128 MiB  memory        ext2   process 71
//! ```
//!
//! Each line is a volume of a block driver: the whole device, then its
//! partitions as its partition table has them. `HOLDS` is what the first
//! sectors say is there, read without disturbing anybody — a driver lets
//! root read a volume somebody else has claimed, and write nothing. `HELD BY`
//! is who has claimed it: a file server that has it mounted, or a program
//! with the device open for writing.

use quark_rt::block;
use quark_rt::{nameserver, print, println, syscall};

// No manifest: asking a driver what it has, and reading a sector into a
// buffer of this program's own, needs nothing but the right to call it —
// which the nameserver gives with the name.

const DRIVERS: [&str; 12] = [
    "disk0", "disk1", "disk2", "disk3", "ram0", "ram1", "ram2", "ram3", "ram4", "ram5", "ram6",
    "ram7",
];

/// What the start of a volume says is on it.
fn contents(driver: usize, volume: u64) -> &'static str {
    let mut two = [0u8; 1024];
    // An ext superblock is the third and fourth sectors: the magic, and the
    // features that say which of the family it is.
    if block::read(driver, volume, 2, &mut two).is_ok() && two[56] == 0x53 && two[57] == 0xEF {
        let compat = u32::from_le_bytes([two[92], two[93], two[94], two[95]]);
        let incompat = u32::from_le_bytes([two[96], two[97], two[98], two[99]]);
        return if incompat & 0x40 != 0 {
            "ext4"
        } else if compat & 0x4 != 0 {
            "ext3"
        } else {
            "ext2"
        };
    }
    let sector = &mut two[..512];
    if block::read(driver, volume, 0, sector).is_err() {
        return "?";
    }
    if &sector[82..87] == b"FAT32" || &sector[54..57] == b"FAT" {
        return "FAT";
    }
    if block::read(driver, volume, 1, sector).is_ok() && &sector[..8] == b"EFI PART" {
        return "GPT";
    }
    ""
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    if quark_rt::args::argv(1).is_some() {
        println!("usage: disks");
        syscall::sys_exit_code(2);
    }
    println!("{:<9} {:>9}  {:<12}  {:<5}  HELD BY", "NAME", "SIZE", "WHAT", "HOLDS");
    let mut found = 0;
    for name in DRIVERS {
        let Some(driver) = nameserver::lookup(name.as_bytes()) else { continue };
        let Ok(whole) = block::info(driver, 0) else { continue };
        for volume in 0..whole.volumes {
            // An empty slot in the partition table is not a volume.
            let Ok(v) = block::info(driver, volume) else { continue };
            found += 1;
            let what = match v.kind {
                block::KIND_WHOLE if name.starts_with("ram") => "memory",
                block::KIND_WHOLE => "disk",
                block::KIND_EFI => "EFI system",
                block::KIND_DATA => "filesystem",
                _ => "other",
            };
            let mut label = [b' '; 9];
            label[..name.len()].copy_from_slice(name.as_bytes());
            let mut len = name.len();
            if volume > 0 {
                label[len] = b'p';
                len += 1;
                if volume >= 10 {
                    label[len] = b'0' + (volume / 10) as u8;
                    len += 1;
                }
                label[len] = b'0' + (volume % 10) as u8;
            }
            print!(
                "{} {:>5} MiB  {:<12}  {:<5}",
                core::str::from_utf8(&label).unwrap_or("?"),
                v.sectors / 2048,
                what,
                contents(driver, volume)
            );
            match v.claimant {
                0 => println!(),
                pid => println!("  process {}", pid),
            }
        }
    }
    if found == 0 {
        println!("(no disks)");
    }
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("disks: {}", info);
    syscall::sys_exit_code(255);
}
