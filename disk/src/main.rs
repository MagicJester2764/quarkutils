#![no_std]
#![no_main]
#![allow(dead_code)]

use quark_rt::block::{self, Device};
use quark_rt::nameserver;
use quark_rt::{println, syscall};

use quark_rt::manifest::CapReq;

// ATA primary channel: command block, control port, and IRQ 14 — and no
// physical memory at all. A client lends the buffer a sector goes into or
// comes out of with its call, and the driver copies through the kernel rather
// than mapping a page the client named. It used to hold all four gigabytes for
// that, and would read a sector over any of them a client asked it to.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::ioport(0x1F0, 0x1F7),
    CapReq::ioport(0x3F6, 0x3F6),
    CapReq::irq(14),
]);

// What a client asks, and who may ask it, is `quark_rt::block`: volumes,
// claims and the partition table are the same for every kind of disk. This
// file is where the sectors are.

// ATA PIO ports (primary channel)
const ATA_DATA: u16 = 0x1F0;
const ATA_ERROR: u16 = 0x1F1;
const ATA_SECTOR_COUNT: u16 = 0x1F2;
const ATA_LBA_LO: u16 = 0x1F3;
const ATA_LBA_MID: u16 = 0x1F4;
const ATA_LBA_HI: u16 = 0x1F5;
const ATA_DRIVE_HEAD: u16 = 0x1F6;
const ATA_STATUS: u16 = 0x1F7;
const ATA_COMMAND: u16 = 0x1F7;
const ATA_ALT_STATUS: u16 = 0x3F6;

// ATA status bits
const ATA_SR_BSY: u8 = 0x80;
const ATA_SR_DRDY: u8 = 0x40;
const ATA_SR_DRQ: u8 = 0x08;
const ATA_SR_ERR: u8 = 0x01;

// ATA commands
const ATA_CMD_IDENTIFY: u8 = 0xEC;
const ATA_CMD_READ_PIO: u8 = 0x20;
const ATA_CMD_WRITE_PIO: u8 = 0x30;
const ATA_CMD_WRITE_MULTIPLE: u8 = 0xC5;
const ATA_CMD_SET_MULTIPLE: u8 = 0xC6;

/// The driver's own page, which every sector passes through on its way to or
/// from a client's lent buffer.
const DRIVE_BUF: usize = 0x86_0000_0000;
/// Eight sectors, the most `TAG_READ_SECTORS` asks for, fill it exactly.
const MAX_SECTORS: u32 = 8;

struct DriveInfo {
    present: bool,
    lba28_sectors: u32,
    /// How many sectors the drive takes as one block of a WRITE MULTIPLE; 0
    /// if it has not agreed to any.
    multiple: u32,
}

static mut DRIVE: DriveInfo = DriveInfo {
    present: false,
    lba28_sectors: 0,
    multiple: 0,
};

fn ata_read_status() -> u8 {
    syscall::sys_ioport_read(ATA_ALT_STATUS) as u8
}

fn ata_wait_not_busy() {
    loop {
        let status = ata_read_status();
        if status & ATA_SR_BSY == 0 {
            return;
        }
        syscall::sys_yield();
    }
}

fn ata_wait_drq() -> bool {
    loop {
        let status = ata_read_status();
        if status & ATA_SR_ERR != 0 {
            return false;
        }
        if status & ATA_SR_BSY == 0 && status & ATA_SR_DRQ != 0 {
            return true;
        }
        syscall::sys_yield();
    }
}

fn ata_400ns_delay() {
    // Read alt status 4 times (~400ns delay)
    for _ in 0..4 {
        syscall::sys_ioport_read(ATA_ALT_STATUS);
    }
}

/// Ask the drive to take `sectors` at a time as one block of a WRITE
/// MULTIPLE. Whether it agreed.
fn ata_set_multiple(sectors: u32) -> bool {
    ata_wait_not_busy();
    syscall::sys_ioport_write(ATA_DRIVE_HEAD, 0xE0);
    ata_400ns_delay();
    syscall::sys_ioport_write(ATA_SECTOR_COUNT, sectors as u8);
    syscall::sys_ioport_write(ATA_COMMAND, ATA_CMD_SET_MULTIPLE);
    ata_400ns_delay();
    ata_wait_not_busy();
    ata_read_status() & ATA_SR_ERR == 0
}

fn ata_identify() -> bool {
    // Select drive 0 (master)
    syscall::sys_ioport_write(ATA_DRIVE_HEAD, 0xA0);
    ata_400ns_delay();

    // Zero out sector count and LBA registers
    syscall::sys_ioport_write(ATA_SECTOR_COUNT, 0);
    syscall::sys_ioport_write(ATA_LBA_LO, 0);
    syscall::sys_ioport_write(ATA_LBA_MID, 0);
    syscall::sys_ioport_write(ATA_LBA_HI, 0);

    // Send IDENTIFY command
    syscall::sys_ioport_write(ATA_COMMAND, ATA_CMD_IDENTIFY);
    ata_400ns_delay();

    // Check if drive exists
    let status = ata_read_status();
    if status == 0 {
        println!("[disk] No drive detected on primary master.");
        return false;
    }

    // Wait for BSY to clear
    ata_wait_not_busy();

    // Check for non-ATA devices (ATAPI, SATA, etc.)
    let lba_mid = syscall::sys_ioport_read(ATA_LBA_MID) as u8;
    let lba_hi = syscall::sys_ioport_read(ATA_LBA_HI) as u8;
    if lba_mid != 0 || lba_hi != 0 {
        println!("[disk] Non-ATA device detected (mid={:#x}, hi={:#x}).", lba_mid, lba_hi);
        return false;
    }

    // Wait for DRQ
    if !ata_wait_drq() {
        println!("[disk] IDENTIFY command failed (error).");
        return false;
    }

    // Read 256 words of identify data
    let mut identify = [0u16; 256];
    let _ = syscall::sys_ioport_rep_insw(ATA_DATA, &mut identify);

    // Extract model string (words 27-46, swapped byte pairs)
    let mut model = [0u8; 40];
    for i in 0..20 {
        let word = identify[27 + i];
        model[i * 2] = (word >> 8) as u8;
        model[i * 2 + 1] = word as u8;
    }
    // Trim trailing spaces
    let model_len = model.iter().rposition(|&b| b != b' ' && b != 0).map_or(0, |p| p + 1);

    // LBA28 sector count (words 60-61)
    let lba28_sectors = (identify[61] as u32) << 16 | (identify[60] as u32);

    unsafe {
        DRIVE.present = true;
        DRIVE.lba28_sectors = lba28_sectors;
    }

    // The most sectors it will take as one block (word 47's low byte): a
    // whole request's worth, if it will take that many. See
    // `ata_write_sectors` for what that buys.
    let most = (identify[47] & 0xFF) as u32;
    if most >= MAX_SECTORS && ata_set_multiple(MAX_SECTORS) {
        unsafe { DRIVE.multiple = MAX_SECTORS };
    }

    // Print drive info
    if let Ok(model_str) = core::str::from_utf8(&model[..model_len]) {
        println!("[disk] ATA drive: {}", model_str);
    }
    println!("[disk] {} sectors ({} MiB)", lba28_sectors, lba28_sectors / 2048);

    true
}

fn ata_read_sectors(lba: u32, count: u32, buf: *mut u8) -> bool {
    let max_sectors = unsafe { DRIVE.lba28_sectors };
    // The LBA comes from a client, so the end is computed without wrapping.
    if count == 0 || count > MAX_SECTORS || lba.checked_add(count).is_none_or(|end| end > max_sectors) {
        return false;
    }

    ata_wait_not_busy();

    syscall::sys_ioport_write(ATA_DRIVE_HEAD, 0xE0 | ((lba >> 24) & 0x0F) as u8);
    ata_400ns_delay();

    syscall::sys_ioport_write(ATA_SECTOR_COUNT, count as u8);
    syscall::sys_ioport_write(ATA_LBA_LO, lba as u8);
    syscall::sys_ioport_write(ATA_LBA_MID, (lba >> 8) as u8);
    syscall::sys_ioport_write(ATA_LBA_HI, (lba >> 16) as u8);

    syscall::sys_ioport_write(ATA_COMMAND, ATA_CMD_READ_PIO);
    ata_400ns_delay();

    for i in 0..count {
        if !ata_wait_drq() {
            return false;
        }
        let offset = (i as usize) * 512;
        let words = unsafe { core::slice::from_raw_parts_mut(buf.add(offset) as *mut u16, 256) };
        let _ = syscall::sys_ioport_rep_insw(ATA_DATA, words);
    }

    true
}

/// Write `count` sectors from `buf` at `lba`, as one command.
///
/// One command for the run, not one a sector: setting a command up is a
/// dozen writes to the drive's registers and two waits, and a client that
/// writes a filesystem block writes eight sectors at a time. Done a sector
/// at a time, that setup was most of what writing a file cost.
///
/// And as one *block* where the drive will take one (WRITE MULTIPLE): it is
/// handed the whole request before it writes any of it. With WRITE SECTORS
/// an emulated drive writes each sector as it arrives, so a machine stopped
/// half way through a request had half of it on the disk — and an ext4
/// superblock is two sectors with its checksum in the second. Stopped
/// between those two, the filesystem was one `e2fsck` would not open
/// without falling back to a copy of the superblock made when the disk was
/// formatted. No ATA command promises that a write is all or nothing; this
/// removes the case there was no need to have.
fn ata_write_sectors(lba: u32, count: u32, buf: *const u8) -> bool {
    let max_sectors = unsafe { DRIVE.lba28_sectors };
    // The LBA comes from a client, so the end is computed without wrapping.
    if count == 0 || count > MAX_SECTORS || lba.checked_add(count).is_none_or(|end| end > max_sectors) {
        return false;
    }

    ata_wait_not_busy();

    // Select drive 0, LBA mode, top 4 bits of LBA
    syscall::sys_ioport_write(ATA_DRIVE_HEAD, 0xE0 | ((lba >> 24) & 0x0F) as u8);
    ata_400ns_delay();

    syscall::sys_ioport_write(ATA_SECTOR_COUNT, count as u8);

    // Set LBA
    syscall::sys_ioport_write(ATA_LBA_LO, lba as u8);
    syscall::sys_ioport_write(ATA_LBA_MID, (lba >> 8) as u8);
    syscall::sys_ioport_write(ATA_LBA_HI, (lba >> 16) as u8);

    // The drive asks for each block when it has taken the last: the whole
    // request at once, or a sector at a time.
    let (command, block) = if unsafe { DRIVE.multiple } >= count {
        (ATA_CMD_WRITE_MULTIPLE, count)
    } else {
        (ATA_CMD_WRITE_PIO, 1)
    };
    syscall::sys_ioport_write(ATA_COMMAND, command);
    ata_400ns_delay();

    for i in 0..count {
        if i % block == 0 && !ata_wait_drq() {
            return false;
        }
        // 512 bytes, four at a time and each its own instruction. That is
        // what an emulated drive makes fast: under a hypervisor every byte
        // written to the data port is a trap, and `rep outsw` is 256 of them
        // taken the slowest way there is, through an instruction emulator.
        // This is 128 taken the quickest, and nearly three times as fast.
        // (Reads are not like this: a hypervisor reads ahead for `rep insw`.)
        // Every PCI IDE controller takes its data 32 bits at a time; the ISA
        // ones that did not are older than the firmware this boots from.
        for w in 0..128usize {
            let at = unsafe { buf.add(i as usize * 512 + w * 4) as *const u32 };
            syscall::sys_ioport_write32(ATA_DATA, unsafe { core::ptr::read_unaligned(at) });
        }
    }

    // Flush cache — wait for BSY to clear after write
    ata_wait_not_busy();

    // Check for errors
    let status = ata_read_status();
    if status & ATA_SR_ERR != 0 {
        return false;
    }

    true
}

/// The drive on the primary channel, as a block device.
struct Ata;

impl Device for Ata {
    fn sectors(&self) -> u64 {
        unsafe { DRIVE.lba28_sectors as u64 }
    }

    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool {
        lba <= u32::MAX as u64 && ata_read_sectors(lba as u32, count, into.as_mut_ptr())
    }

    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool {
        if lba.checked_add(count as u64).is_none_or(|end| end > u32::MAX as u64) {
            return false;
        }
        ata_write_sectors(lba as u32, count, from.as_ptr())
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("[disk] Started.");

    if syscall::sys_mmap(DRIVE_BUF, 1).is_err() {
        println!("[disk] No memory for a sector buffer. Exiting.");
        syscall::sys_exit();
    }

    // Identify drive
    if !ata_identify() {
        println!("[disk] No usable drive found. Exiting.");
        syscall::sys_exit();
    }

    // The first disk. There is one channel here and one drive on it; a
    // second driver would be `disk1`.
    if nameserver::register(b"disk0").is_ok() {
        println!("[disk] Registered with nameserver as disk0.");
    } else {
        println!("[disk] Failed to register with nameserver.");
    }

    block::serve(&mut Ata, DRIVE_BUF)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[disk] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
