//! `/dev`: the devices every C program expects to find, and the disks.
//!
//! None of them is on a disk. A path under `/dev` is answered here before any
//! filesystem sees it, and a handle on one of these is served here too. The
//! root filesystem still carries an empty `/dev` directory, so that listing
//! `/` shows it.
//!
//! A disk is here as a file: `disk0` is the whole of the first one and
//! `disk0p2` its second partition, and a RAM disk is `ram0`. Each is a volume
//! of a block driver (`quark_rt::block`), found by asking the nameserver for
//! the driver, so what is listed is whatever is there now. It can be read and
//! written at any offset, which is what a program that makes a filesystem or
//! a partition table needs.
//!
//! Opening one claims the volume from its driver for as long as it is open,
//! and that is the whole of the protection: a volume a file server has
//! mounted is that server's, the driver refuses a second claim, and the open
//! fails as busy. The one case the driver cannot see is this server's own
//! root, which it holds itself — so that is refused here, for writing.

use crate::handles::{FsFileData, OpenFile};
use crate::protocol::*;
use crate::{error_reply, lend_in, lend_out, reply_opened, space_of, CLIENT_BUF, PAGE_SIZE};
use quark_rt::block;
use quark_rt::ipc::Message;
use quark_rt::{nameserver, syscall};

#[derive(Clone, Copy, PartialEq)]
pub enum Device {
    Null,
    Zero,
    Full,
    Random,
    Urandom,
    /// A volume of a block driver: which of [`DRIVERS`], and which volume.
    Block { driver: u8, volume: u8 },
}

/// The block drivers there may be, by the names they register under.
const DRIVERS: [&[u8]; 12] = [
    b"disk0", b"disk1", b"disk2", b"disk3", b"ram0", b"ram1", b"ram2", b"ram3", b"ram4", b"ram5",
    b"ram6", b"ram7",
];

/// A volume this server has claimed for somebody who has its device open.
#[derive(Clone, Copy)]
struct Held {
    tid: usize,
    volume: u8,
    /// How many handles there are for it. 0 is a free entry.
    opens: u16,
    sectors: u64,
}

const MAX_HELD: usize = 16;
static mut HELD: [Held; MAX_HELD] = [Held { tid: 0, volume: 0, opens: 0, sectors: 0 }; MAX_HELD];

fn held() -> &'static mut [Held; MAX_HELD] {
    unsafe { &mut *core::ptr::addr_of_mut!(HELD) }
}

/// Sixteen sectors: a page, and the sector at each end it may only half
/// cover.
static mut SECTORS: [u8; 16 * block::SECTOR] = [0; 16 * block::SECTOR];

/// The driver a block device is a volume of, if it is there.
fn driver_of(dev: Device) -> Option<(usize, u64)> {
    match dev {
        Device::Block { driver, volume } => {
            nameserver::lookup(DRIVERS[driver as usize]).map(|tid| (tid, volume as u64))
        }
        _ => None,
    }
}

/// `disk0`, `disk0p2`, `ram1`: a driver's name, and a partition after a `p`.
fn block_by_name(name: &[u8]) -> Option<Device> {
    let (driver, rest) = DRIVERS
        .iter()
        .enumerate()
        .find_map(|(i, d)| name.strip_prefix(*d).map(|rest| (i, rest)))?;
    let volume = match rest {
        [] => 0,
        [b'p', digits @ ..] if !digits.is_empty() && digits.len() <= 2 && digits[0] != b'0' => {
            digits.iter().try_fold(0u8, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0')))?
        }
        _ => return None,
    };
    let dev = Device::Block { driver: driver as u8, volume };
    let (tid, volume) = driver_of(dev)?;
    block::info(tid, volume).ok().map(|_| dev)
}

/// The name of a block device, into `buf`.
fn block_name(driver: usize, volume: u64, buf: &mut [u8; 12]) -> usize {
    let name = DRIVERS[driver];
    buf[..name.len()].copy_from_slice(name);
    let mut len = name.len();
    if volume > 0 {
        buf[len] = b'p';
        len += 1;
        if volume >= 10 {
            buf[len] = b'0' + (volume / 10) as u8;
            len += 1;
        }
        buf[len] = b'0' + (volume % 10) as u8;
        len += 1;
    }
    len
}

/// Whether a device is one this server's own root lies on: its volume, or
/// the whole disk that volume is part of.
fn under_root(tid: usize, volume: u64) -> bool {
    let (root_tid, root_volume) = crate::disk::root();
    tid == root_tid && (volume == 0 || root_volume == 0 || volume == root_volume)
}

/// How many bytes a block device has; 0 if it has gone.
pub fn size_of(dev: Device) -> u64 {
    driver_of(dev)
        .and_then(|(tid, volume)| block::info(tid, volume).ok())
        .map_or(0, |i| i.sectors * block::SECTOR as u64)
}

pub fn is_block(dev: Device) -> bool {
    matches!(dev, Device::Block { .. })
}

/// A handle on a block device has gone: the volume is let go with the last.
pub fn closed(dev: Device) {
    let Some((tid, volume)) = driver_of(dev) else { return };
    if let Some(h) = held().iter_mut().find(|h| h.opens > 0 && h.tid == tid && h.volume as u64 == volume) {
        h.opens -= 1;
        // This server's own root stays claimed: it was never claimed for the
        // sake of the device.
        if h.opens == 0 && !under_root(tid, volume) {
            let _ = block::release(tid, volume);
        }
    }
}

pub const NAMES: [(&[u8], Device); 5] = [
    (b"null", Device::Null),
    (b"zero", Device::Zero),
    (b"full", Device::Full),
    (b"random", Device::Random),
    (b"urandom", Device::Urandom),
];

/// Ids beside anything a filesystem hands out: the devices in the order of
/// [`NAMES`], then the directory.
const FIRST_ID: u64 = 0xFFFF_FF00;
pub const DIR_ID: u64 = FIRST_ID + NAMES.len() as u64;
/// The root directory's inode, which is what `..` names.
const ROOT_ID: u64 = 2;

const DEVICE_MODE: u64 = 0o020666;
/// A disk: a block device, and root's alone.
const BLOCK_MODE: u64 = 0o060600;
const DIR_MODE: u64 = 0o040755;
/// Where block devices' ids begin: thirty-two for each driver, below the
/// character devices.
const BLOCK_ID: u64 = 0xFFFF_FD00;
/// A directory entry's type for one.
const DT_BLK: u8 = 6;

/// What a path names, as far as this module is concerned.
#[derive(Clone, Copy, PartialEq)]
pub enum Lookup {
    /// Not under `/dev`: the filesystem's business.
    Elsewhere,
    Dir,
    Device(Device),
    /// Under `/dev`, and nothing is there.
    Missing,
}

/// Where `path` lands, spelled any way: repeated slashes, `.` and `..` are
/// taken as they would be walked from the root.
pub fn lookup(path: &[u8]) -> Lookup {
    // The first two components that survive are all that matter: nothing
    // under /dev is a directory, so anything deeper is missing. `extra`
    // counts the components past the ones kept.
    let mut kept: [&[u8]; 2] = [b""; 2];
    let mut depth = 0usize;
    let mut extra = 0usize;
    for part in path.split(|&b| b == b'/') {
        match part {
            b"" | b"." => {}
            b".." if extra > 0 => extra -= 1,
            b".." => depth = depth.saturating_sub(1),
            _ if extra == 0 && depth < kept.len() => {
                kept[depth] = part;
                depth += 1;
            }
            _ => extra += 1,
        }
    }
    if depth == 0 || kept[0] != b"dev" {
        return Lookup::Elsewhere;
    }
    if extra > 0 {
        return Lookup::Missing;
    }
    if depth == 1 {
        return Lookup::Dir;
    }
    match by_name(kept[1]) {
        Some(dev) => Lookup::Device(dev),
        None => Lookup::Missing,
    }
}

/// The device called `name` in `/dev`.
pub fn by_name(name: &[u8]) -> Option<Device> {
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, d)| *d).or_else(|| block_by_name(name))
}

/// A device's id, as `STAT` gives it.
pub fn id_of(dev: Device) -> u64 {
    match dev {
        Device::Block { driver, volume } => BLOCK_ID + driver as u64 * 32 + volume as u64,
        _ => FIRST_ID + index_of(dev) as u64,
    }
}

fn index_of(dev: Device) -> usize {
    NAMES.iter().position(|(_, d)| *d == dev).unwrap_or(0)
}

/// OPEN on a path under `/dev`. `found` is what [`lookup`] said, and is not
/// `Elsewhere`.
pub fn open(sender: usize, path: &[u8], found: Lookup, flags: u64) {
    let trailing = path.len() > 1 && path[path.len() - 1] == b'/';
    let mut size = 0;
    let mut writable = true;
    let (is_dir, fs, id, mode, access) = match found {
        Lookup::Dir => (true, FsFileData::DevDir, DIR_ID, DIR_MODE, 5),
        Lookup::Device(dev @ Device::Block { .. }) => {
            if trailing || flags & OPEN_DIRECTORY != 0 {
                return error_reply(sender, ERR_NOT_DIR);
            }
            // Root's, and nobody else's: whoever can write a disk can write
            // anything on it.
            if crate::get_sender_uid_gid(sender).0 != 0 {
                return error_reply(sender, ERR_PERMISSION);
            }
            // A descriptor says what it is for. A handle a program holds for
            // itself says nothing, and is taken to be for reading unless the
            // device may be written.
            let wants_write = flags & OPEN_WRITE != 0;
            match hold(dev, wants_write) {
                Ok((sectors, may_write)) => {
                    size = sectors * block::SECTOR as u64;
                    writable = may_write;
                }
                Err(code) => return error_reply(sender, code),
            }
            (false, FsFileData::Device(dev), id_of(dev), BLOCK_MODE, 6)
        }
        Lookup::Device(dev) => {
            if trailing || flags & OPEN_DIRECTORY != 0 {
                return error_reply(sender, ERR_NOT_DIR);
            }
            (false, FsFileData::Device(dev), FIRST_ID + index_of(dev) as u64, DEVICE_MODE, 6)
        }
        // Nothing can be made here: the devices are the whole of it.
        Lookup::Missing if flags & OPEN_CREATE != 0 => return error_reply(sender, ERR_PERMISSION),
        _ => return error_reply(sender, ERR_NOT_FOUND),
    };
    if flags & OPEN_CREATE != 0 && flags & OPEN_EXCLUSIVE != 0 {
        if let FsFileData::Device(dev) = fs {
            closed(dev);
        }
        return error_reply(sender, ERR_EXISTS);
    }
    let file = OpenFile {
        in_use: true,
        owner: space_of(sender),
        is_dir,
        writable: !is_dir && writable,
        fs,
        ..OpenFile::empty()
    };
    // If the handle cannot be made, `opened` has answered, and a volume
    // claimed for it has nobody holding it.
    let before = crate::handles::count();
    crate::opened(sender, flags, file, [0, size, is_dir as u64, mode, access, id]);
    if crate::handles::count() == before {
        if let Lookup::Device(dev @ Device::Block { .. }) = found {
            closed(dev);
        }
    }
}

/// Claim a block device's volume for one more handle. Answers with its size
/// in sectors and whether it may be written.
fn hold(dev: Device, wants_write: bool) -> Result<(u64, bool), u64> {
    let (tid, volume) = driver_of(dev).ok_or(ERR_NOT_FOUND)?;
    let info = block::info(tid, volume).map_err(|_| ERR_NOT_FOUND)?;
    // The disk this server's own root is on is read, and not written:
    // nothing here can tell a write that would be harmless from one that
    // takes the root from under everything running.
    let may_write = !under_root(tid, volume);
    if wants_write && !may_write {
        return Err(ERR_BUSY);
    }
    let table = held();
    if let Some(h) = table.iter_mut().find(|h| h.opens > 0 && h.tid == tid && h.volume as u64 == volume) {
        h.opens += 1;
        return Ok((info.sectors, may_write));
    }
    let free = table.iter_mut().find(|h| h.opens == 0).ok_or(ERR_TOO_MANY_OPEN)?;
    // Already this server's, if it is the root's own volume.
    let (root_tid, root_volume) = crate::disk::root();
    if !(tid == root_tid && volume == root_volume) {
        block::claim(tid, volume).map_err(|why| match why {
            block::ERR_BUSY => ERR_BUSY,
            block::ERR_NOT_ALLOWED => ERR_PERMISSION,
            _ => ERR_IO,
        })?;
    }
    *free = Held { tid, volume: volume as u8, opens: 1, sectors: info.sectors };
    Ok((info.sectors, may_write))
}

/// TAG_DEVCTL: `[handle, operation]`. Operation 1 has a disk's driver read
/// its partition table again, for whoever has just written one; the handle
/// has to be for the whole disk.
pub fn control(sender: usize, msg: &Message) {
    let dev = match crate::get_handle(msg.data[0] as usize, sender).map(|f| &f.fs) {
        Some(FsFileData::Device(dev @ Device::Block { volume: 0, .. })) => *dev,
        _ => return error_reply(sender, ERR_INVALID_HANDLE),
    };
    let Some((tid, _)) = driver_of(dev) else {
        return error_reply(sender, ERR_IO);
    };
    match msg.data[1] {
        DEVCTL_RESCAN => match block::rescan(tid) {
            Ok(volumes) => reply_opened(sender, [volumes, 0, 0, 0, 0, 0]),
            Err(block::ERR_BUSY) => error_reply(sender, ERR_BUSY),
            Err(_) => error_reply(sender, ERR_IO),
        },
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
}

/// Whether `msg`, a request naming a handle, is for one of these.
pub fn is_ours(sender: usize, msg: &Message) -> bool {
    matches!(
        crate::get_handle(msg.data[0] as usize, sender).map(|f| &f.fs),
        Some(FsFileData::Device(_) | FsFileData::DevDir)
    )
}

/// READ, WRITE, STAT, READDIR_BULK and TRUNCATE on a handle [`is_ours`] said
/// is ours.
pub fn serve(sender: usize, msg: &Message) {
    let Some(file) = crate::get_handle(msg.data[0] as usize, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let target = match file.fs {
        FsFileData::Device(dev) => Some(dev),
        FsFileData::DevDir => None,
        _ => return error_reply(sender, ERR_INVALID_HANDLE),
    };
    match (msg.tag, target) {
        (TAG_READ | TAG_WRITE, Some(dev @ Device::Block { .. })) => {
            let writing = msg.tag == TAG_WRITE;
            if writing && !file.writable {
                return error_reply(sender, ERR_READ_ONLY);
            }
            block_io(sender, dev, writing, msg.data[2], msg.data[3] as usize)
        }
        (TAG_READ, Some(dev)) => read(sender, dev, msg.data[3] as usize),
        (TAG_READ, None) => error_reply(sender, ERR_IS_DIR),
        (TAG_WRITE, Some(Device::Full)) => error_reply(sender, ERR_NO_SPACE),
        // Written and forgotten. Linux would stir the pool with what is
        // written to random; nothing here needs to.
        (TAG_WRITE, Some(_)) => crate::reply_count(sender, msg.data[3].min(PAGE_SIZE as u64)),
        (TAG_WRITE, None) => error_reply(sender, ERR_IS_DIR),
        (TAG_STAT, _) => stat(sender, target),
        (TAG_READDIR_BULK, None) => list(sender, msg.data[1], msg.data[2] as usize),
        (TAG_READDIR_BULK, Some(_)) => error_reply(sender, ERR_NOT_DIR),
        (TAG_TRUNCATE, Some(_)) => error_reply(sender, ERR_INVALID_PATH),
        (TAG_TRUNCATE, None) => error_reply(sender, ERR_IS_DIR),
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
}

fn read(sender: usize, dev: Device, want: usize) {
    let n = want.min(PAGE_SIZE);
    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, n) };
    let n = match dev {
        Device::Null => 0,
        Device::Zero | Device::Full => {
            buf.fill(0);
            n
        }
        Device::Random | Device::Urandom => match quark_rt::random::fill(buf) {
            Ok(()) => n,
            Err(()) => return error_reply(sender, ERR_IO),
        },
        Device::Block { .. } => return error_reply(sender, ERR_INVALID_HANDLE),
    };
    if lend_out(sender, n) {
        crate::reply_count(sender, n as u64);
    } else {
        error_reply(sender, ERR_IO);
    }
    // What was handed out is the caller's alone.
    if matches!(dev, Device::Random | Device::Urandom) {
        buf.fill(0);
    }
}

/// Read or write a block device at any offset: whole sectors go straight
/// through, and a sector only partly covered is read first, so that the rest
/// of it is written back as it was.
fn block_io(sender: usize, dev: Device, writing: bool, offset: u64, len: usize) {
    let Some((tid, volume)) = driver_of(dev) else {
        return error_reply(sender, ERR_IO);
    };
    let size = size_of(dev);
    if offset >= size {
        // Past the end there is nothing to read and nowhere to write.
        return if writing { error_reply(sender, ERR_NO_SPACE) } else { crate::reply_count(sender, 0) };
    }
    let len = len.min(PAGE_SIZE).min((size - offset) as usize);
    if len == 0 {
        return crate::reply_count(sender, 0);
    }
    let sector = block::SECTOR;
    let first = offset / sector as u64;
    let skip = (offset % sector as u64) as usize;
    let span = (skip + len).div_ceil(sector);
    let scratch = unsafe { &mut (&mut *core::ptr::addr_of_mut!(SECTORS))[..span * sector] };
    let client = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, len) };
    // In pieces a driver takes in one request.
    let chunk = block::MAX_SECTORS as usize;
    let transfer = |scratch: &mut [u8], write: bool| -> bool {
        (0..span).step_by(chunk).all(|at| {
            let n = chunk.min(span - at);
            let piece = &mut scratch[at * sector..(at + n) * sector];
            if write {
                block::write(tid, volume, first + at as u64, piece).is_ok()
            } else {
                block::read(tid, volume, first + at as u64, piece).is_ok()
            }
        })
    };
    if !writing {
        if !transfer(scratch, false) {
            return error_reply(sender, ERR_IO);
        }
        client.copy_from_slice(&scratch[skip..skip + len]);
        return if lend_out(sender, len) {
            crate::reply_count(sender, len as u64)
        } else {
            error_reply(sender, ERR_IO)
        };
    }
    if !lend_in(sender, len) {
        return error_reply(sender, ERR_IO);
    }
    // The ends, if the write does not cover them whole.
    let ragged_start = skip != 0;
    let ragged_end = (skip + len) % sector != 0;
    if ragged_start && block::read(tid, volume, first, &mut scratch[..sector]).is_err() {
        return error_reply(sender, ERR_IO);
    }
    if ragged_end
        && (span > 1 || !ragged_start)
        && block::read(tid, volume, first + span as u64 - 1, &mut scratch[(span - 1) * sector..]).is_err()
    {
        return error_reply(sender, ERR_IO);
    }
    scratch[skip..skip + len].copy_from_slice(client);
    if transfer(scratch, true) {
        crate::reply_count(sender, len as u64)
    } else {
        error_reply(sender, ERR_IO)
    }
}

fn stat(sender: usize, target: Option<Device>) {
    let now = syscall::unix_time();
    let (id, mode, links) = match target {
        Some(dev @ Device::Block { .. }) => (id_of(dev), BLOCK_MODE, 1),
        Some(dev) => (FIRST_ID + index_of(dev) as u64, DEVICE_MODE, 1),
        None => (DIR_ID, DIR_MODE, 2),
    };
    let record = StatRecord {
        id,
        size: target.map_or(0, size_of),
        mode,
        links,
        uid: 0,
        gid: 0,
        atime: now,
        mtime: now,
        ctime: now,
        blocks: 0,
        block_size: PAGE_SIZE as u64,
    };
    match syscall::sys_lent_write(sender, 0, &record.to_bytes()) {
        Ok(n) if n == STAT_LEN => reply_opened(sender, [STAT_LEN as u64, 0, 0, 0, 0, 0]),
        _ => error_reply(sender, ERR_IO),
    }
}

/// The directory: `.`, `..`, the devices, and then whatever disks there are
/// now, as a bulk read lists them.
fn list(sender: usize, start: u64, room: usize) {
    let room = room.min(PAGE_SIZE);
    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, room) };
    let mut used = 0;
    let mut next = start;
    let mut end = true;
    let mut index = 0u64;
    // Each entry in turn; false once one does not fit.
    let mut put = |id: u64, kind: u8, name: &[u8]| -> bool {
        let this = index;
        index += 1;
        if this < start || !end {
            return end;
        }
        match put_dirent(buf, used, id, this + 1, 0, kind, name) {
            Some(len) => {
                used += len;
                next = this + 1;
            }
            None => end = false,
        }
        end
    };
    put(DIR_ID, DT_DIR, b".");
    put(ROOT_ID, DT_DIR, b"..");
    for (i, (name, _)) in NAMES.iter().enumerate() {
        put(FIRST_ID + i as u64, DT_CHR, name);
    }
    'drivers: for (d, name) in DRIVERS.iter().enumerate() {
        let Some(tid) = nameserver::lookup(name) else { continue };
        let volumes = block::info(tid, 0).map_or(0, |i| i.volumes);
        for volume in 0..volumes {
            // An empty slot in a partition table is not a device.
            if block::info(tid, volume).is_err() {
                continue;
            }
            let mut text = [0u8; 12];
            let len = block_name(d, volume, &mut text);
            if !put(BLOCK_ID + d as u64 * 32 + volume, DT_BLK, &text[..len]) {
                break 'drivers;
            }
        }
    }
    crate::reply_dirents(sender, used, next, end);
}

/// Whether a request that changes names touches `/dev`, where nothing may be
/// made, removed or renamed.
pub fn refuses(path: &[u8]) -> bool {
    lookup(path) != Lookup::Elsewhere
}
