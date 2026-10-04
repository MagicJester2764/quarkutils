//! Requests to the disk driver.
//!
//! Every sector passes through `DISK_IO_BUF`, which each request lends the
//! driver for the length of the call: a read fills it, a write is copied out of
//! it. The driver never learns where it is, and nothing here needs to know
//! where it lives in physical memory — which is what let both the driver and
//! this server give up their authority over physical memory.
//!
//! What this server has is a *volume* of the driver's: a partition, or a
//! whole device with a filesystem straight on it. Sector 0 is the volume's
//! first, whatever that is on the disk, and the driver refuses anything past
//! its last. Which volume is decided once, when the server starts.
//!
//! Or it is memory of this server's own (`vfs mem MEGABYTES`, what `mount -t
//! tmpfs` starts): a region reserved and not backed, so that a page has a
//! frame once something is written in it, and gives it back when the block
//! it holds is freed ([`discard`]). The same requests are answered by
//! copying, and nothing above this file can tell — except that what is in it
//! goes when the server does.

use quark_rt::block;
use quark_rt::ipc::Message;
use quark_rt::syscall;

use crate::DISK_IO_BUF;

/// Sectors one request may carry: as many as fill `DISK_IO_BUF`.
pub const MAX_SECTORS: u32 = block::MAX_SECTORS;

/// The volume this server was started on, and whose it is.
static mut VOLUME: u64 = 0;
static mut DRIVER: usize = 0;

/// The "driver" of a volume that is this server's memory.
pub const MEMORY: usize = usize::MAX;
/// Where that memory is: a terabyte up, past everything else here, with room
/// for as much as anybody could ask for.
const MEMORY_AT: usize = 0x100_0000_0000;
pub const MEMORY_MAX: usize = 0x80_0000_0000;
const PAGE: usize = 4096;
/// How long it is: nought for a volume of a driver's.
static mut MEMORY_BYTES: usize = 0;

/// Serve `bytes` of this server's own memory as the volume, none of it
/// backed until it is written.
pub fn in_memory(bytes: usize) -> Result<(), ()> {
    let pages = bytes / PAGE;
    if pages == 0 || bytes > MEMORY_MAX {
        return Err(());
    }
    syscall::sys_map_anon(MEMORY_AT, pages, false)?;
    unsafe {
        MEMORY_BYTES = pages * PAGE;
        DRIVER = MEMORY;
        VOLUME = 0;
    }
    Ok(())
}

/// The volume, where it is memory.
pub fn memory() -> Option<&'static mut [u8]> {
    let bytes = unsafe { MEMORY_BYTES };
    (bytes != 0).then(|| unsafe { core::slice::from_raw_parts_mut(MEMORY_AT as *mut u8, bytes) })
}

/// The `count` sectors from `lba` of a volume in memory.
fn in_memory_at(lba: u32, count: u32) -> Option<&'static mut [u8]> {
    let memory = memory()?;
    let start = lba as usize * 512;
    let end = start + count as usize * 512;
    memory.get_mut(start..end)
}

/// Nothing is wanted of the `count` sectors from `lba` any more: a block
/// freed. A volume in memory gives back every page they cover whole, and
/// has it reserved again, unbacked, for whatever is written there next. A
/// driver's volume is told nothing.
pub fn discard(lba: u32, count: u32) {
    if memory().is_none() {
        return;
    }
    let start = (lba as usize * 512).next_multiple_of(PAGE);
    let end = (lba as usize + count as usize) * 512 / PAGE * PAGE;
    if end <= start || end > unsafe { MEMORY_BYTES } {
        return;
    }
    let pages = (end - start) / PAGE;
    if syscall::sys_munmap(MEMORY_AT + start, pages).is_ok() && syscall::sys_map_anon(MEMORY_AT + start, pages, false).is_err() {
        // The range was given back and could not be had again: what is
        // read there next would fault. Backed instead, as written.
        let _ = syscall::sys_map_anon(MEMORY_AT + start, pages, true);
    }
}

/// Take `volume` of the driver for this server alone. Nobody else's reads
/// or writes of it are answered from then on.
pub fn claim(disk_tid: usize, volume: u64) -> Result<(), u64> {
    block::claim(disk_tid, volume)?;
    unsafe {
        VOLUME = volume;
        DRIVER = disk_tid;
    }
    Ok(())
}

/// The driver and the volume this server's filesystem is on.
pub fn root() -> (usize, u64) {
    unsafe { (DRIVER, VOLUME) }
}

fn volume() -> u64 {
    unsafe { VOLUME }
}

/// How many sectors the volume has.
pub fn sectors(disk_tid: usize) -> Option<u64> {
    if let Some(memory) = memory() {
        return Some(memory.len() as u64 / 512);
    }
    block::info(disk_tid, volume()).ok().map(|i| i.sectors)
}

/// Read `count` sectors (at most [`MAX_SECTORS`]) from `lba` of the volume
/// into `DISK_IO_BUF`.
pub fn read(disk_tid: usize, lba: u32, count: u32) -> Result<(), ()> {
    let count = count.clamp(1, MAX_SECTORS);
    if memory().is_some() {
        let from = in_memory_at(lba, count).ok_or(())?;
        let buf = unsafe { core::slice::from_raw_parts_mut(DISK_IO_BUF as *mut u8, from.len()) };
        buf.copy_from_slice(from);
        return Ok(());
    }
    let (tag, data) = if count == 1 {
        (block::TAG_READ_SECTOR, [lba as u64, volume(), 0, 0, 0, 0])
    } else {
        (block::TAG_READ_SECTORS, [lba as u64, volume(), count as u64, 0, 0, 0])
    };
    let buf = unsafe {
        core::slice::from_raw_parts_mut(DISK_IO_BUF as *mut u8, count as usize * 512)
    };
    let mut reply = Message::empty();
    match syscall::sys_call_lend_mut(disk_tid, &Message { sender: 0, tag, data }, &mut reply, buf) {
        Ok(()) if reply.tag == block::TAG_OK => Ok(()),
        _ => Err(()),
    }
}

/// Write the first `count` sectors of `DISK_IO_BUF` (at most
/// [`MAX_SECTORS`]) to `lba` of the volume, in one request.
pub fn write_many(disk_tid: usize, lba: u32, count: u32) -> Result<(), ()> {
    if count <= 1 {
        return write(disk_tid, lba);
    }
    let count = count.min(MAX_SECTORS);
    let buf = unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, count as usize * 512) };
    if memory().is_some() {
        in_memory_at(lba, count).ok_or(())?.copy_from_slice(buf);
        return Ok(());
    }
    let msg = Message {
        sender: 0,
        tag: block::TAG_WRITE_SECTORS,
        data: [lba as u64, volume(), count as u64, 0, 0, 0],
    };
    let mut reply = Message::empty();
    match syscall::sys_call_lend(disk_tid, &msg, &mut reply, buf) {
        Ok(()) if reply.tag == block::TAG_OK => Ok(()),
        _ => Err(()),
    }
}

/// Make everything written to the volume lasting: the disk's own cache
/// written out (`block::TAG_FLUSH`). Answered, a machine that loses power
/// keeps all of it.
pub fn flush(disk_tid: usize) -> Result<(), ()> {
    // Memory is as lasting as it gets the moment it is written.
    if memory().is_some() {
        return Ok(());
    }
    block::flush(disk_tid, volume()).map_err(|_| ())
}

/// Write the sector at the start of `DISK_IO_BUF` to `lba` of the volume.
pub fn write(disk_tid: usize, lba: u32) -> Result<(), ()> {
    let buf = unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, 512) };
    if memory().is_some() {
        in_memory_at(lba, 1).ok_or(())?.copy_from_slice(buf);
        return Ok(());
    }
    let msg = Message {
        sender: 0,
        tag: block::TAG_WRITE_SECTOR,
        data: [lba as u64, volume(), 0, 0, 0, 0],
    };
    let mut reply = Message::empty();
    match syscall::sys_call_lend(disk_tid, &msg, &mut reply, buf) {
        Ok(()) if reply.tag == block::TAG_OK => Ok(()),
        _ => Err(()),
    }
}
