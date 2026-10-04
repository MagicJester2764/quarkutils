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

use quark_rt::block;
use quark_rt::ipc::Message;
use quark_rt::syscall;

use crate::DISK_IO_BUF;

/// Sectors one request may carry: as many as fill `DISK_IO_BUF`.
pub const MAX_SECTORS: u32 = block::MAX_SECTORS;

/// The volume this server was started on, and whose it is.
static mut VOLUME: u64 = 0;
static mut DRIVER: usize = 0;

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
    block::info(disk_tid, volume()).ok().map(|i| i.sectors)
}

/// Read `count` sectors (at most [`MAX_SECTORS`]) from `lba` of the volume
/// into `DISK_IO_BUF`.
pub fn read(disk_tid: usize, lba: u32, count: u32) -> Result<(), ()> {
    let count = count.clamp(1, MAX_SECTORS);
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
    block::flush(disk_tid, volume()).map_err(|_| ())
}

/// Write the sector at the start of `DISK_IO_BUF` to `lba` of the volume.
pub fn write(disk_tid: usize, lba: u32) -> Result<(), ()> {
    let buf = unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, 512) };
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
