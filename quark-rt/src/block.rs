//! Block devices: the protocol a disk driver speaks, for both ends of it.
//!
//! A driver serves *volumes*. Volume 0 is the whole device; volume N is its
//! Nth partition, as the partition table on the device says. Every request
//! names a volume, and the sector numbers in it count from the start of that
//! volume: a client that was given a partition cannot reach outside it, and
//! does not need to know where it is.
//!
//! A volume is written by one client at a time. A client *claims* it, and
//! from then on only that client's writes to it are answered, until it lets
//! go or dies. That is what stops a partition being formatted while a file
//! server has it mounted: the file server holds the claim. Reading takes
//! nothing from whoever holds a volume, and root may read one it has not
//! claimed — which is how a program says what is on a disk without taking
//! it from anybody. The whole
//! device and its partitions are the same sectors, so a claim on volume 0
//! and a claim on any other, by different clients, are refused each other.
//!
//! Who may claim is the driver's to say, and it says root: a capability to
//! call a driver is handed to anybody who looks its name up, so the call
//! cannot be the authority.
//!
//! The driver end is [`serve`]: a driver supplies sectors ([`Device`]) and
//! this supplies the rest — volumes, claims, the partition table. A disk and
//! a RAM disk differ in where the sectors are and in nothing else.

use crate::ipc::{death_notice, Message, TAG_PING, TID_ANY};
use crate::syscall;

/// Bytes in a sector. Everything here counts in these.
pub const SECTOR: usize = 512;
/// The most sectors one request carries.
pub const MAX_SECTORS: u32 = 8;
/// The whole device, and as many partitions as are looked for.
pub const MAX_VOLUMES: usize = 17;

/// Read one sector: `[lba, volume]`, lending 512 bytes to be filled.
pub const TAG_READ_SECTOR: u64 = 1;
/// Write one sector: `[lba, volume]`, lending the 512 bytes.
pub const TAG_WRITE_SECTOR: u64 = 2;
/// Ask about a volume: `[_, volume]`. Anybody may. The reply is
/// `[sectors, start, kind, claimant, volumes]`: its size, where on the device
/// it begins, what the partition table calls it, the process id of whoever
/// has claimed it (0 for nobody) and how many volumes the device has.
pub const TAG_INFO: u64 = 3;
/// Read several: `[lba, volume, count]`, lending `count * 512` bytes.
pub const TAG_READ_SECTORS: u64 = 4;
/// Claim a volume: `[_, volume]`.
pub const TAG_CLAIM: u64 = 5;
/// Let a claimed volume go: `[_, volume]`.
pub const TAG_RELEASE: u64 = 6;
/// Read the partition table again. Refused while any partition is claimed:
/// whoever holds one was told where it is.
pub const TAG_RESCAN: u64 = 7;
/// Write several: `[lba, volume, count]`, lending `count * 512` bytes.
pub const TAG_WRITE_SECTORS: u64 = 8;

pub const TAG_OK: u64 = 0;
pub const TAG_ERROR: u64 = u64::MAX;

/// The lent buffer could not be used.
pub const ERR_BUFFER: u64 = 1;
/// The device failed to read, or to write.
pub const ERR_READ: u64 = 2;
pub const ERR_WRITE: u64 = 3;
/// Sectors past the end of the volume.
pub const ERR_RANGE: u64 = 4;
/// The caller has not claimed the volume.
pub const ERR_NOT_CLAIMANT: u64 = 5;
/// Somebody else has it, or has a volume that overlaps it.
pub const ERR_BUSY: u64 = 6;
/// The caller is not allowed to claim volumes.
pub const ERR_NOT_ALLOWED: u64 = 7;
/// The device has no such volume.
pub const ERR_NO_VOLUME: u64 = 8;
pub const ERR_UNKNOWN: u64 = 0xFF;

/// What a volume is, as far as a partition table says.
pub const KIND_WHOLE: u64 = 0;
/// An EFI system partition.
pub const KIND_EFI: u64 = 1;
/// A partition for a filesystem.
pub const KIND_DATA: u64 = 2;
/// A partition of some other type.
pub const KIND_OTHER: u64 = 3;

/// The EFI system partition's type, and the one every Unix uses for "a
/// filesystem", as they are stored on the disk: the first three fields
/// little-endian.
pub const GUID_EFI: [u8; 16] = [
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
];
pub const GUID_DATA: [u8; 16] = [
    0xAF, 0x3D, 0xC6, 0x0F, 0x83, 0x84, 0x72, 0x47, 0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4,
];

// ---------------------------------------------------------------------------
// The client's end.
// ---------------------------------------------------------------------------

/// What a driver says of a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub sectors: u64,
    /// Where on the device it begins.
    pub start: u64,
    pub kind: u64,
    /// The process id of whoever has claimed it; 0 for nobody.
    pub claimant: u64,
    /// How many volumes the device has, the whole one included.
    pub volumes: u64,
}

fn call(server: usize, tag: u64, data: [u64; 6]) -> Result<Message, u64> {
    let mut reply = Message::empty();
    match syscall::sys_call(server, &Message { sender: 0, tag, data }, &mut reply) {
        Ok(()) if reply.tag == TAG_OK => Ok(reply),
        Ok(()) => Err(reply.data[0]),
        Err(()) => Err(ERR_UNKNOWN),
    }
}

/// Ask a driver about one of its volumes.
pub fn info(server: usize, volume: u64) -> Result<Info, u64> {
    let r = call(server, TAG_INFO, [0, volume, 0, 0, 0, 0])?;
    Ok(Info {
        sectors: r.data[0],
        start: r.data[1],
        kind: r.data[2],
        claimant: r.data[3],
        volumes: r.data[4],
    })
}

/// Take a volume for this task alone.
pub fn claim(server: usize, volume: u64) -> Result<(), u64> {
    call(server, TAG_CLAIM, [0, volume, 0, 0, 0, 0]).map(|_| ())
}

/// Let a volume go.
pub fn release(server: usize, volume: u64) -> Result<(), u64> {
    call(server, TAG_RELEASE, [0, volume, 0, 0, 0, 0]).map(|_| ())
}

/// Have the driver read its partition table again. Answers with how many
/// volumes there now are.
pub fn rescan(server: usize) -> Result<u64, u64> {
    call(server, TAG_RESCAN, [0; 6]).map(|r| r.data[0])
}

/// Read whole sectors of a claimed volume, from `lba`, to fill `buf`: as
/// many as it holds, and at most [`MAX_SECTORS`].
pub fn read(server: usize, volume: u64, lba: u64, buf: &mut [u8]) -> Result<(), u64> {
    let count = (buf.len() / SECTOR) as u64;
    if count == 0 || count > MAX_SECTORS as u64 || buf.len() % SECTOR != 0 {
        return Err(ERR_BUFFER);
    }
    let msg = Message { sender: 0, tag: TAG_READ_SECTORS, data: [lba, volume, count, 0, 0, 0] };
    let mut reply = Message::empty();
    match syscall::sys_call_lend_mut(server, &msg, &mut reply, buf) {
        Ok(()) if reply.tag == TAG_OK => Ok(()),
        Ok(()) => Err(reply.data[0]),
        Err(()) => Err(ERR_UNKNOWN),
    }
}

/// Write whole sectors of a claimed volume, from `lba`, out of `buf`.
pub fn write(server: usize, volume: u64, lba: u64, buf: &[u8]) -> Result<(), u64> {
    let count = (buf.len() / SECTOR) as u64;
    if count == 0 || count > MAX_SECTORS as u64 || buf.len() % SECTOR != 0 {
        return Err(ERR_BUFFER);
    }
    let msg = Message { sender: 0, tag: TAG_WRITE_SECTORS, data: [lba, volume, count, 0, 0, 0] };
    let mut reply = Message::empty();
    match syscall::sys_call_lend(server, &msg, &mut reply, buf) {
        Ok(()) if reply.tag == TAG_OK => Ok(()),
        Ok(()) => Err(reply.data[0]),
        Err(()) => Err(ERR_UNKNOWN),
    }
}

// ---------------------------------------------------------------------------
// The driver's end.
// ---------------------------------------------------------------------------

/// Where a driver's sectors are.
pub trait Device {
    /// How many there are.
    fn sectors(&self) -> u64;
    /// Read `count` sectors (at most [`MAX_SECTORS`]) from `lba` into `into`.
    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool;
    /// Write `count` sectors at `lba` out of `from`.
    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool;
}

#[derive(Clone, Copy)]
struct Volume {
    start: u64,
    sectors: u64,
    kind: u64,
    /// The task that has claimed it; 0 for nobody.
    claimant: usize,
}

const NO_VOLUME: Volume = Volume { start: 0, sectors: 0, kind: KIND_WHOLE, claimant: 0 };

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    (le32(b, at) as u64) | (le32(b, at + 4) as u64) << 32
}

/// Read the partition table: the GPT if there is one, and failing that the
/// four entries of an MBR. `volumes[0]` is left alone; the rest are replaced.
/// Returns how many volumes there are, counting the whole device.
///
/// A partition keeps its number: volume N is entry N of the table, and an
/// empty entry is a volume of no sectors rather than a renumbering of the
/// ones after it.
fn scan<D: Device>(dev: &mut D, volumes: &mut [Volume; MAX_VOLUMES], sector: &mut [u8]) -> usize {
    for v in volumes[1..].iter_mut() {
        *v = NO_VOLUME;
    }
    let total = dev.sectors();
    let fits = |start: u64, sectors: u64| start != 0 && sectors != 0 && start.checked_add(sectors).is_some_and(|end| end <= total);
    let mut last = 0;

    // A GPT: its header is the second sector.
    if total > 2 && dev.read(1, 1, &mut sector[..SECTOR]) && &sector[..8] == b"EFI PART" {
        let entries_at = le64(sector, 72);
        let count = le32(sector, 80) as usize;
        let size = le32(sector, 84) as usize;
        if size >= 128 && size <= SECTOR && SECTOR % size == 0 {
            let per_sector = SECTOR / size;
            for n in 0..count.min(MAX_VOLUMES - 1) {
                if n % per_sector == 0
                    && !dev.read(entries_at + (n / per_sector) as u64, 1, &mut sector[..SECTOR])
                {
                    break;
                }
                let e = &sector[(n % per_sector) * size..][..size];
                if e[..16].iter().all(|&b| b == 0) {
                    continue;
                }
                let (first, end) = (le64(e, 32), le64(e, 40));
                if end < first || !fits(first, end - first + 1) {
                    continue;
                }
                let kind = if e[..16] == GUID_EFI {
                    KIND_EFI
                } else if e[..16] == GUID_DATA {
                    KIND_DATA
                } else {
                    KIND_OTHER
                };
                volumes[n + 1] = Volume { start: first, sectors: end - first + 1, kind, claimant: 0 };
                last = n + 1;
            }
            return last + 1;
        }
    }

    // An MBR: four entries of sixteen bytes at 446, and a signature.
    if total > 1 && dev.read(0, 1, &mut sector[..SECTOR]) && sector[510] == 0x55 && sector[511] == 0xAA {
        for n in 0..4 {
            let e = &sector[446 + n * 16..][..16];
            let (kind, first, count) = (e[4], le32(e, 8) as u64, le32(e, 12) as u64);
            // 0xEE is the entry a GPT hides behind, and not a partition.
            if kind == 0 || kind == 0xEE || !fits(first, count) {
                continue;
            }
            let kind = if kind == 0xEF { KIND_EFI } else { KIND_DATA };
            volumes[n + 1] = Volume { start: first, sectors: count, kind, claimant: 0 };
            last = n + 1;
        }
    }
    last + 1
}

fn status(tag: u64, word: u64) -> Message {
    Message { sender: 0, tag, data: [word, 0, 0, 0, 0, 0] }
}

/// Serve `dev` for the rest of this task's life.
///
/// `page` is a page of the driver's own memory, mapped: every sector passes
/// through it on its way to or from what a client lent. The driver is lent
/// the client's buffer by the kernel for the length of the call and never
/// learns where it is.
/// Register a disk's driver under the first of `disk0` to `disk3` that
/// nobody has: the name, or `None` when all four are taken.
pub fn register_disk() -> Option<[u8; 5]> {
    (0..4u8).map(|n| [b'd', b'i', b's', b'k', b'0' + n]).find(|name| crate::nameserver::register(name).is_ok())
}

pub fn serve<D: Device>(dev: &mut D, page: usize) -> ! {
    let buf = unsafe { core::slice::from_raw_parts_mut(page as *mut u8, MAX_SECTORS as usize * SECTOR) };
    let mut volumes = [NO_VOLUME; MAX_VOLUMES];
    volumes[0] = Volume { start: 0, sectors: dev.sectors(), kind: KIND_WHOLE, claimant: 0 };
    let mut count = scan(dev, &mut volumes, &mut buf[..]);

    loop {
        // What the driver's wait for its device heard first.
        let mut msg = Message::empty();
        if let Some(earlier) = crate::ipc::kept() {
            msg = earlier;
        } else if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        if let Some(dead) = death_notice(&msg) {
            for v in volumes.iter_mut().filter(|v| v.claimant == dead) {
                v.claimant = 0;
            }
            continue;
        }
        let sender = msg.sender;
        let volume = msg.data[1] as usize;
        let known = volume < count && (volume == 0 || volumes[volume].sectors != 0);
        let reply = match msg.tag {
            TAG_PING => Message { sender: 0, tag: TAG_PING, data: [0; 6] },
            TAG_INFO if !known => status(TAG_ERROR, ERR_NO_VOLUME),
            TAG_INFO => {
                let v = &volumes[volume];
                let claimant = if v.claimant == 0 { 0 } else { syscall::sys_pid(v.claimant).unwrap_or(0) };
                Message {
                    sender: 0,
                    tag: TAG_OK,
                    data: [v.sectors, v.start, v.kind, claimant, count as u64, 0],
                }
            }
            TAG_CLAIM if !known => status(TAG_ERROR, ERR_NO_VOLUME),
            TAG_CLAIM => {
                // The whole device and a partition of it are the same
                // sectors: one client may have both, two may not have one
                // each.
                let overlaps = |other: usize| volume == 0 || other == 0 || other == volume;
                let taken = volumes
                    .iter()
                    .enumerate()
                    .any(|(i, v)| v.claimant != 0 && v.claimant != sender && overlaps(i));
                if !matches!(syscall::sys_get_tuid(sender), Ok((0, _))) {
                    status(TAG_ERROR, ERR_NOT_ALLOWED)
                } else if taken {
                    status(TAG_ERROR, ERR_BUSY)
                } else {
                    volumes[volume].claimant = sender;
                    // To hear of it going: a claim dies with its claimant.
                    let _ = syscall::sys_task_watch(sender);
                    status(TAG_OK, 0)
                }
            }
            TAG_RELEASE if !known || volumes[volume].claimant != sender => {
                status(TAG_ERROR, ERR_NOT_CLAIMANT)
            }
            TAG_RELEASE => {
                volumes[volume].claimant = 0;
                status(TAG_OK, 0)
            }
            TAG_RESCAN => {
                // For whoever has the whole device, which is who just wrote
                // the table. Not while a partition is in use: its holder was
                // told where it is.
                if volumes[0].claimant != sender {
                    status(TAG_ERROR, ERR_NOT_CLAIMANT)
                } else if volumes[1..].iter().any(|v| v.claimant != 0) {
                    status(TAG_ERROR, ERR_BUSY)
                } else {
                    count = scan(dev, &mut volumes, &mut buf[..]);
                    status(TAG_OK, count as u64)
                }
            }
            TAG_READ_SECTOR | TAG_READ_SECTORS | TAG_WRITE_SECTOR | TAG_WRITE_SECTORS => {
                let single = msg.tag == TAG_READ_SECTOR || msg.tag == TAG_WRITE_SECTOR;
                let n = if single { 1 } else { msg.data[2] };
                let lba = msg.data[0];
                let len = n as usize * SECTOR;
                let reading = msg.tag == TAG_READ_SECTOR || msg.tag == TAG_READ_SECTORS;
                let allowed = known
                    && (volumes[volume].claimant == sender
                        || (reading && matches!(syscall::sys_get_tuid(sender), Ok((0, _)))));
                if !allowed {
                    status(TAG_ERROR, ERR_NOT_CLAIMANT)
                } else if n == 0
                    || n > MAX_SECTORS as u64
                    || lba.checked_add(n).is_none_or(|end| end > volumes[volume].sectors)
                {
                    status(TAG_ERROR, ERR_RANGE)
                } else if reading {
                    if !dev.read(volumes[volume].start + lba, n as u32, &mut buf[..len]) {
                        status(TAG_ERROR, ERR_READ)
                    } else if syscall::sys_lent_write(sender, 0, &buf[..len]).is_err() {
                        status(TAG_ERROR, ERR_BUFFER)
                    } else {
                        status(TAG_OK, len as u64)
                    }
                } else if syscall::sys_lent_read(sender, 0, &mut buf[..len]) != Ok(len) {
                    status(TAG_ERROR, ERR_BUFFER)
                } else if !dev.write(volumes[volume].start + lba, n as u32, &buf[..len]) {
                    status(TAG_ERROR, ERR_WRITE)
                } else {
                    status(TAG_OK, len as u64)
                }
            }
            _ => status(TAG_ERROR, ERR_UNKNOWN),
        };
        let _ = syscall::sys_reply(sender, &reply);
    }
}
