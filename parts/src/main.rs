#![no_std]
#![no_main]

//! A disk's partition table: read it, make one, add to it, take from it.
//!
//! ```text
//! parts DISK                  what is on DISK
//! parts DISK init             a new, empty table
//! parts DISK new TYPE [SIZE]  one more partition, after the last
//! parts DISK delete NUMBER    one fewer
//! ```
//!
//! `DISK` is a disk as `disks` names it: `disk0`, `ram1`. `TYPE` is `efi`,
//! the partition firmware boots from; `root`, a system's root filesystem; or
//! `data`, any other. `SIZE` is a number of megabytes, or a number followed
//! by `K`, `M` or `G`; left out, the partition takes all the room there is.
//!
//! The table is a GPT, which is the only kind UEFI firmware promises to
//! read: a header in the second sector, 128 entries after it, the same
//! again at the end of the disk, and in the first sector an MBR with one
//! entry that says "this disk is taken" to anything that only knows MBRs.
//! Partitions begin on a megabyte.
//!
//! It talks to the disk's driver and not to a file under `/dev`, because
//! what it does afterwards only the driver can do: read the table again, so
//! that the new partitions are volumes. That is refused while any partition
//! of the disk is in use, and so is the claim on the whole disk this makes
//! before it writes — a disk with a mounted filesystem is not repartitioned
//! under it.

use quark_rt::block::{self, SECTOR};
use quark_rt::{nameserver, print, println, syscall};

// No manifest: what this needs is to be root, which is who a driver lets
// claim a disk. It is asked of the driver, not granted here.

const ENTRIES: usize = 128;
const ENTRY: usize = 128;
/// The entries, as sectors: thirty-two.
const TABLE_SECTORS: u64 = (ENTRIES * ENTRY / SECTOR) as u64;
/// Partitions begin on a megabyte.
const ALIGN: u64 = 2048;

/// The linux-filesystem type, which is what every Unix calls "a filesystem",
/// is `block::GUID_DATA`; the EFI system partition's is `block::GUID_EFI`.
const NAME_EFI: &str = "EFI system";
const NAME_ROOT: &str = "ExplOSion root";
const NAME_DATA: &str = "data";

static mut TABLE: [u8; ENTRIES * ENTRY] = [0; ENTRIES * ENTRY];

fn table() -> &'static mut [u8; ENTRIES * ENTRY] {
    unsafe { &mut *core::ptr::addr_of_mut!(TABLE) }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    (le32(b, at) as u64) | (le32(b, at + 4) as u64) << 32
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// The CRC a GPT is checked with: the one Ethernet and zip use.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A random GUID, of the kind that says it is random.
fn new_guid() -> [u8; 16] {
    let mut g = [0u8; 16];
    let _ = syscall::sys_getrandom(&mut g);
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

struct Disk {
    driver: usize,
    sectors: u64,
    guid: [u8; 16],
    /// Whether there was a table to read.
    has_table: bool,
}

impl Disk {
    fn first_usable(&self) -> u64 {
        2 + TABLE_SECTORS
    }

    fn last_usable(&self) -> u64 {
        self.sectors - 2 - TABLE_SECTORS
    }
}

fn entry(n: usize) -> &'static mut [u8] {
    &mut table()[n * ENTRY..(n + 1) * ENTRY]
}

fn used(n: usize) -> bool {
    entry(n)[..16].iter().any(|&b| b != 0)
}

/// Read `count` sectors at `lba` into `buf`, eight at a time.
fn read(driver: usize, lba: u64, buf: &mut [u8]) -> bool {
    buf.chunks_mut(block::MAX_SECTORS as usize * SECTOR)
        .enumerate()
        .all(|(i, piece)| block::read(driver, 0, lba + i as u64 * block::MAX_SECTORS as u64, piece).is_ok())
}

fn write(driver: usize, lba: u64, buf: &[u8]) -> bool {
    buf.chunks(block::MAX_SECTORS as usize * SECTOR)
        .enumerate()
        .all(|(i, piece)| block::write(driver, 0, lba + i as u64 * block::MAX_SECTORS as u64, piece).is_ok())
}

/// Read the disk's table into [`table`]. A disk with none is a disk with an
/// empty one, and says so.
fn load(driver: usize) -> Option<Disk> {
    let info = block::info(driver, 0).ok()?;
    let mut disk = Disk { driver, sectors: info.sectors, guid: [0; 16], has_table: false };
    table().fill(0);
    let mut header = [0u8; SECTOR];
    if info.sectors < 2 * (2 + TABLE_SECTORS) + ALIGN {
        return Some(disk);
    }
    // Refused is not the same as nothing there: a disk a user may not read
    // used to be shown as one with no table, to somebody about to believe it.
    match block::read(driver, 0, 1, &mut header) {
        Ok(()) => {}
        Err(block::ERR_NOT_ALLOWED | block::ERR_NOT_CLAIMANT) => fail("only root reads a disk"),
        Err(_) => return Some(disk),
    }
    if &header[..8] != b"EFI PART" {
        return Some(disk);
    }
    let (count, size, at) = (le32(&header, 80) as usize, le32(&header, 84) as usize, le64(&header, 72));
    if count != ENTRIES || size != ENTRY || !read(driver, at, table()) {
        // A table this does not know how to keep: left alone, and not
        // written over by anything but `init`.
        table().fill(0);
        return Some(disk);
    }
    disk.guid.copy_from_slice(&header[56..72]);
    disk.has_table = true;
    Some(disk)
}

/// Write the table in [`table`] to the disk, both copies, and the MBR in
/// front of it.
fn store(disk: &Disk) -> bool {
    let last = disk.sectors - 1;
    let table_crc = crc32(table());
    let header = |mine: u64, other: u64, entries_at: u64| {
        let mut h = [0u8; SECTOR];
        h[..8].copy_from_slice(b"EFI PART");
        put32(&mut h, 8, 0x0001_0000);
        put32(&mut h, 12, 92);
        put64(&mut h, 24, mine);
        put64(&mut h, 32, other);
        put64(&mut h, 40, disk.first_usable());
        put64(&mut h, 48, disk.last_usable());
        h[56..72].copy_from_slice(&disk.guid);
        put64(&mut h, 72, entries_at);
        put32(&mut h, 80, ENTRIES as u32);
        put32(&mut h, 84, ENTRY as u32);
        put32(&mut h, 88, table_crc);
        let crc = crc32(&h[..92]);
        put32(&mut h, 16, crc);
        h
    };
    // An MBR with one entry of the type that means "a GPT follows", over
    // the whole disk: what keeps a tool that only knows MBRs from thinking
    // the disk is empty.
    let mut mbr = [0u8; SECTOR];
    let e = &mut mbr[446..462];
    e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    e[4] = 0xEE;
    e[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    put32(e, 8, 1);
    put32(e, 12, last.min(u32::MAX as u64) as u32);
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    // The copy at the end first: if this stops half way, the one firmware
    // reads first is still the old one, whole.
    write(disk.driver, last - TABLE_SECTORS, table())
        && write(disk.driver, last, &header(last, 1, last - TABLE_SECTORS))
        && write(disk.driver, 2, table())
        && write(disk.driver, 1, &header(1, last, 2))
        && write(disk.driver, 0, &mbr)
}

fn megabytes(sectors: u64) -> u64 {
    sectors / 2048
}

fn print_table(name: &str, disk: &Disk) {
    println!(
        "{}: {} MiB, {}",
        name,
        megabytes(disk.sectors),
        if disk.has_table { "GPT" } else { "no partition table" }
    );
    let mut next_free = ALIGN;
    for n in 0..ENTRIES {
        if !used(n) {
            continue;
        }
        let e = entry(n);
        let (first, last) = (le64(e, 32), le64(e, 40));
        let what = if e[..16] == block::GUID_EFI {
            "EFI system"
        } else if e[..16] == block::GUID_DATA {
            "filesystem"
        } else {
            "other"
        };
        print!("  {}p{:<3} {:>6} MiB  {:<11} ", name, n + 1, megabytes(last - first + 1), what);
        // The name: UTF-16, of which the ASCII is shown.
        for pair in e[56..128].chunks(2) {
            match (pair[0], pair[1]) {
                (0, 0) => break,
                (c @ 0x20..=0x7E, 0) => print!("{}", c as char),
                _ => print!("?"),
            }
        }
        println!();
        next_free = next_free.max((last + 1).div_ceil(ALIGN) * ALIGN);
    }
    if disk.has_table {
        let room = (disk.last_usable() + 1).saturating_sub(next_free);
        println!("  {} MiB free", megabytes(room));
    }
}

/// `256M`, `2G`, `512K`, or megabytes with nothing after: sectors.
fn size(arg: &[u8]) -> Option<u64> {
    let (digits, per) = match arg.split_last()? {
        (b'K' | b'k', d) => (d, 2),
        (b'M' | b'm', d) => (d, 2048),
        (b'G' | b'g', d) => (d, 2048 * 1024),
        _ => (arg, 2048),
    };
    if digits.is_empty() {
        return None;
    }
    let n = digits.iter().try_fold(0u64, |n, &c| {
        c.is_ascii_digit().then_some(())?;
        n.checked_mul(10)?.checked_add((c - b'0') as u64)
    })?;
    n.checked_mul(per).filter(|&s| s > 0)
}

fn usage() -> ! {
    println!("usage: parts DISK");
    println!("       parts DISK init");
    println!("       parts DISK new efi|root|data [SIZE]");
    println!("       parts DISK delete NUMBER");
    syscall::sys_exit_code(2);
}

fn fail(what: &str) -> ! {
    println!("parts: {}", what);
    syscall::sys_exit_code(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let arg = |n| quark_rt::args::argv(n);
    let Some(name) = arg(1) else { usage() };
    let name = name.strip_prefix(b"/dev/").unwrap_or(name);
    let Ok(name) = core::str::from_utf8(name) else { usage() };
    let Some(driver) = nameserver::lookup(name.as_bytes()) else {
        println!("parts: there is no disk called {}", name);
        syscall::sys_exit_code(1);
    };
    let Some(mut disk) = load(driver) else { fail("the disk cannot be read") };

    let Some(verb) = arg(2) else {
        print_table(name, &disk);
        syscall::sys_exit_code(0);
    };

    // Everything below writes. The whole disk is claimed first, which the
    // driver refuses if a partition of it is somebody's.
    match block::claim(driver, 0) {
        Ok(()) => {}
        Err(block::ERR_BUSY) => {
            println!("parts: {} is in use: a filesystem on it is mounted, or it is open", name);
            syscall::sys_exit_code(1);
        }
        Err(block::ERR_NOT_ALLOWED) => fail("only root changes a partition table"),
        Err(_) => fail("the disk cannot be claimed"),
    }
    if disk.sectors < 2 * (2 + TABLE_SECTORS) + ALIGN {
        fail("the disk is too small for a partition table");
    }

    match (verb, arg(3), arg(4)) {
        (b"init", None, None) => {
            table().fill(0);
            disk.guid = new_guid();
            disk.has_table = true;
        }
        (b"new", Some(kind), amount) => {
            if !disk.has_table {
                fail("there is no partition table: `parts DISK init` makes one");
            }
            let (guid, label) = match kind {
                b"efi" => (block::GUID_EFI, NAME_EFI),
                b"root" => (block::GUID_DATA, NAME_ROOT),
                b"data" => (block::GUID_DATA, NAME_DATA),
                _ => usage(),
            };
            let Some(slot) = (0..ENTRIES).find(|&n| !used(n)) else { fail("the table is full") };
            // After everything that is there, on the next megabyte.
            let start = (0..ENTRIES)
                .filter(|&n| used(n))
                .map(|n| (le64(entry(n), 40) + 1).div_ceil(ALIGN) * ALIGN)
                .max()
                .unwrap_or(ALIGN);
            let room = (disk.last_usable() + 1).saturating_sub(start);
            let sectors = match amount {
                Some(a) => match size(a) {
                    Some(s) => s,
                    None => usage(),
                },
                // All that is left, in whole megabytes.
                None => room / ALIGN * ALIGN,
            };
            if sectors == 0 || sectors > room {
                println!("parts: there are {} MiB left on {}", megabytes(room), name);
                syscall::sys_exit_code(1);
            }
            let e = entry(slot);
            e.fill(0);
            e[..16].copy_from_slice(&guid);
            e[16..32].copy_from_slice(&new_guid());
            put64(e, 32, start);
            put64(e, 40, start + sectors - 1);
            for (i, c) in label.bytes().enumerate() {
                e[56 + i * 2] = c;
            }
            println!("{}p{}: {} MiB", name, slot + 1, megabytes(sectors));
        }
        (b"delete", Some(number), None) => {
            let n = number.iter().try_fold(0usize, |n, &c| {
                c.is_ascii_digit().then(|| n * 10 + (c - b'0') as usize)
            });
            match n {
                Some(n) if n >= 1 && n <= ENTRIES && used(n - 1) => entry(n - 1).fill(0),
                _ => fail("there is no such partition"),
            }
        }
        _ => usage(),
    }

    if !store(&disk) {
        fail("the table could not be written");
    }
    // The driver reads it again, and the partitions are volumes.
    if block::rescan(driver).is_err() {
        println!("parts: written, but a partition is in use: the new table takes effect when it is not");
    }
    let _ = block::release(driver, 0);
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("parts: {}", info);
    syscall::sys_exit_code(255);
}
