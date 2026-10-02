#![no_std]
#![no_main]
#![allow(dead_code)]
#![allow(static_mut_refs)]

pub mod ext2;
pub mod ext2_alloc;
pub mod ext2_dir;
pub mod ext4;
pub mod csum;
pub mod cwd;
pub mod devices;
pub mod disk;
pub mod ext2_ops;
pub mod handles;
pub mod journal;
pub mod locks;
pub mod mounts;
pub mod pager;
pub mod protocol;
pub mod who;

pub use protocol::*;
use handles::{FsFileData, OpenFile};

use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::nameserver;
use quark_rt::{println, syscall};

use quark_rt::manifest::CapReq;

// A server, and nothing else. It used to hold all of physical memory in order
// to map the page each client named for its data, and frames of its own for
// its buffers; clients lend their buffers with the call now, and the buffers
// here are ordinary memory.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_SERVER),
]);

pub const PAGE_SIZE: usize = 4096;

/// A filesystem being mounted says nothing of how it started: whoever is at
/// the terminal asked for a mount, not for an account of one. What goes
/// wrong is said either way.
static mut QUIET: bool = false;

macro_rules! say {
    ($($arg:tt)*) => {
        if !unsafe { QUIET } {
            println!($($arg)*);
        }
    };
}

// The disk driver's protocol is `quark_rt::block`; `disk` is this server's
// use of it.

// The VFS protocol's own numbers live in `protocol`.

// ---------------------------------------------------------------------------
// Filesystem type detection
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum FsType {
    Fat32,
    Ext2,
}

static mut FS_TYPE: FsType = FsType::Fat32;
/// The journal, if the mounted filesystem has one.
///
/// A global for the same reason as the filesystem state: the sector read and
/// write paths that have to consult it are free functions several layers below
/// anything holding a reference.
static mut JOURNAL: journal::Journal = journal::Journal::empty();

pub fn journal_ref() -> &'static journal::Journal {
    unsafe { &*core::ptr::addr_of!(JOURNAL) }
}

pub fn journal_mut() -> &'static mut journal::Journal {
    unsafe { &mut *core::ptr::addr_of_mut!(JOURNAL) }
}

/// The mounted filesystem's state.
///
/// A flat static rather than an `Option`, and never assigned as a whole: it
/// holds the entire block group descriptor table, so moving one through a
/// local overflows this server's stack. `FS_TYPE` says whether it is mounted.
static mut EXT2_STATE: ext2::Ext2State = ext2::Ext2State::empty();

/// Whether the filesystem here is one of the ext family, and not FAT32.
pub(crate) fn is_ext2() -> bool {
    unsafe { FS_TYPE == FsType::Ext2 }
}

pub(crate) fn ext2_state() -> &'static ext2::Ext2State {
    unsafe { &*core::ptr::addr_of!(EXT2_STATE) }
}

fn ext2_state_mut() -> &'static mut ext2::Ext2State {
    unsafe { &mut *core::ptr::addr_of_mut!(EXT2_STATE) }
}

// Where this server keeps its buffers.
pub const DISK_IO_BUF: usize = 0x86_0000_0000;
/// A page of this server's own that file data passes through on its way to or
/// from the buffer a client lent: filled and then copied out for a read,
/// copied in and then written for a write.
pub const CLIENT_BUF: usize = 0x87_0000_0000;
pub const CACHE_BUF_BASE: usize = 0x8A_0000_0000;
/// Pages behind the sector cache: 256 sectors of 512 bytes.
const CACHE_PAGES: usize = 32;

// ---------------------------------------------------------------------------
// Sector cache
// ---------------------------------------------------------------------------

const CACHE_ENTRIES: usize = 256;
const HASH_BUCKETS: usize = 128;
const NONE: u16 = 0xFFFF; // sentinel for "no entry"

pub struct CacheEntry {
    valid: bool,
    used: bool,
    lba: u32,
    hash_next: u16, // next slot in hash chain, NONE = end
}

pub struct SectorCache {
    entries: [CacheEntry; CACHE_ENTRIES],
    buckets: [u16; HASH_BUCKETS],
    clock_hand: usize,
}

impl SectorCache {
    pub const fn new() -> Self {
        const EMPTY: CacheEntry = CacheEntry {
            valid: false,
            used: false,
            lba: 0,
            hash_next: NONE,
        };
        SectorCache {
            entries: [EMPTY; CACHE_ENTRIES],
            buckets: [NONE; HASH_BUCKETS],
            clock_hand: 0,
        }
    }

    /// Look up a sector in the cache via hash chain. O(1) average.
    pub fn lookup(&mut self, lba: u32) -> Option<usize> {
        let bucket = (lba as usize) % HASH_BUCKETS;
        let mut idx = self.buckets[bucket];
        while idx != NONE {
            let i = idx as usize;
            if self.entries[i].valid && self.entries[i].lba == lba {
                self.entries[i].used = true;
                return Some(i);
            }
            idx = self.entries[i].hash_next;
        }
        None
    }

    /// Find a victim slot using clock eviction and insert a new sector.
    /// `src` is the memory address containing the 512-byte sector data.
    /// Returns the slot index.
    pub fn insert(&mut self, lba: u32, src: usize) -> usize {
        // First check for an invalid (empty) slot
        for i in 0..CACHE_ENTRIES {
            if !self.entries[i].valid {
                self.fill_slot(i, lba, src);
                self.link(i);
                return i;
            }
        }
        // Clock eviction
        loop {
            let i = self.clock_hand;
            self.clock_hand = (self.clock_hand + 1) % CACHE_ENTRIES;
            if self.entries[i].used {
                self.entries[i].used = false;
            } else {
                self.unlink(i);
                self.fill_slot(i, lba, src);
                self.link(i);
                return i;
            }
        }
    }

    fn fill_slot(&mut self, idx: usize, lba: u32, src: usize) {
        // Copy from source address into cache slot
        unsafe {
            core::ptr::copy_nonoverlapping(
                src as *const u8,
                (CACHE_BUF_BASE + idx * 512) as *mut u8,
                512,
            );
        }
        self.entries[idx] = CacheEntry {
            valid: true,
            used: true,
            lba,
            hash_next: NONE,
        };
    }

    /// Prepend slot `idx` to its LBA's hash bucket chain.
    fn link(&mut self, idx: usize) {
        let bucket = (self.entries[idx].lba as usize) % HASH_BUCKETS;
        self.entries[idx].hash_next = self.buckets[bucket];
        self.buckets[bucket] = idx as u16;
    }

    /// Remove slot `idx` from its hash bucket chain.
    fn unlink(&mut self, idx: usize) {
        let bucket = (self.entries[idx].lba as usize) % HASH_BUCKETS;
        let target = idx as u16;
        if self.buckets[bucket] == target {
            self.buckets[bucket] = self.entries[idx].hash_next;
        } else {
            let mut prev = self.buckets[bucket];
            while prev != NONE {
                let p = prev as usize;
                if self.entries[p].hash_next == target {
                    self.entries[p].hash_next = self.entries[idx].hash_next;
                    break;
                }
                prev = self.entries[p].hash_next;
            }
        }
        self.entries[idx].hash_next = NONE;
    }

    /// Drop everything. Used when an abandoned transaction means the cache
    /// may hold writes that are never going to reach the disk.
    pub fn flush_all(&mut self) {
        for i in 0..CACHE_ENTRIES {
            if self.entries[i].valid {
                self.invalidate(self.entries[i].lba);
            }
        }
    }

    /// Invalidate any cached copy of a given LBA.
    pub fn invalidate(&mut self, lba: u32) {
        if let Some(idx) = self.lookup(lba) {
            self.unlink(idx);
            self.entries[idx].valid = false;
        }
    }
}

pub static mut SECTOR_CACHE: SectorCache = SectorCache::new();

// ---------------------------------------------------------------------------
// FAT32 structures
// ---------------------------------------------------------------------------

struct Bpb {
    bytes_per_sector: u32,
    sectors_per_cluster: u32,
    reserved_sectors: u32,
    num_fats: u32,
    fat_size_32: u32,
    root_cluster: u32,
    /// How many sectors the volume has, and which of them holds the counts
    /// kept for whoever mounts it next (0 if none does).
    total_sectors: u32,
    fs_info: u32,
}

fn parse_bpb(data: &[u8]) -> Bpb {
    Bpb {
        bytes_per_sector: read_u16(data, 11) as u32,
        sectors_per_cluster: data[13] as u32,
        reserved_sectors: read_u16(data, 14) as u32,
        num_fats: data[16] as u32,
        fat_size_32: read_u32(data, 36),
        root_cluster: read_u32(data, 44),
        total_sectors: match read_u32(data, 32) {
            0 => read_u16(data, 19) as u32,
            n => n,
        },
        fs_info: read_u16(data, 48) as u32,
    }
}

/// Where the last cluster was taken: the next search for a free one starts
/// after it, rather than at the front of the table every time.
static mut FAT_HINT: u32 = 2;
/// Clusters freed, less clusters taken, since the count on the disk was last
/// brought up to date.
static mut FAT_FREED: i64 = 0;

pub fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

pub fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

// ---------------------------------------------------------------------------
// Disk reader (communicates with disk driver via IPC)
// ---------------------------------------------------------------------------

pub(crate) struct DiskState {
    disk_tid: usize,
    part_lba: u32,
    bpb: Bpb,
}

impl DiskState {
    fn raw_read_sector(disk_tid: usize, lba: u32) -> Result<(), ()> {
        disk::read(disk_tid, lba, 1)
    }

    fn read_sector(&self, lba: u32) -> Result<(), ()> {
        Self::raw_read_sector(self.disk_tid, self.part_lba + lba)
    }

    /// Read a sector through the cache. Returns a slice to cached data.
    fn cached_read_sector(&self, lba: u32) -> Result<&[u8], ()> {
        let abs_lba = self.part_lba + lba;
        let cache = unsafe { &mut SECTOR_CACHE };
        let idx = if let Some(i) = cache.lookup(abs_lba) {
            i
        } else {
            // Cache miss — read from disk into DISK_IO_BUF, then insert into cache
            Self::raw_read_sector(self.disk_tid, abs_lba)?;
            cache.insert(abs_lba, DISK_IO_BUF)
        };
        Ok(unsafe { core::slice::from_raw_parts((CACHE_BUF_BASE + idx * 512) as *const u8, 512) })
    }

    /// Read multiple consecutive sectors into DISK_IO_BUF (up to 8, fitting one 4K page).
    fn raw_read_sectors(disk_tid: usize, start_lba: u32, count: u32) -> Result<(), ()> {
        disk::read(disk_tid, start_lba, count)
    }

    /// Prefetch consecutive sectors into the cache using a single multi-sector IPC call.
    fn prefetch_sectors(&self, start_lba: u32, count: u32) {
        let count = count.min(8) as usize;
        let cache = unsafe { &mut SECTOR_CACHE };

        // Check how many sectors are already cached
        let mut all_cached = true;
        for i in 0..count {
            let abs_lba = self.part_lba + start_lba + i as u32;
            if cache.lookup(abs_lba).is_none() {
                all_cached = false;
                break;
            }
        }
        if all_cached {
            return;
        }

        // Read all sectors in one IPC call
        let abs_start = self.part_lba + start_lba;
        if Self::raw_read_sectors(self.disk_tid, abs_start, count as u32).is_err() {
            return;
        }

        // Insert each sector into the cache
        for i in 0..count {
            let abs_lba = abs_start + i as u32;
            if cache.lookup(abs_lba).is_none() {
                cache.insert(abs_lba, DISK_IO_BUF + i * 512);
            }
        }
    }

    fn sector_data(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, 512) }
    }

    fn fat_next(&self, cluster: u32) -> Option<u32> {
        let fat_byte_off = (cluster as usize) * 4;
        let sector_in_fat = fat_byte_off / 512;
        let offset_in_sector = fat_byte_off % 512;
        let lba = self.bpb.reserved_sectors + sector_in_fat as u32;
        let data = self.cached_read_sector(lba).ok()?;
        let next = read_u32(data, offset_in_sector) & 0x0FFF_FFFF;
        if next >= 0x0FFF_FFF8 { None } else { Some(next) }
    }

    fn cluster_start_lba(&self, cluster: u32) -> u32 {
        let data_start = self.bpb.reserved_sectors + self.bpb.num_fats * self.bpb.fat_size_32;
        data_start + (cluster - 2) * self.bpb.sectors_per_cluster
    }

    fn write_sector(&self, lba: u32) -> Result<(), ()> {
        disk::write(self.disk_tid, self.part_lba + lba)
    }

    fn sector_data_mut(&self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(DISK_IO_BUF as *mut u8, 512) }
    }

    /// Write a FAT entry: set fat[cluster] = value.
    fn fat_set(&self, cluster: u32, value: u32) -> Result<(), ()> {
        let fat_byte_off = (cluster as usize) * 4;
        let sector_in_fat = fat_byte_off / 512;
        let offset_in_sector = fat_byte_off % 512;
        let lba = self.bpb.reserved_sectors + sector_in_fat as u32;

        // Read the FAT sector
        self.read_sector(lba).map_err(|_| ())?;

        // Modify the entry (preserve top 4 bits)
        let data = self.sector_data_mut();
        let old = read_u32(data, offset_in_sector);
        let new_val = (old & 0xF000_0000) | (value & 0x0FFF_FFFF);
        let bytes = new_val.to_le_bytes();
        data[offset_in_sector..offset_in_sector + 4].copy_from_slice(&bytes);

        // Write back
        self.write_sector(lba).map_err(|_| ())?;
        unsafe { SECTOR_CACHE.invalidate(self.part_lba + lba); }

        // Update second FAT copy if present (buffer still has modified sector)
        if self.bpb.num_fats > 1 {
            let lba2 = lba + self.bpb.fat_size_32;
            self.write_sector(lba2).map_err(|_| ())?;
            unsafe { SECTOR_CACHE.invalidate(self.part_lba + lba2); }
        }

        Ok(())
    }

    /// How many clusters the volume has. They are numbered from 2, and the
    /// table may have room for more than there are.
    fn cluster_count(&self) -> u32 {
        let data_start = self.bpb.reserved_sectors + self.bpb.num_fats * self.bpb.fat_size_32;
        let on_disk = self.bpb.total_sectors.saturating_sub(data_start) / self.bpb.sectors_per_cluster.max(1);
        on_disk.min((self.bpb.fat_size_32 * 512 / 4).saturating_sub(2))
    }

    /// Allocate a free cluster. Marks it as EOF in the FAT.
    fn fat_alloc(&self) -> Result<u32, ()> {
        let count = self.cluster_count();
        if count == 0 {
            return Err(());
        }
        let from = unsafe { FAT_HINT }.clamp(2, count + 1) - 2;
        // Scan FAT for a free entry (value == 0), once round from the hint.
        for step in 0..count {
            let cluster = 2 + (from + step) % count;
            let fat_byte_off = (cluster as usize) * 4;
            let sector_in_fat = fat_byte_off / 512;
            let offset_in_sector = fat_byte_off % 512;
            let lba = self.bpb.reserved_sectors + sector_in_fat as u32;

            let data = match self.cached_read_sector(lba) {
                Ok(d) => d,
                Err(()) => continue,
            };
            let val = read_u32(data, offset_in_sector) & 0x0FFF_FFFF;
            if val == 0 {
                // Mark as EOF
                self.fat_set(cluster, 0x0FFF_FFFF)?;
                unsafe {
                    FAT_HINT = cluster + 1;
                    FAT_FREED -= 1;
                }
                return Ok(cluster);
            }
        }
        Err(()) // disk full
    }

    /// Give back a file's clusters, from `first` to the end of its chain.
    fn fat_free_chain(&self, first: u32) -> Result<(), ()> {
        let end = self.cluster_count() + 2;
        let mut cluster = first;
        // No chain is longer than the volume: one that loops is damage, and
        // is not followed for ever.
        for _ in 0..end {
            if cluster < 2 || cluster >= end {
                break;
            }
            let next = self.fat_next(cluster);
            self.fat_set(cluster, 0)?;
            unsafe {
                FAT_FREED += 1;
                FAT_HINT = FAT_HINT.min(cluster);
            }
            match next {
                Some(n) => cluster = n,
                None => break,
            }
        }
        Ok(())
    }

    /// Bring the count of free clusters the filesystem keeps — for whoever
    /// mounts it next, so that they need not count — up to date with what
    /// has been taken and given back. Left alone it is wrong, and a checker
    /// says so.
    fn fat_sync_info(&self) {
        let freed = unsafe { core::mem::take(&mut *core::ptr::addr_of_mut!(FAT_FREED)) };
        let sector = self.bpb.fs_info;
        if freed == 0 || sector == 0 || sector >= self.bpb.reserved_sectors {
            return;
        }
        if self.read_sector(sector).is_err() {
            return;
        }
        let data = self.sector_data_mut();
        if read_u32(data, 0) != 0x4161_5252 || read_u32(data, 484) != 0x6141_7272 {
            return;
        }
        // All ones is "nobody has counted", and stays that.
        let free = read_u32(data, 488);
        if free != 0xFFFF_FFFF {
            let now = (free as i64 + freed).clamp(0, self.cluster_count() as i64) as u32;
            data[488..492].copy_from_slice(&now.to_le_bytes());
        }
        data[492..496].copy_from_slice(&unsafe { FAT_HINT }.to_le_bytes());
        if self.write_sector(sector).is_ok() {
            unsafe { SECTOR_CACHE.invalidate(self.part_lba + sector) };
        }
    }

    /// Extend a cluster chain by allocating a new cluster and linking it.
    fn fat_extend(&self, last_cluster: u32) -> Result<u32, ()> {
        let new_cluster = self.fat_alloc()?;
        self.fat_set(last_cluster, new_cluster)?;
        Ok(new_cluster)
    }

    /// Zero out a cluster's data sectors.
    fn zero_cluster(&self, cluster: u32) -> Result<(), ()> {
        let start_lba = self.cluster_start_lba(cluster);
        let data = self.sector_data_mut();
        for i in 0..512 {
            data[i] = 0;
        }
        for s in 0..self.bpb.sectors_per_cluster {
            self.write_sector(start_lba + s).map_err(|_| ())?;
            unsafe { SECTOR_CACHE.invalidate(self.part_lba + start_lba + s); }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Open file table (see `handles`)
// ---------------------------------------------------------------------------

/// What this caller may do with an inode, as the rwx bits `access(2)` asks
/// about: 4 read, 2 write, 1 execute.
///
/// The client asks the server rather than working it out from a mode, because
/// the answer depends on the file's owner and the caller's identity and the
/// server is the only party that knows both. A client that recomputed it would
/// be keeping a second copy of this system's permission policy.
fn access_bits(inode: &ext2::Ext2Inode, uid: u32, gid: u32) -> u64 {
    let mut bits = 0;
    for probe in [4u16, 2, 1] {
        if ext2::check_permission(inode, uid, gid, probe) {
            bits |= probe as u64;
        }
    }
    // User 0 may read and write whatever the mode says, but a file is run
    // only if it is one somebody may run. A file with no execute bit at all
    // is not a program even to user 0, as on Linux — which is what stops a
    // shell running a text file because nobody said it could not.
    if uid == 0 && !inode.is_dir() && inode.i_mode & 0o111 == 0 {
        bits &= !1;
    }
    bits
}

/// FAT has no owners and no mode bits, and this server checks nothing on it.
/// Saying "anyone may do anything" is a description of what will actually
/// happen rather than a default standing in for information we lost.
const FAT_FILE_MODE: u64 = 0o100777;
const FAT_DIR_MODE: u64 = 0o040777;
const FAT_ACCESS: u64 = 7;
/// A FAT32 cluster's size, which is what a file's blocks come in.
static mut FAT_CLUSTER_BYTES: u32 = 512;

fn fat32_file(
    tid: usize,
    cluster: u32,
    size: u32,
    is_dir: bool,
    dir_cluster: u32,
    fat_name: &[u8; 11],
) -> OpenFile {
    OpenFile {
        in_use: true,
        owner: space_of(tid),
        file_size: size,
        is_dir,
        writable: true, // FAT32: no permission checks
        link: false,
        read_offset: 0,
        fs: FsFileData::Fat32 {
            first_cluster: cluster,
            cur_cluster: cluster,
            cur_cluster_offset: 0,
            dir_cluster,
            fat_name: *fat_name,
        },
        ..OpenFile::empty()
    }
}

/// The program a caller belongs to, or 0 if it has none (and so owns nothing).
pub(crate) fn space_of(sender: usize) -> u64 {
    syscall::sys_task_space(sender).unwrap_or(0)
}

/// The handle `sender` may use as `handle`: its program's, or one a
/// descriptor it holds names. For the second the kernel is asked, and its
/// answer is the whole of the authority — a task holds a cookie only by
/// having been given a descriptor for it.
pub(crate) fn get_handle(handle: usize, sender: usize) -> Option<&'static mut OpenFile> {
    if handles::is_descriptor(handle) {
        return if syscall::sys_fd_holds(sender, handle as u64) {
            handles::descriptor(handle)
        } else {
            None
        };
    }
    handles::get(handle, space_of(sender))
}

/// Where `sender` is: the directory in its descriptor table's slot for one,
/// if this server put it there; else the one kept for its program, which is
/// what FAT32 has and what a program nobody moved has; else the root.
fn cwd_of(sender: usize) -> (cwd::Where, &'static [u8]) {
    if let Some(cookie) = syscall::sys_fd_cookie(sender, syscall::FD_CWD) {
        if let Some(file) = handles::descriptor(cookie as usize) {
            match file.fs {
                FsFileData::Ext2 { inode_num } if file.is_dir => {
                    return if inode_num == ext2::EXT2_ROOT_INO {
                        (cwd::Where::Root, b"/")
                    } else {
                        (cwd::Where::Inode(inode_num), b"/")
                    };
                }
                FsFileData::DevDir if ext2_dir::dev_dir() != 0 => {
                    return (cwd::Where::Inode(ext2_dir::dev_dir()), b"/");
                }
                _ => {}
            }
        }
    }
    cwd::get(space_of(sender))
}

/// Put `file` in the table and answer the OPEN that asked for it. For a
/// descriptor the caller is given one, and told which.
pub fn opened(sender: usize, flags: u64, mut file: OpenFile, mut words: [u64; 6]) {
    let by_fd = flags & OPEN_DESCRIPTOR != 0;
    if by_fd {
        file.by_fd = true;
        file.append = flags & OPEN_APPEND != 0;
        file.may_read = flags & OPEN_READ != 0;
        file.may_write = flags & OPEN_WRITE != 0;
    }
    let Some(handle) = handles::alloc(file) else {
        return error_reply(sender, ERR_TOO_MANY_OPEN);
    };
    words[0] = handle as u64;
    if by_fd {
        match syscall::sys_fd_serve(sender, handle as u64, syscall::ANY_FD) {
            Ok(fd) => words[0] = (handle as u64) << 32 | fd as u64,
            Err(()) => {
                // Its table is full, or the kernel's. Nothing names the
                // handle, so nothing will ever say it has been closed.
                let _ = handles::release(handle);
                return error_reply(sender, ERR_TOO_MANY_OPEN);
            }
        }
    }
    reply_opened(sender, words);
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Convert a path component to FAT 8.3 name.
/// Input: "HELLO.ELF" or "USR" (uppercase, no long names)
/// Output: "HELLO   ELF" or "USR        "
fn to_fat83(component: &[u8], out: &mut [u8; 11]) {
    *out = [b' '; 11];

    // Find dot separator
    let dot_pos = component.iter().position(|&b| b == b'.');

    let (base, ext) = match dot_pos {
        Some(pos) => (&component[..pos], &component[pos + 1..]),
        None => (component, &[] as &[u8]),
    };

    // Copy base name (up to 8 chars), uppercase
    let base_len = base.len().min(8);
    for i in 0..base_len {
        out[i] = base[i].to_ascii_uppercase();
    }

    // Copy extension (up to 3 chars), uppercase
    let ext_len = ext.len().min(3);
    for i in 0..ext_len {
        out[8 + i] = ext[i].to_ascii_uppercase();
    }
}

/// Resolve a path like "/USR/BIN/HELLO.ELF" to (cluster, size, is_dir, parent_cluster, fat_name).
/// Paths use "/" separators. Leading "/" is optional.
fn resolve_path(
    disk: &DiskState,
    path: &[u8],
) -> Result<(u32, u32, bool, u32, [u8; 11]), u64> {
    let path = if !path.is_empty() && path[0] == b'/' {
        &path[1..]
    } else {
        path
    };

    if path.is_empty() {
        // Root directory
        let root_name = [b' '; 11];
        return Ok((disk.bpb.root_cluster, 0, true, 0, root_name));
    }

    let mut current_cluster = disk.bpb.root_cluster;

    // Split path into components
    let mut remaining = path;
    loop {
        // Find next "/" or end
        let (component, rest) = match remaining.iter().position(|&b| b == b'/') {
            Some(pos) => (&remaining[..pos], &remaining[pos + 1..]),
            None => (remaining, &[] as &[u8]),
        };

        if component.is_empty() {
            remaining = rest;
            if remaining.is_empty() {
                let root_name = [b' '; 11];
                return Ok((current_cluster, 0, true, 0, root_name));
            }
            continue;
        }

        let mut target = [0u8; 11];
        to_fat83(component, &mut target);

        let is_last = rest.is_empty();

        // Search directory for this component
        match find_entry(disk, current_cluster, &target)? {
            Some((cluster, size, is_dir)) => {
                if is_last {
                    return Ok((cluster, size, is_dir, current_cluster, target));
                }
                // Intermediate component must be a directory
                if !is_dir {
                    return Err(ERR_NOT_DIR);
                }
                current_cluster = cluster;
                remaining = rest;
            }
            None => return Err(ERR_NOT_FOUND),
        }
    }
}

/// Search a directory for an entry matching the given FAT 8.3 name.
/// Returns (cluster, size, is_dir) or None.
fn find_entry(
    disk: &DiskState,
    dir_cluster: u32,
    name: &[u8; 11],
) -> Result<Option<(u32, u32, bool)>, u64> {
    let spc = disk.bpb.sectors_per_cluster;
    let mut cluster = dir_cluster;

    loop {
        let start_lba = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start_lba, spc);
        for s in 0..spc {
            let sec_data = disk.cached_read_sector(start_lba + s).map_err(|_| ERR_IO)?;
            let mut sec_buf = [0u8; 512];
            sec_buf.copy_from_slice(sec_data);

            for e in 0..16 {
                let off = e * 32;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 {
                    return Ok(None); // end of directory
                }
                if first_byte == 0xE5 {
                    continue;
                }
                let attr = sec_buf[off + 11];
                if attr & 0x0F == 0x0F {
                    continue; // LFN
                }
                if attr & 0x08 != 0 {
                    continue; // volume label
                }

                if &sec_buf[off..off + 11] == name {
                    let hi = read_u16(&sec_buf, off + 20) as u32;
                    let lo = read_u16(&sec_buf, off + 26) as u32;
                    let cluster = (hi << 16) | lo;
                    let size = read_u32(&sec_buf, off + 28);
                    let is_dir = attr & 0x10 != 0;
                    return Ok(Some((cluster, size, is_dir)));
                }
            }
        }
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => break,
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Read file data into CLIENT_BUF
// ---------------------------------------------------------------------------

/// Read up to `max_bytes` (at most a page) from a file at `offset` into
/// `CLIENT_BUF`. Returns bytes actually read.
fn read_file_data(
    disk: &DiskState,
    file: &mut OpenFile,
    offset: u32,
    max_bytes: u32,
) -> Result<u32, u64> {
    if file.is_dir {
        return Err(ERR_IS_DIR);
    }
    // What a handle knows of its file is what the directory said when it was
    // opened. Before saying there is nothing more, ask again: another handle
    // may have written since.
    if offset >= file.file_size {
        if let FsFileData::Fat32 { first_cluster, cur_cluster, cur_cluster_offset, dir_cluster, fat_name } =
            &mut file.fs
        {
            if let Ok(Some((cluster, size, false))) = find_entry(disk, *dir_cluster, fat_name) {
                if *first_cluster != cluster {
                    *first_cluster = cluster;
                    *cur_cluster = cluster;
                    *cur_cluster_offset = 0;
                }
                file.file_size = size;
            }
        }
        if offset >= file.file_size {
            return Ok(0);
        }
    }

    let (first_cluster, cur_cluster, cur_cluster_offset) = match &file.fs {
        FsFileData::Fat32 { first_cluster, cur_cluster, cur_cluster_offset, .. } =>
            (*first_cluster, *cur_cluster, *cur_cluster_offset),
        _ => return Err(ERR_IO),
    };

    let available = file.file_size - offset;
    let to_read = max_bytes.min(available).min(PAGE_SIZE as u32);
    if to_read == 0 {
        return Ok(0);
    }

    let cluster_bytes = disk.bpb.sectors_per_cluster * disk.bpb.bytes_per_sector;

    // Navigate to the cluster containing `offset`
    let mut cluster;
    let mut byte_pos;

    // Use cached position if we can advance from it
    if offset >= cur_cluster_offset && cur_cluster != 0 {
        cluster = cur_cluster;
        byte_pos = cur_cluster_offset;
    } else {
        cluster = first_cluster;
        byte_pos = 0;
    }

    // Skip clusters until we reach the one containing `offset`
    while byte_pos + cluster_bytes <= offset {
        match disk.fat_next(cluster) {
            Some(next) => {
                cluster = next;
                byte_pos += cluster_bytes;
            }
            None => return Ok(0),
        }
    }

    // Cache the position
    if let FsFileData::Fat32 { cur_cluster: cc, cur_cluster_offset: co, .. } = &mut file.fs {
        *cc = cluster;
        *co = byte_pos;
    }

    // Prefetch all sectors in the current cluster
    let cluster_lba = disk.cluster_start_lba(cluster);
    disk.prefetch_sectors(cluster_lba, disk.bpb.sectors_per_cluster);

    let mut written = 0u32;

    while written < to_read {
        let offset_in_cluster = (offset + written) - byte_pos;
        let sector_in_cluster = offset_in_cluster / disk.bpb.bytes_per_sector;
        let offset_in_sector = offset_in_cluster % disk.bpb.bytes_per_sector;

        let lba = disk.cluster_start_lba(cluster) + sector_in_cluster;
        let sec_data = disk.cached_read_sector(lba).map_err(|_| ERR_IO)?;

        let copy_start = offset_in_sector as usize;
        let copy_len = (512 - copy_start).min((to_read - written) as usize);

        unsafe {
            core::ptr::copy_nonoverlapping(
                sec_data.as_ptr().add(copy_start),
                (CLIENT_BUF + written as usize) as *mut u8,
                copy_len,
            );
        }

        written += copy_len as u32;

        // Check if we need to move to next cluster
        let new_offset_in_cluster = offset_in_cluster + copy_len as u32;
        if new_offset_in_cluster >= cluster_bytes && written < to_read {
            match disk.fat_next(cluster) {
                Some(next) => {
                    cluster = next;
                    byte_pos += cluster_bytes;
                    if let FsFileData::Fat32 { cur_cluster: cc, cur_cluster_offset: co, .. } = &mut file.fs {
                        *cc = cluster;
                        *co = byte_pos;
                    }
                    // Prefetch next cluster's sectors
                    let next_lba = disk.cluster_start_lba(cluster);
                    disk.prefetch_sectors(next_lba, disk.bpb.sectors_per_cluster);
                }
                None => break,
            }
        }
    }

    file.read_offset = offset + written;

    Ok(written)
}

// ---------------------------------------------------------------------------
// Read directory entries
// ---------------------------------------------------------------------------

/// Read directory entry at `index` from a directory.
/// Returns entry info packed into IPC message data words:
///   data[0] = handle (echo back)
///   data[1..2] = 8.3 name (11 bytes in 2 words)
///   data[3] = file_size
///   data[4] = (is_dir << 32) | first_cluster
///   data[5] = attr
fn read_dir_entry(
    disk: &DiskState,
    dir_cluster: u32,
    index: u32,
) -> Result<Option<(u32, [u8; 11], u32, bool, u8)>, u64> {
    let spc = disk.bpb.sectors_per_cluster;
    let mut cluster = dir_cluster;
    let mut current_idx: u32 = 0;

    loop {
        let start_lba = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start_lba, spc);
        for s in 0..spc {
            let sec_data = disk.cached_read_sector(start_lba + s).map_err(|_| ERR_IO)?;
            let mut sec_buf = [0u8; 512];
            sec_buf.copy_from_slice(sec_data);

            for e in 0..16 {
                let off = e * 32;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 {
                    return Ok(None);
                }
                if first_byte == 0xE5 {
                    continue;
                }
                let attr = sec_buf[off + 11];
                if attr & 0x0F == 0x0F {
                    continue; // LFN
                }
                if attr & 0x08 != 0 {
                    continue; // volume label
                }

                if current_idx == index {
                    let mut name = [0u8; 11];
                    name.copy_from_slice(&sec_buf[off..off + 11]);
                    let hi = read_u16(&sec_buf, off + 20) as u32;
                    let lo = read_u16(&sec_buf, off + 26) as u32;
                    let size = read_u32(&sec_buf, off + 28);
                    let is_dir = attr & 0x10 != 0;
                    let _cluster = (hi << 16) | lo;
                    return Ok(Some((_cluster, name, size, is_dir, attr)));
                }
                current_idx += 1;
            }
        }
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => break,
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Create a new directory entry
// ---------------------------------------------------------------------------

/// Now, as a FAT directory entry says it: a date (years from 1980, month,
/// day) and a time (hours, minutes, seconds in twos). An entry with neither
/// has the zeroth day of the zeroth month, which no calendar has.
fn fat_now() -> (u16, u16) {
    let seconds = syscall::unix_time();
    let days = (seconds / 86400) as i64;
    let rest = seconds % 86400;
    // Days since 1970 to a date, the way every C library does it.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    // FAT counts from 1980 and stops at 2107. A machine with no clock is in
    // 1970, before any of it: it gets the first day FAT has.
    if !(1980..=2107).contains(&year) {
        return (1 << 5 | 1, 0);
    }
    let date = ((year - 1980) << 9 | month << 5 | day) as u16;
    let time = (rest / 3600 << 11 | rest % 3600 / 60 << 5 | rest % 60 / 2) as u16;
    (date, time)
}

/// Create a new file entry in a directory. Returns the first cluster of the new file.
fn create_dir_entry(
    disk: &DiskState,
    dir_cluster: u32,
    name: &[u8; 11],
    is_dir: bool,
) -> Result<u32, u64> {
    // Check if name already exists
    if let Ok(Some(_)) = find_entry(disk, dir_cluster, name) {
        return Err(ERR_INVALID_PATH); // already exists
    }

    // A directory has a cluster from the start, for `.` and `..`. A file has
    // none until something is written to it: an empty file with a cluster is
    // a file whose length and whose chain disagree, and a checker says so.
    let new_cluster = if is_dir {
        let cluster = disk.fat_alloc().map_err(|_| ERR_NO_SPACE)?;
        disk.zero_cluster(cluster).map_err(|_| ERR_IO)?;
        cluster
    } else {
        0
    };

    // If creating a directory, write "." and ".." entries
    if is_dir {
        let start_lba = disk.cluster_start_lba(new_cluster);
        if disk.read_sector(start_lba).is_err() {
            return Err(ERR_IO);
        }
        let sec = disk.sector_data_mut();

        // "." entry — points to self
        sec[0..11].copy_from_slice(b".          ");
        sec[11] = 0x10; // directory attribute
        let cl_hi = ((new_cluster >> 16) & 0xFFFF) as u16;
        let cl_lo = (new_cluster & 0xFFFF) as u16;
        sec[20..22].copy_from_slice(&cl_hi.to_le_bytes());
        sec[26..28].copy_from_slice(&cl_lo.to_le_bytes());

        // ".." entry — points to parent
        sec[32..43].copy_from_slice(b"..         ");
        sec[43] = 0x10;
        let (date, time) = fat_now();
        for at in [0usize, 32] {
            sec[at + 14..at + 16].copy_from_slice(&time.to_le_bytes());
            sec[at + 16..at + 18].copy_from_slice(&date.to_le_bytes());
            sec[at + 18..at + 20].copy_from_slice(&date.to_le_bytes());
            sec[at + 22..at + 24].copy_from_slice(&time.to_le_bytes());
            sec[at + 24..at + 26].copy_from_slice(&date.to_le_bytes());
        }
        let parent_cl = if dir_cluster == disk.bpb.root_cluster { 0 } else { dir_cluster };
        let p_hi = ((parent_cl >> 16) & 0xFFFF) as u16;
        let p_lo = (parent_cl & 0xFFFF) as u16;
        sec[52..54].copy_from_slice(&p_hi.to_le_bytes());
        sec[58..60].copy_from_slice(&p_lo.to_le_bytes());

        disk.write_sector(start_lba).map_err(|_| ERR_IO)?;
        unsafe { SECTOR_CACHE.invalidate(disk.part_lba + start_lba); }
    }

    // Find a free slot in the parent directory
    let spc = disk.bpb.sectors_per_cluster;
    let mut cluster = dir_cluster;

    loop {
        let start_lba = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start_lba, spc);
        for s in 0..spc {
            let sec_data = disk.cached_read_sector(start_lba + s).map_err(|_| ERR_IO)?;
            let mut sec_buf = [0u8; 512];
            sec_buf.copy_from_slice(sec_data);

            for e in 0..16 {
                let off = e * 32;
                let first_byte = sec_buf[off];
                // Free slot: 0x00 (end of dir) or 0xE5 (deleted)
                if first_byte == 0x00 || first_byte == 0xE5 {
                    // Write the new entry
                    sec_buf[off..off + 11].copy_from_slice(name);
                    sec_buf[off + 11] = if is_dir { 0x10 } else { 0x20 }; // dir or archive
                    // Zero out remaining fields, and say when it was made.
                    for i in 12..32 {
                        sec_buf[off + i] = 0;
                    }
                    let (date, time) = fat_now();
                    sec_buf[off + 14..off + 16].copy_from_slice(&time.to_le_bytes());
                    sec_buf[off + 16..off + 18].copy_from_slice(&date.to_le_bytes());
                    sec_buf[off + 18..off + 20].copy_from_slice(&date.to_le_bytes());
                    sec_buf[off + 22..off + 24].copy_from_slice(&time.to_le_bytes());
                    sec_buf[off + 24..off + 26].copy_from_slice(&date.to_le_bytes());
                    // Set first cluster
                    let cl_hi = ((new_cluster >> 16) & 0xFFFF) as u16;
                    let cl_lo = (new_cluster & 0xFFFF) as u16;
                    sec_buf[off + 20..off + 22].copy_from_slice(&cl_hi.to_le_bytes());
                    sec_buf[off + 26..off + 28].copy_from_slice(&cl_lo.to_le_bytes());
                    // Size = 0 initially
                    sec_buf[off + 28..off + 32].copy_from_slice(&0u32.to_le_bytes());

                    // If this was end-of-dir (0x00), mark next slot as end if room
                    if first_byte == 0x00 && e + 1 < 16 {
                        sec_buf[(e + 1) * 32] = 0x00;
                    }

                    // Write sector back
                    let data = disk.sector_data_mut();
                    data.copy_from_slice(&sec_buf);
                    disk.write_sector(start_lba + s).map_err(|_| ERR_IO)?;
                    unsafe { SECTOR_CACHE.invalidate(disk.part_lba + start_lba + s); }

                    return Ok(new_cluster);
                }
            }
        }
        // Extend the directory with a new cluster
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => {
                let new_dir_cluster = disk.fat_extend(cluster).map_err(|_| ERR_IO)?;
                disk.zero_cluster(new_dir_cluster).map_err(|_| ERR_IO)?;
                cluster = new_dir_cluster;
                // Loop again — the zeroed cluster will have 0x00 entries
            }
        }
    }
}

/// Update the file size in its directory entry.
fn update_dir_entry_size(
    disk: &DiskState,
    dir_cluster: u32,
    name: &[u8; 11],
    new_size: u32,
) -> Result<(), u64> {
    update_dir_entry(disk, dir_cluster, name, Change::Size(new_size))
}

/// What [`update_dir_entry`] does to an entry.
#[derive(Clone, Copy)]
enum Change {
    Size(u32),
    /// Where the file's first cluster is, and how long the file is.
    Start(u32, u32),
    /// The name is free again.
    Remove,
}

/// Change the entry called `name` in a directory.
fn update_dir_entry(
    disk: &DiskState,
    dir_cluster: u32,
    name: &[u8; 11],
    change: Change,
) -> Result<(), u64> {
    let spc = disk.bpb.sectors_per_cluster;
    let mut cluster = dir_cluster;

    loop {
        let start_lba = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start_lba, spc);
        for s in 0..spc {
            let sec_data = disk.cached_read_sector(start_lba + s).map_err(|_| ERR_IO)?;
            let mut sec_buf = [0u8; 512];
            sec_buf.copy_from_slice(sec_data);

            for e in 0..16 {
                let off = e * 32;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 {
                    return Err(ERR_NOT_FOUND);
                }
                if first_byte == 0xE5 {
                    continue;
                }
                let attr = sec_buf[off + 11];
                if attr & 0x0F == 0x0F || attr & 0x08 != 0 {
                    continue;
                }
                if &sec_buf[off..off + 11] == name {
                    match change {
                        Change::Size(size) => sec_buf[off + 28..off + 32].copy_from_slice(&size.to_le_bytes()),
                        Change::Start(cluster, size) => {
                            sec_buf[off + 20..off + 22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
                            sec_buf[off + 26..off + 28].copy_from_slice(&(cluster as u16).to_le_bytes());
                            sec_buf[off + 28..off + 32].copy_from_slice(&size.to_le_bytes());
                        }
                        Change::Remove => sec_buf[off] = 0xE5,
                    }
                    // Written to: when.
                    if !matches!(change, Change::Remove) {
                        let (date, time) = fat_now();
                        sec_buf[off + 22..off + 24].copy_from_slice(&time.to_le_bytes());
                        sec_buf[off + 24..off + 26].copy_from_slice(&date.to_le_bytes());
                    }
                    let data = disk.sector_data_mut();
                    data.copy_from_slice(&sec_buf);
                    disk.write_sector(start_lba + s).map_err(|_| ERR_IO)?;
                    unsafe { SECTOR_CACHE.invalidate(disk.part_lba + start_lba + s); }
                    return Ok(());
                }
            }
        }
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => break,
        }
    }
    Err(ERR_NOT_FOUND)
}

// ---------------------------------------------------------------------------
// Write file data from CLIENT_BUF
// ---------------------------------------------------------------------------

/// Write up to `len` bytes (at most a page) from `CLIENT_BUF` to a file at
/// `offset`. Returns bytes actually written.
fn write_file_data(
    disk: &DiskState,
    file: &mut OpenFile,
    offset: u32,
    len: u32,
) -> Result<u32, u64> {
    if file.is_dir {
        return Err(ERR_IS_DIR);
    }

    let (mut first_cluster, dir_cluster, fat_name) = match &file.fs {
        FsFileData::Fat32 { first_cluster, dir_cluster, fat_name, .. } => (*first_cluster, *dir_cluster, *fat_name),
        _ => return Err(ERR_IO),
    };

    let to_write = len.min(PAGE_SIZE as u32);
    if to_write == 0 {
        return Ok(0);
    }

    // An empty file has no cluster. It is given its first here — unless the
    // directory says another handle on it already has.
    if first_cluster == 0 {
        first_cluster = match find_entry(disk, dir_cluster, &fat_name)? {
            Some((cluster, size, false)) if cluster != 0 => {
                file.file_size = file.file_size.max(size);
                cluster
            }
            Some((_, _, false)) => {
                let cluster = disk.fat_alloc().map_err(|_| ERR_NO_SPACE)?;
                disk.zero_cluster(cluster).map_err(|_| ERR_IO)?;
                update_dir_entry(disk, dir_cluster, &fat_name, Change::Start(cluster, 0))?;
                cluster
            }
            _ => return Err(ERR_NOT_FOUND),
        };
        if let FsFileData::Fat32 { first_cluster: fc, cur_cluster: cc, cur_cluster_offset: co, .. } = &mut file.fs {
            *fc = first_cluster;
            *cc = first_cluster;
            *co = 0;
        }
    }

    let cluster_bytes = disk.bpb.sectors_per_cluster * disk.bpb.bytes_per_sector;

    // Navigate to the cluster containing `offset`, allocating as needed
    let mut cluster = first_cluster;
    let mut byte_pos: u32 = 0;

    // Skip clusters until we reach the one containing `offset`
    while byte_pos + cluster_bytes <= offset {
        match disk.fat_next(cluster) {
            Some(next) => {
                cluster = next;
                byte_pos += cluster_bytes;
            }
            None => {
                // Need to allocate more clusters to reach the offset
                let new = disk.fat_extend(cluster).map_err(|_| ERR_IO)?;
                disk.zero_cluster(new).map_err(|_| ERR_IO)?;
                cluster = new;
                byte_pos += cluster_bytes;
            }
        }
    }

    let mut written = 0u32;

    while written < to_write {
        let offset_in_cluster = (offset + written) - byte_pos;
        let sector_in_cluster = offset_in_cluster / disk.bpb.bytes_per_sector;
        let offset_in_sector = offset_in_cluster % disk.bpb.bytes_per_sector;

        let lba = disk.cluster_start_lba(cluster) + sector_in_cluster;

        // Read existing sector data (for partial-sector writes)
        if disk.read_sector(lba).is_err() {
            return Err(ERR_IO);
        }

        let copy_start = offset_in_sector as usize;
        let copy_len = (512 - copy_start).min((to_write - written) as usize);

        // Copy from client buffer into disk I/O buffer
        unsafe {
            core::ptr::copy_nonoverlapping(
                (CLIENT_BUF + written as usize) as *const u8,
                (DISK_IO_BUF + copy_start) as *mut u8,
                copy_len,
            );
        }

        // Write sector back to disk
        disk.write_sector(lba).map_err(|_| ERR_IO)?;
        unsafe { SECTOR_CACHE.invalidate(disk.part_lba + lba); }

        written += copy_len as u32;

        // Check if we need to move to next cluster
        let new_offset_in_cluster = offset_in_cluster + copy_len as u32;
        if new_offset_in_cluster >= cluster_bytes && written < to_write {
            match disk.fat_next(cluster) {
                Some(next) => {
                    cluster = next;
                    byte_pos += cluster_bytes;
                }
                None => {
                    let new = disk.fat_extend(cluster).map_err(|_| ERR_IO)?;
                    disk.zero_cluster(new).map_err(|_| ERR_IO)?;
                    cluster = new;
                    byte_pos += cluster_bytes;
                }
            }
        }
    }

    // Update cached position
    if let FsFileData::Fat32 { cur_cluster: cc, cur_cluster_offset: co, .. } = &mut file.fs {
        *cc = cluster;
        *co = byte_pos;
    }

    // Update file size if we wrote past the end
    let new_end = offset + written;
    if new_end > file.file_size {
        file.file_size = new_end;
    }

    Ok(written)
}

pub(crate) fn error_reply(sender: usize, err_code: u64) {
    let reply = Message {
        sender: 0,
        tag: TAG_ERROR,
        data: [err_code, 0, 0, 0, 0, 0],
    };
    let _ = syscall::sys_reply(sender, &reply);
}


// ---------------------------------------------------------------------------
// Cache warmup
// ---------------------------------------------------------------------------

/// Prefetch FAT table sectors and root directory cluster into the sector cache.
/// This eliminates cold-miss IPC round-trips on the first readdir/ls.
fn warm_cache(disk: &DiskState) {
    let fat_start = disk.bpb.reserved_sectors;
    let fat_sectors = disk.bpb.fat_size_32.min(64); // cap at 64 sectors (32 KiB of FAT)

    // Prefetch FAT in 8-sector batches
    let mut lba = 0u32;
    while lba < fat_sectors {
        let batch = (fat_sectors - lba).min(8);
        disk.prefetch_sectors(fat_start + lba, batch);
        lba += batch;
    }

    // Prefetch root directory's first cluster
    let root_lba = disk.cluster_start_lba(disk.bpb.root_cluster);
    disk.prefetch_sectors(root_lba, disk.bpb.sectors_per_cluster.min(8));
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // `vfs DRIVER VOLUME mount` is a filesystem to be mounted in another:
    // it has no name to be looked up by, and is called by whoever started
    // it and by the server it is mounted in. The one with a name is the
    // root.
    let to_be_mounted = quark_rt::args::argv(3) == Some(b"mount");
    unsafe { QUIET = to_be_mounted };
    if to_be_mounted {
        mounts::to_be_mounted();
        // Its `dev` is a directory on its disk. The devices are the root's.
        devices::disable();
    }
    say!("[vfs] Started.");

    // What to serve: `vfs DRIVER VOLUME`, a block driver by the name it
    // registered under and one of its volumes. Whoever starts this says;
    // with nothing said it is the first disk, and the volume a disk laid out
    // the usual way keeps its root on.
    let driver = quark_rt::args::argv(1).unwrap_or(b"disk0");
    let disk_tid = match nameserver::lookup_retry(driver, 20) {
        Some(tid) => tid,
        None => {
            println!("[vfs] No block driver called {}. Exiting.", core::str::from_utf8(driver).unwrap_or("?"));
            syscall::sys_exit();
        }
    };
    let volume = match quark_rt::args::argv(2) {
        Some(arg) => arg.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64)),
        // An EFI partition and then the root; or one partition; or no table
        // at all, and the filesystem on the device itself.
        None => quark_rt::block::info(disk_tid, 0).ok().map(|i| i.volumes.saturating_sub(1).min(2)),
    };
    let Some(volume) = volume else {
        println!("[vfs] No volume to serve. Exiting.");
        syscall::sys_exit();
    };
    say!(
        "[vfs] Serving volume {} of {} (TID {})",
        volume,
        core::str::from_utf8(driver).unwrap_or("?"),
        disk_tid
    );
    if let Err(why) = disk::claim(disk_tid, volume) {
        println!("[vfs] The volume cannot be had ({}). Exiting.", why);
        syscall::sys_exit();
    }

    // The page every sector passes through, and the sector cache (32 pages,
    // 256 sectors). Ordinary memory: the disk driver is lent the one and
    // never sees the other.
    if syscall::sys_mmap(DISK_IO_BUF, 1).is_err()
        || syscall::sys_mmap(CLIENT_BUF, 1).is_err()
        || syscall::sys_mmap(protocol::PATH_BUF, protocol::PATH_BUF_PAGES).is_err()
        || syscall::sys_mmap(mounts::RELAY_BUF, mounts::RELAY_PAGES).is_err()
        || syscall::sys_mmap(CACHE_BUF_BASE, CACHE_PAGES).is_err()
    {
        println!("[vfs] No memory for disk buffers.");
        syscall::sys_exit();
    }

    // The volume begins at its own sector 0, wherever that is on the disk:
    // the driver knows, and this does not need to.
    let part_lba: u32 = 0;

    // Detect filesystem type: check for ext2 magic at partition offset 1024 (sector 2)
    if DiskState::raw_read_sector(disk_tid, part_lba + 2).is_ok() {
        let sb_data = unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, 512) };
        let magic = read_u16(sb_data, 56);
        if magic == ext2::EXT2_MAGIC {
            match ext2::init_ext2(ext2_state_mut(), disk_tid, part_lba) {
                Ok(()) => {
                    let state = ext2_state();
                    say!(
                        "[vfs] {} detected: blocks={} inodes={} block_size={} groups={}{}",
                        if state.is_ext4() { "ext4" } else { "ext2" },
                        state.total_blocks, state.total_inodes,
                        state.block_size, state.num_block_groups,
                        if state.read_only { " (read-only)" } else { "" }
                    );
                    unsafe { FS_TYPE = FsType::Ext2 };

                    // The journal, and whatever it says did not finish.
                    if journal::map_buffers().is_err() {
                        println!("[vfs] could not map journal buffers; mounting read-only");
                        ext2_state_mut().read_only = true;
                    } else {
                        match journal::load(journal_mut(), ext2_state()) {
                            Ok(true) => {
                                if let Err(e) = journal::recover(journal_mut(), ext2_state()) {
                                    println!("[vfs] journal recovery failed ({}); read-only", e);
                                    ext2_state_mut().read_only = true;
                                } else if let Err(e) =
                                    journal::checkpoint_done(journal_mut(), ext2_state())
                                {
                                    println!("[vfs] could not clear the journal ({}); read-only", e);
                                    ext2_state_mut().read_only = true;
                                } else if ext2::read_state(ext2_state_mut()).is_err() {
                                    // What the replay changed, read again
                                    // over what was read before it.
                                    println!("[vfs] could not read the filesystem back after its journal; read-only");
                                    ext2_state_mut().read_only = true;
                                } else {
                                    say!("[vfs] journal ready");
                                }
                            }
                            Ok(false) => {
                                // No journal. If the filesystem says it needs
                                // recovering, nothing here can do it.
                                if ext2_state().feature_incompat & ext4::INCOMPAT_RECOVER != 0 {
                                    println!(
                                        "[vfs] needs recovery but has no journal; read-only"
                                    );
                                    ext2_state_mut().read_only = true;
                                }
                            }
                            Err(e) => {
                                println!("[vfs] journal unusable ({}); mounting read-only", e);
                                ext2_state_mut().read_only = true;
                            }
                        }
                    }
                }
                Err(()) => {
                    println!("[vfs] mount failed, falling back to FAT32.");
                }
            }
        }
    }

    // Create a dummy DiskState for FAT32 (needed even in ext2 mode for the service loop signature)
    let disk = if unsafe { FS_TYPE } == FsType::Fat32 {
        // Read BPB
        if DiskState::raw_read_sector(disk_tid, part_lba).is_err() {
            println!("[vfs] Failed to read BPB.");
            syscall::sys_exit();
        }
        let data = unsafe { core::slice::from_raw_parts(DISK_IO_BUF as *const u8, 512) };
        let bpb = parse_bpb(data);
        // Anything that is not ext2 was taken for FAT32, and a volume with
        // nothing on it is neither: its first sector describes a filesystem
        // of no sectors in clusters of none. Nor is FAT12 or FAT16, which
        // keep the root directory in a place of its own and say how big it
        // is and how long one table is in two fields FAT32 leaves at nothing
        // — and keep other things where FAT32 keeps the two this reads, so a
        // FAT16 volume used to be mounted and then fail every read.
        let plausible = data[510] == 0x55
            && data[511] == 0xAA
            && bpb.bytes_per_sector == 512
            && bpb.sectors_per_cluster.is_power_of_two()
            && bpb.num_fats >= 1
            && read_u16(data, 17) == 0
            && read_u16(data, 22) == 0
            && bpb.fat_size_32 != 0
            && bpb.root_cluster >= 2;
        if !plausible {
            println!("[vfs] No filesystem this knows on the volume. Exiting.");
            let _ = quark_rt::block::release(disk_tid, volume);
            syscall::sys_exit();
        }
        say!(
            "[vfs] FAT32: bps={} spc={} reserved={} root={}",
            bpb.bytes_per_sector, bpb.sectors_per_cluster,
            bpb.reserved_sectors, bpb.root_cluster
        );

        unsafe {
            FAT_CLUSTER_BYTES = bpb.bytes_per_sector as u32 * bpb.sectors_per_cluster as u32;
        }
        let d = DiskState { disk_tid, part_lba, bpb };
        warm_cache(&d);
        d
    } else {
        // Dummy — won't be used for ext2 path
        DiskState {
            disk_tid,
            part_lba,
            bpb: Bpb {
                bytes_per_sector: 512,
                sectors_per_cluster: 1,
                reserved_sectors: 0,
                num_fats: 0,
                fat_size_32: 0,
                root_cluster: 0,
                total_sectors: 0,
                fs_info: 0,
            },
        }
    };

    if unsafe { FS_TYPE } == FsType::Ext2 {
        if !to_be_mounted {
            ext2_dir::note_dev_dir(ext2_state());
        }
        if !ext2_state().read_only {
            recover_orphans();
        }
    }

    // What this serves, for whoever asks what is mounted.
    if is_ext2() {
        let kind = if ext2_state().is_ext4() { KIND_EXT4 } else { KIND_EXT2 };
        mounts::describe(driver, volume, ext2::EXT2_ROOT_INO as u64, kind);
    } else {
        mounts::describe(driver, volume, disk.bpb.root_cluster as u64, KIND_FAT);
    }

    if to_be_mounted {
        // Nothing to register: see above.
    } else if nameserver::register(b"vfs").is_ok() {
        println!("[vfs] Registered with nameserver.");
    } else {
        println!("[vfs] Failed to register with nameserver.");
    }

    // Service loop
    loop {
        let mut msg = Message::empty();
        // A mapped file nothing maps any more, which could not be let go
        // when the kernel said so.
        if pager::owed() {
            transacted(pager::retry);
        }
        // Writes are waiting for another to join them. Not for long: half a
        // second at most, whatever else is being asked meanwhile.
        if journal::in_transaction(journal_ref())
            && syscall::sys_ticks().wrapping_sub(unsafe { WAITING_SINCE }) >= MAX_WAIT_TICKS
        {
            commit_pending();
        }
        if journal::in_transaction(journal_ref()) {
            // And with nothing asked at all, what they changed is committed
            // at once.
            if syscall::sys_recv_timeout(TID_ANY, &mut msg, FLUSH_TICKS).is_err() {
                commit_pending();
                continue;
            }
        } else if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }

        let sender = msg.sender;

        // From the kernel, which is not waiting for an answer: a program has
        // gone, or a task that may have been waiting for a lock. The same
        // tags from anybody else are unknown requests.
        who::began(sender);
        if let Some(space) = quark_rt::ipc::space_death_notice(&msg) {
            // The server this is mounted in, if that is who it was: this
            // ends with it.
            mounts::program_gone(space);
            client_died(space);
            continue;
        }
        if let Some(tid) = quark_rt::ipc::death_notice(&msg) {
            locks::drop_task(tid);
            continue;
        }
        // The last descriptor for something has closed. One notice however
        // many there are: collect until there are none.
        if quark_rt::ipc::fd_released_notice(&msg) {
            while let Some(cookie) = syscall::sys_fd_reap() {
                descriptor_closed(cookie as usize);
            }
            continue;
        }
        // A task in a call is not waiting in an earlier one: a lock it asked
        // for and gave up on is not a request any more, and granting it later
        // would hand a lock to a program that had stopped asking.
        locks::drop_task(sender);

        // The kernel, reading or writing through a descriptor for a task that
        // may know nothing of this protocol: `[cookie, length]`, wherever the
        // descriptor is. The same thing as a client asking for itself, and
        // checked the same way — the tag proves nothing, holding the cookie
        // does.
        let msg = match msg.tag {
            quark_rt::ipc::TAG_FD_READ | quark_rt::ipc::TAG_FD_WRITE => Message {
                sender,
                tag: if msg.tag == quark_rt::ipc::TAG_FD_READ { TAG_READ } else { TAG_WRITE },
                data: [msg.data[0], 0, AT_POSITION, msg.data[1].min(PAGE_SIZE as u64), 0, 0],
            },
            _ => msg,
        };

        // A path that leads into a filesystem mounted here, or a word from
        // the server this one is mounted in.
        if mounts::intercept(&disk, sender, &msg) {
            continue;
        }

        match msg.tag {
            TAG_READ | TAG_WRITE if handles::is_descriptor(msg.data[0] as usize) => {
                descriptor_io(&disk, sender, &msg)
            }
            TAG_READDIR_BULK if msg.data[1] == AT_POSITION => descriptor_list(&disk, sender, &msg),
            TAG_SEEK => handle_seek(sender, &msg),
            TAG_DEVCTL => devices::control(sender, &msg),
            TAG_SETATTR => transacted(|| handle_setattr(sender, &msg)),
            _ => dispatch(&disk, sender, &msg),
        }
        // What the request took and gave back, counted where a FAT
        // filesystem keeps count.
        if !is_ext2() {
            disk.fat_sync_info();
        }
    }
}

/// Every request that is not about where a descriptor is.
fn dispatch(disk: &DiskState, sender: usize, msg: &Message) {
    // A FAT filesystem has no owners and no modes to check anything against.
    // It is root's to change and anybody's to read, which is the least that
    // keeps a user off the partition a machine starts from — and what a
    // Unix makes of one, mounted as it comes.
    if unsafe { FS_TYPE } != FsType::Ext2 && get_sender_uid_gid(sender).0 != 0 {
        let changes = match msg.tag {
            TAG_WRITE | TAG_MKDIR | TAG_MKNOD | TAG_UNLINK | TAG_RMDIR | TAG_RENAME | TAG_LINK
            | TAG_SYMLINK | TAG_TRUNCATE | TAG_SETATTR => true,
            TAG_OPEN => msg.data[1] & (OPEN_CREATE | OPEN_TRUNCATE | OPEN_WRITE | OPEN_APPEND) != 0,
            _ => false,
        };
        if changes {
            return error_reply(sender, ERR_PERMISSION);
        }
    }
    match msg.tag {
        // A link opened as itself, or anything opened only to be asked
        // about, answers STAT and nothing else — whatever it is, which is
        // why this comes before anybody else is asked whose handle it is.
        TAG_READ | TAG_WRITE | TAG_READDIR_BULK | TAG_TRUNCATE
            if get_handle(msg.data[0] as usize, sender).is_some_and(|f| f.link) =>
        {
            error_reply(sender, ERR_NOT_SUPPORTED)
        }
        TAG_READ | TAG_WRITE | TAG_STAT | TAG_READDIR_BULK | TAG_TRUNCATE
            if mounts::is_ours(sender, msg) =>
        {
            mounts::serve(sender, msg)
        }
        TAG_READ | TAG_WRITE | TAG_STAT | TAG_READDIR_BULK | TAG_TRUNCATE
            if devices::is_ours(sender, msg) =>
        {
            devices::serve(sender, msg)
        }
        TAG_OPEN if msg.data[1] & (OPEN_CREATE | OPEN_TRUNCATE) != 0 => {
            transacted(|| handle_open(disk, sender, msg))
        }
        TAG_OPEN => handle_open(disk, sender, msg),
        TAG_READ => handle_read(disk, sender, msg),
        TAG_CLOSE => handle_close(sender, msg),
        TAG_STAT => handle_stat(sender, msg),
        TAG_WRITE => deferred(|| handle_write(disk, sender, msg)),
        TAG_MKDIR => transacted(|| handle_mkdir(disk, sender, msg)),
        TAG_MKNOD => transacted(|| handle_mknod(sender, msg)),
        TAG_UNLINK | TAG_RMDIR | TAG_RENAME | TAG_LINK | TAG_SYMLINK => {
            transacted(|| handle_namespace(disk, sender, msg))
        }
        TAG_READLINK => handle_readlink(disk, sender, msg),
        TAG_CHDIR | TAG_FCHDIR => handle_chdir(disk, sender, msg),
        TAG_GETCWD => handle_getcwd(sender),
        TAG_GIVE_CWD => handle_give_cwd(sender, msg),
        TAG_LOCK => handle_lock(sender, msg),
        TAG_MAP if unsafe { FS_TYPE } == FsType::Ext2 => pager::handle_map(sender, msg),
        TAG_MAP => error_reply(sender, ERR_NOT_SUPPORTED),
        // From the kernel alone: nobody else can set the pager bit.
        quark_rt::ipc::TAG_PAGE_IN if sender & quark_rt::ipc::PAGER_BIT != 0 => {
            pager::page_in(sender, msg)
        }
        quark_rt::ipc::TAG_OBJECT_SYNC if sender & quark_rt::ipc::PAGER_BIT != 0 => {
            transacted(|| pager::sync(sender, msg))
        }
        quark_rt::ipc::TAG_OBJECT_IDLE if sender == 0 => {
            transacted(|| pager::idle(msg.data[0] as u32, msg.data[1]))
        }
        TAG_TRUNCATE => transacted(|| handle_truncate(disk, sender, msg)),
        TAG_STATFS => handle_statfs(sender),
        // Everything said to have been written is on the disk when this is
        // answered: here, and in every filesystem mounted here.
        TAG_SYNC => {
            commit_pending();
            mounts::sync();
            reply_opened(sender, [0; 6]);
        }
        TAG_READDIR_BULK => handle_readdir_bulk(disk, sender, msg),
        quark_rt::ipc::TAG_PING => {
            // Liveness probe: reply immediately, touching no disk state.
            let reply = Message {
                sender: 0,
                tag: quark_rt::ipc::TAG_PING,
                data: [0; 6],
            };
            let _ = syscall::sys_reply(sender, &reply);
        }
        _ => error_reply(sender, 0xFF),
    }
}

/// What the request being answered transferred: the bytes of a read or a
/// write, or the entry a listing goes on from. Left by the reply, for whoever
/// moves a descriptor past it.
static mut MOVED: Option<u64> = None;

/// Answer a read or a write with how much was transferred.
pub fn reply_count(sender: usize, n: u64) {
    unsafe { MOVED = Some(n) };
    reply_opened(sender, [n, 0, 0, 0, 0, 0]);
}

/// How long the file behind a handle is.
fn size_of(file: &OpenFile) -> Result<u64, u64> {
    match file.fs {
        FsFileData::Ext2 { inode_num } => {
            ext2::read_inode(ext2_state(), inode_num).map(|inode| inode.size64())
        }
        FsFileData::Fat32 { .. } => Ok(file.file_size as u64),
        FsFileData::Disk(ref disk) => Ok(devices::size_of(disk)),
        FsFileData::Remote(ref remote) => mounts::size_of(remote),
        _ => Ok(0),
    }
}

/// A read or a write through a descriptor's handle.
///
/// It is checked against what the descriptor was opened to do, and an offset
/// of `AT_POSITION` is wherever the descriptor is — or, writing to one opened
/// to append, the end of the file — and moves it. The position is here and
/// not in the client because every descriptor made from the first shares it:
/// a child writing after its parent through what it inherited writes *after*
/// it, which is the whole of how a shell's `{ a; b; } > file` works.
fn descriptor_io(disk: &DiskState, sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let writing = msg.tag == TAG_WRITE;
    let Some(file) = get_handle(handle, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    if (writing && !file.may_write) || (!writing && !file.may_read) {
        return error_reply(sender, ERR_INVALID_HANDLE);
    }
    // A directory is listed, not read: its blocks are not the caller's to see.
    if file.is_dir {
        return error_reply(sender, ERR_IS_DIR);
    }
    let moves = msg.data[2] == AT_POSITION;
    let at = if !moves {
        msg.data[2]
    } else if writing && file.append {
        match size_of(file) {
            Ok(end) => end,
            Err(code) => return error_reply(sender, code),
        }
    } else {
        file.pos
    };
    // A file here is at most four gigabytes: past that there is nothing to
    // read and nowhere to write. A disk is not a file, and is as long as it
    // is.
    let is_disk = matches!(file.fs, FsFileData::Disk(_));
    if at > u32::MAX as u64 && !is_disk {
        return if writing { error_reply(sender, ERR_NO_SPACE) } else { reply_count(sender, 0) };
    }
    let mut placed = *msg;
    placed.data[2] = at;
    unsafe { MOVED = None };
    dispatch(disk, sender, &placed);
    if moves {
        if let (Some(n), Some(file)) = (unsafe { MOVED.take() }, handles::descriptor(handle)) {
            file.pos = at + n;
        }
    }
}

/// READDIR_BULK from wherever a directory descriptor has got to.
fn descriptor_list(disk: &DiskState, sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let Some(file) = get_handle(handle, sender).filter(|f| f.by_fd) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let mut from = *msg;
    from.data[1] = file.dirpos;
    unsafe { MOVED = None };
    dispatch(disk, sender, &from);
    if let (Some(next), Some(file)) = (unsafe { MOVED.take() }, handles::descriptor(handle)) {
        file.dirpos = next;
    }
}

/// TAG_SEEK: `[handle, offset, whence]`, the offset signed. Reply: where the
/// descriptor now is, and what it was opened to do (bit 0 read, 1 write,
/// 2 append) — which a client that has just been exec'd into holding it has no
/// other way to learn. A directory can be sent to the start, or to an entry a
/// listing named.
fn handle_seek(sender: usize, msg: &Message) {
    let Some(file) = get_handle(msg.data[0] as usize, sender).filter(|f| f.by_fd) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let how = file.may_read as u64 | (file.may_write as u64) << 1 | (file.append as u64) << 2;
    let offset = msg.data[1] as i64;
    if file.is_dir {
        return match (msg.data[2], offset) {
            (SEEK_SET, to) if to >= 0 => {
                file.dirpos = to as u64;
                reply_opened(sender, [file.dirpos, how, 0, 0, 0, 0])
            }
            (SEEK_CUR, 0) => reply_opened(sender, [file.dirpos, how, 0, 0, 0, 0]),
            _ => error_reply(sender, ERR_INVALID_PATH),
        };
    }
    let from = match msg.data[2] {
        SEEK_SET => 0,
        SEEK_CUR => file.pos as i64,
        SEEK_END => match size_of(file) {
            Ok(end) => end as i64,
            Err(code) => return error_reply(sender, code),
        },
        _ => return error_reply(sender, ERR_INVALID_PATH),
    };
    match from.checked_add(offset) {
        Some(to) if to >= 0 => {
            file.pos = to as u64;
            reply_opened(sender, [file.pos, how, 0, 0, 0, 0])
        }
        _ => error_reply(sender, ERR_INVALID_PATH),
    }
}

/// The kernel says no descriptor names `handle` any more: it is closed.
fn descriptor_closed(handle: usize) {
    let Some(ino) = handles::release(handle) else {
        return;
    };
    // Its own locks went with it, and whoever was waiting for one through it
    // is not going to get it.
    while let Some(w) = locks::drop_handle(handle) {
        error_reply(w.sender, ERR_INVALID_HANDLE);
    }
    grant_waiters();
    settle(&[ino]);
}

/// TAG_SETATTR: `[path_len, which, nofollow]`, with the path lent and, after
/// it, five words: `[mode, uid, gid, atime, mtime]`. `which` says which of
/// them to use. A `path_len` of 0 means the open file `data[5]` names (a
/// handle plus one) rather than a path from it.
fn handle_setattr(sender: usize, msg: &Message) {
    if unsafe { FS_TYPE } != FsType::Ext2 {
        return error_reply(sender, ERR_NOT_SUPPORTED);
    }
    if ext2_state().read_only {
        return error_reply(sender, ERR_READ_ONLY);
    }
    let len = msg.data[0] as usize;
    let (uid, gid) = get_sender_uid_gid(sender);
    let ino = if len == 0 {
        match get_handle(msg.data[5].wrapping_sub(1) as usize, sender) {
            Some(file) => match file.fs {
                FsFileData::Ext2 { inode_num } => Ok(inode_num),
                // The devices are the server's, and are what they are.
                FsFileData::Device(_) | FsFileData::Disk(_) | FsFileData::DevDir => Err(ERR_PERMISSION),
                _ => Err(ERR_NOT_SUPPORTED),
            },
            None => Err(ERR_INVALID_HANDLE),
        }
    } else {
        protocol::lent_path(sender, 0, len, 0).and_then(|path| {
            let base = base_of(sender, msg.data[5])?;
            if ext2_dir::dev_dir() == 0 && devices::refuses(ext2_whole_path(base, path)?) {
                return Err(ERR_PERMISSION);
            }
            match ext2_dir::resolve(ext2_state(), base, path, uid, gid, msg.data[2] == 0)? {
                ext2_dir::Found::Inode(ino, _, _) if ino != ext2_dir::dev_dir() => Ok(ino),
                _ => Err(ERR_PERMISSION),
            }
        })
    };
    let mut raw = [0u8; ATTR_LEN];
    if syscall::sys_lent_read(sender, len, &mut raw) != Ok(ATTR_LEN) {
        return error_reply(sender, ERR_INVALID_PATH);
    }
    let word = |i: usize| u64::from_le_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
    let attrs = ext2_ops::Attrs {
        which: msg.data[1],
        mode: word(0),
        uid: word(1),
        gid: word(2),
        atime: word(3),
        mtime: word(4),
    };
    match ino.and_then(|ino| ext2_ops::setattr(ext2_state_mut(), ino, &attrs, uid, gid)) {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

// ---------------------------------------------------------------------------
// Request handlers
// ---------------------------------------------------------------------------

/// TAG_OPEN: data[0] = path length, data[1] = flags; the path is lent.
/// Reply: [handle, size, is_dir, mode with its type bits, access, inode id].
fn handle_open(disk: &DiskState, sender: usize, msg: &Message) {
    let flags = msg.data[1];
    let path = match protocol::lent_path(sender, 0, msg.data[0] as usize, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    if unsafe { FS_TYPE } == FsType::Ext2 {
        let base = match base_of(sender, msg.data[5]) {
            Ok(b) => b,
            Err(code) => return error_reply(sender, code),
        };
        // The lookup itself finds /dev, through links and all, when the root
        // has one. Without it, the path is matched as written.
        if ext2_dir::dev_dir() == 0 {
            let whole = match ext2_whole_path(base, path) {
                Ok(p) => p,
                Err(code) => return error_reply(sender, code),
            };
            match devices::lookup(whole) {
                devices::Lookup::Elsewhere => {}
                found => return devices::open(sender, whole, found, flags),
            }
        }
        open_ext2(sender, base, path, flags, given_mode(msg.data[2]));
    } else {
        let path = match fat_path(disk, sender, msg.data[5], path) {
            Ok(p) => p,
            Err(code) => return error_reply(sender, code),
        };
        match devices::lookup(path) {
            devices::Lookup::Elsewhere => {}
            found => return devices::open(sender, path, found, flags),
        }
        open_fat32(disk, sender, path, flags);
    }
}

/// The ext2 directory a relative path starts from. `word` is 0 for the
/// program's working directory, or one more than an open directory handle.
///
/// A directory in a mounted filesystem is not one a path can be walked from
/// here, and a path that starts from one has gone to that filesystem's
/// server before any handler asks. What is left is a path that turns
/// straight back out of the mount — which starts from the directory the
/// mount is on — and an absolute one, which starts from nowhere.
pub(crate) fn base_of(sender: usize, word: u64) -> Result<u32, u64> {
    if word == 0 {
        if let Some(remote) = mounts::cwd_remote(sender) {
            return Ok(mounts::covered(&remote));
        }
        return Ok(local_cwd(sender));
    }
    let file = get_handle((word - 1) as usize, sender).ok_or(ERR_INVALID_HANDLE)?;
    match file.fs {
        FsFileData::Ext2 { inode_num } if file.is_dir => Ok(inode_num),
        FsFileData::DevDir if ext2_dir::dev_dir() != 0 => Ok(ext2_dir::dev_dir()),
        FsFileData::Remote(ref remote) if file.is_dir => Ok(mounts::covered(remote)),
        _ => Err(ERR_NOT_DIR),
    }
}

/// The directory of this filesystem `sender` is in: the root, if it is in
/// none.
pub(crate) fn local_cwd(sender: usize) -> u32 {
    match cwd_of(sender).0 {
        cwd::Where::Inode(ino) => ino,
        _ => ext2::EXT2_ROOT_INO,
    }
}

/// `path` from `base` written out whole, for a root with no /dev to find.
fn ext2_whole_path(base: u32, path: &'static [u8]) -> Result<&'static [u8], u64> {
    if path.first() == Some(&b'/') {
        return Ok(path);
    }
    let dir = ext2_dir::path_of(ext2_state(), base)?;
    // path_of's buffer is overwritten by nothing join does.
    cwd::join(dir, path)
}

/// A FAT32 path made absolute: FAT32 keeps a program's directory as a path.
/// One that starts from an open directory starts from where that directory
/// is found to be.
fn fat_path(disk: &DiskState, sender: usize, word: u64, path: &[u8]) -> Result<&'static [u8], u64> {
    if word != 0 && path.first() != Some(&b'/') {
        let file = get_handle((word - 1) as usize, sender).ok_or(ERR_INVALID_HANDLE)?;
        let cluster = match file.fs {
            FsFileData::Fat32 { first_cluster, .. } if file.is_dir => first_cluster,
            _ => return Err(ERR_NOT_DIR),
        };
        return cwd::join(fat_dir_path(disk, cluster)?, path);
    }
    let (_, dir) = cwd::get(space_of(sender));
    cwd::join(dir, path)
}

/// The path of a FAT32 directory, found the only way FAT32 allows: every
/// directory but the root says where its parent is, and the parent is
/// searched for the entry that leads back.
pub(crate) fn fat_dir_path(disk: &DiskState, cluster: u32) -> Result<&'static [u8], u64> {
    static mut OUT: [u8; MAX_PATH + 1] = [0; MAX_PATH + 1];
    let out = unsafe { &mut *core::ptr::addr_of_mut!(OUT) };
    let root = disk.bpb.root_cluster;
    let mut start = out.len();
    let mut cur = cluster;
    // As deep as a path can be.
    for _ in 0..MAX_PATH / 2 {
        if cur == root || cur == 0 {
            if start == out.len() {
                start -= 1;
                out[start] = b'/';
            }
            return Ok(&out[start..]);
        }
        let parent = match find_entry(disk, cur, b"..         ")? {
            // The root is written as no cluster at all.
            Some((0, _, true)) => root,
            Some((parent, _, true)) => parent,
            _ => return Err(ERR_NOT_FOUND),
        };
        let mut name = [0u8; 12];
        let mut len = 0;
        for index in 0.. {
            match read_dir_entry(disk, parent, index)? {
                Some((entry, raw, _, true, _)) if entry == cur && raw[0] != b'.' => {
                    len = fat_display_name(&raw, &mut name);
                    break;
                }
                Some(_) => {}
                None => return Err(ERR_NOT_FOUND),
            }
        }
        if len + 1 > start {
            return Err(ERR_NAME_TOO_LONG);
        }
        start -= len;
        out[start..start + len].copy_from_slice(&name[..len]);
        start -= 1;
        out[start] = b'/';
        cur = parent;
    }
    Err(ERR_LOOP)
}

/// The permission bits a word carries for something being made, if it
/// carries any.
fn given_mode(word: u64) -> Option<u16> {
    (word & MODE_GIVEN != 0).then_some((word & 0o7777) as u16)
}

pub(crate) fn reply_opened(sender: usize, words: [u64; 6]) {
    let reply = Message { sender: 0, tag: TAG_OK, data: words };
    let _ = syscall::sys_reply(sender, &reply);
}

fn open_ext2(sender: usize, base: u32, path: &[u8], flags: u64, mode: Option<u16>) {
    let (uid, gid) = get_sender_uid_gid(sender);
    let trailing = path.len() > 1 && path[path.len() - 1] == b'/';
    let wants_dir = flags & OPEN_DIRECTORY != 0 || trailing;
    let follow = flags & OPEN_NOFOLLOW == 0;
    let found = ext2_dir::resolve(ext2_state(), base, path, uid, gid, follow);
    let found = match found {
        Ok(ext2_dir::Found::Device(dev)) => {
            return devices::open(sender, path, devices::Lookup::Device(dev), flags);
        }
        Ok(ext2_dir::Found::Inode(ino, _, _)) if ino == ext2_dir::dev_dir() => {
            return devices::open(sender, path, devices::Lookup::Dir, flags);
        }
        Ok(ext2_dir::Found::Inode(ino, inode, holder)) => Ok((ino, inode, holder)),
        Err(code) => Err(code),
    };
    let (ino, inode) = match found {
        Ok((ino, inode, _)) => {
            if flags & OPEN_CREATE != 0 && flags & OPEN_EXCLUSIVE != 0 {
                return error_reply(sender, ERR_EXISTS);
            }
            (ino, inode)
        }
        Err(ERR_NOT_FOUND) if flags & OPEN_CREATE != 0 => {
            if trailing {
                return error_reply(sender, ERR_IS_DIR);
            }
            if ext2_state().read_only {
                return error_reply(sender, ERR_READ_ONLY);
            }
            match ext2_ops::create(ext2_state_mut(), base, path, uid, gid, false, mode) {
                Ok(made) => made,
                Err(code) => return error_reply(sender, code),
            }
        }
        Err(code) => return error_reply(sender, code),
    };
    if wants_dir && !inode.is_dir() {
        return error_reply(sender, ERR_NOT_DIR);
    }
    // Only OPEN_NOFOLLOW gets this far with a link. And a file opened only
    // to be asked about is, from here on, treated as a link is: nothing of
    // its mode is asked for, and the handle reads and writes nothing.
    let link = inode.is_symlink() || protocol::asks(flags);
    if flags & (OPEN_DESCRIPTOR | OPEN_PROXIED) != 0 {
        // A descriptor says what it is for, and is refused here if the file
        // does not allow it — not at the first write, a long way from the
        // open that should have failed. So does the server this filesystem
        // is mounted in, for the descriptor it is about to make.
        if !link && flags & OPEN_READ != 0 && !ext2::check_permission(&inode, uid, gid, 4) {
            return error_reply(sender, ERR_PERMISSION);
        }
        if flags & OPEN_WRITE != 0 {
            if inode.is_dir() {
                return error_reply(sender, ERR_IS_DIR);
            }
            // What is written to a named pipe is not written to the disk.
            if ext2_state().read_only && !inode.is_fifo() {
                return error_reply(sender, ERR_READ_ONLY);
            }
            if link || !ext2::check_permission(&inode, uid, gid, 2) {
                return error_reply(sender, ERR_PERMISSION);
            }
        }
    } else if !link && !ext2::check_permission(&inode, uid, gid, 4) {
        return error_reply(sender, ERR_PERMISSION);
    }
    // A named pipe, opened to read or to write: an end of the pipe the
    // kernel keeps for this inode while anybody has it open. The permissions
    // were checked above, as for a file; what is handed over is not a file,
    // and this server sees no more of it — not the bytes, and not the close.
    //
    // Opened for neither (to be asked about, or as the start of a path) it
    // is an inode like any other, and falls through.
    if inode.is_fifo() && flags & OPEN_DESCRIPTOR != 0 && flags & (OPEN_READ | OPEN_WRITE) != 0 {
        // One end to an open. Linux lets a named pipe be opened for both,
        // which is two ends in one descriptor, and a descriptor here is one.
        if flags & OPEN_READ != 0 && flags & OPEN_WRITE != 0 {
            return error_reply(sender, ERR_NOT_SUPPORTED);
        }
        let write = flags & OPEN_WRITE != 0;
        // A writer that will not wait is not given an end nobody is reading:
        // for a moment there would have been a writer, and a reader waiting
        // for one would have gone on to read the end of nothing.
        let only_with_peer = write && flags & OPEN_NOWAIT != 0;
        return match syscall::sys_fd_serve_pipe(sender, ino as u64, write, only_with_peer) {
            // The second word is what the opener should wait on, where a
            // file's size would be.
            syscall::PipeEnd::Given(fd, wait) => reply_opened(sender, [
                fd as u64,
                wait,
                0,
                inode.i_mode as u64,
                access_bits(&inode, uid, gid),
                ino as u64,
            ]),
            syscall::PipeEnd::NoPeer => error_reply(sender, ERR_NO_PEER),
            syscall::PipeEnd::Failed => error_reply(sender, ERR_TOO_MANY_OPEN),
        };
    }
    let writable =
        !link && !ext2_state().read_only && ext2::check_permission(&inode, uid, gid, 2);
    let mut size = inode.size64();
    if flags & OPEN_TRUNCATE != 0 && inode.is_regular() {
        if !writable {
            return error_reply(sender, ERR_PERMISSION);
        }
        if let Err(code) = ext2_ops::truncate(ext2_state_mut(), ino, 0) {
            return error_reply(sender, code);
        }
        pager::resized(ino, 0);
        size = 0;
    }
    let file = OpenFile {
        in_use: true,
        owner: space_of(sender),
        is_dir: inode.is_dir(),
        writable,
        link,
        fs: FsFileData::Ext2 { inode_num: ino },
        ..OpenFile::empty()
    };
    opened(sender, flags, file, [
        0,
        size,
        inode.is_dir() as u64,
        inode.i_mode as u64,
        access_bits(&inode, uid, gid),
        ino as u64,
    ]);
}

/// Whether `name` is a FAT short name: at most eight characters, a dot and
/// three more. Anything longer would be squeezed into one by `to_fat83` and
/// name a different file.
fn fits_fat83(name: &[u8]) -> bool {
    let (base, ext) = match name.iter().position(|&b| b == b'.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, &[][..]),
    };
    !base.is_empty() && base.len() <= 8 && ext.len() <= 3 && !ext.contains(&b'.')
}

/// Split a FAT32 path into its parent directory's cluster and the new name.
fn fat32_parent(disk: &DiskState, path: &[u8]) -> Result<(u32, [u8; 11]), u64> {
    let (parent, name) = ext2_ops::split_path(path)?;
    if !fits_fat83(name) {
        return Err(ERR_NAME_TOO_LONG);
    }
    let (cluster, _, is_dir, _, _) = resolve_path(disk, parent)?;
    if !is_dir {
        return Err(ERR_NOT_DIR);
    }
    let mut fat_name = [0u8; 11];
    to_fat83(name, &mut fat_name);
    Ok((cluster, fat_name))
}

/// Make a FAT32 file `size` bytes long, which is no longer than it is: the
/// clusters past the new end are given back, and an empty file keeps none.
fn fat32_truncate(disk: &DiskState, file: &mut OpenFile, size: u32) -> Result<(), u64> {
    if file.is_dir {
        return Err(ERR_IS_DIR);
    }
    let (dir_cluster, fat_name) = match &file.fs {
        FsFileData::Fat32 { dir_cluster, fat_name, .. } => (*dir_cluster, *fat_name),
        _ => return Err(ERR_INVALID_HANDLE),
    };
    // As the directory has it now, whatever this handle last knew.
    let (first, current) = match find_entry(disk, dir_cluster, &fat_name)? {
        Some((cluster, len, false)) => (cluster, len),
        _ => return Err(ERR_NOT_FOUND),
    };
    // Longer would be clusters of zeroes to write, and nothing here asks.
    if size > current {
        return Err(ERR_NOT_SUPPORTED);
    }
    let cluster_bytes = disk.bpb.sectors_per_cluster * disk.bpb.bytes_per_sector;
    let keep = size.div_ceil(cluster_bytes);
    let mut start = first;
    if size < current {
        if keep == 0 {
            // The entry first: a file that claims clusters the table says are
            // free is worse than clusters nothing claims.
            update_dir_entry(disk, dir_cluster, &fat_name, Change::Start(0, 0))?;
            if first != 0 {
                disk.fat_free_chain(first).map_err(|_| ERR_IO)?;
            }
            start = 0;
        } else {
            let mut last = first;
            for _ in 1..keep {
                last = disk.fat_next(last).ok_or(ERR_IO)?;
            }
            update_dir_entry(disk, dir_cluster, &fat_name, Change::Size(size))?;
            if let Some(rest) = disk.fat_next(last) {
                disk.fat_set(last, 0x0FFF_FFFF).map_err(|_| ERR_IO)?;
                disk.fat_free_chain(rest).map_err(|_| ERR_IO)?;
            }
        }
    }
    file.file_size = size;
    if let FsFileData::Fat32 { first_cluster, cur_cluster, cur_cluster_offset, .. } = &mut file.fs {
        *first_cluster = start;
        *cur_cluster = start;
        *cur_cluster_offset = 0;
    }
    Ok(())
}

/// Whether a FAT32 directory holds nothing but `.` and `..`.
fn fat32_dir_empty(disk: &DiskState, cluster: u32) -> Result<bool, u64> {
    for index in 0.. {
        match read_dir_entry(disk, cluster, index)? {
            Some((_, name, _, _, _)) if name[0] == b'.' => {}
            Some(_) => return Ok(false),
            None => break,
        }
    }
    Ok(true)
}

/// Remove a FAT32 file's name, or an empty directory's, and give back what
/// it held. FAT has one name to a file, so the file goes with it — which is
/// why one that is open is refused: there is nowhere for it to go on being.
fn fat32_remove(disk: &DiskState, path: &[u8], dir: bool) -> Result<(), u64> {
    let (cluster, _, is_dir, parent, name) = resolve_path(disk, path)?;
    if parent == 0 {
        // The root has no name to remove.
        return Err(ERR_BUSY);
    }
    match (dir, is_dir) {
        (true, false) => return Err(ERR_NOT_DIR),
        (false, true) => return Err(ERR_IS_DIR),
        _ => {}
    }
    if is_dir && !fat32_dir_empty(disk, cluster)? {
        return Err(ERR_NOT_EMPTY);
    }
    if handles::fat_is_open(parent, &name) {
        return Err(ERR_BUSY);
    }
    update_dir_entry(disk, parent, &name, Change::Remove)?;
    if cluster != 0 {
        disk.fat_free_chain(cluster).map_err(|_| ERR_IO)?;
    }
    Ok(())
}

fn open_fat32(disk: &DiskState, sender: usize, path: &[u8], flags: u64) {
    let trailing = path.len() > 1 && path[path.len() - 1] == b'/';
    let wants_dir = flags & OPEN_DIRECTORY != 0 || trailing;
    let found = match resolve_path(disk, path) {
        Ok(found) => {
            if flags & OPEN_CREATE != 0 && flags & OPEN_EXCLUSIVE != 0 {
                return error_reply(sender, ERR_EXISTS);
            }
            found
        }
        Err(ERR_NOT_FOUND) if flags & OPEN_CREATE != 0 && !trailing => {
            let (parent, fat_name) = match fat32_parent(disk, path) {
                Ok(p) => p,
                Err(code) => return error_reply(sender, code),
            };
            match create_dir_entry(disk, parent, &fat_name, false) {
                Ok(cluster) => (cluster, 0, false, parent, fat_name),
                Err(code) => return error_reply(sender, code),
            }
        }
        Err(code) => return error_reply(sender, code),
    };
    let (cluster, size, is_dir, dir_cluster, fat_name) = found;
    if wants_dir && !is_dir {
        return error_reply(sender, ERR_NOT_DIR);
    }
    let mode = if is_dir { FAT_DIR_MODE } else { FAT_FILE_MODE };
    let mut file = fat32_file(sender, cluster, size, is_dir, dir_cluster, &fat_name);
    let mut size = size;
    if flags & OPEN_TRUNCATE != 0 && !is_dir {
        if let Err(code) = fat32_truncate(disk, &mut file, 0) {
            return error_reply(sender, code);
        }
        size = 0;
    }
    // A file's id is its first cluster, and an empty one has none: the
    // cluster its name is in, and where in it, would do, but nothing here
    // needs an empty file told from another.
    opened(sender, flags, file, [0, size as u64, is_dir as u64, mode, FAT_ACCESS, cluster as u64]);
}

/// TAG_MKDIR: data[0] = path length; the path is lent.
fn handle_mkdir(disk: &DiskState, sender: usize, msg: &Message) {
    let path = match protocol::lent_path(sender, 0, msg.data[0] as usize, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    let made = if unsafe { FS_TYPE } == FsType::Ext2 {
        if ext2_state().read_only {
            Err(ERR_READ_ONLY)
        } else if ext2_dir::dev_dir() == 0 && devices::refuses(path) {
            Err(ERR_PERMISSION)
        } else {
            let (uid, gid) = get_sender_uid_gid(sender);
            base_of(sender, msg.data[5]).and_then(|base| {
                ext2_ops::create(ext2_state_mut(), base, path, uid, gid, true, given_mode(msg.data[1]))
                    .map(|_| ())
            })
        }
    } else {
        match fat_path(disk, sender, msg.data[5], path) {
            Ok(path) if devices::refuses(path) => Err(ERR_PERMISSION),
            Ok(path) => fat32_mkdir(disk, path),
            Err(code) => Err(code),
        }
    };
    match made {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

/// Make a named pipe. A device is not a file anybody can make — the ones
/// there are, are this server's own — and a regular file is made by opening
/// it.
fn handle_mknod(sender: usize, msg: &Message) {
    let path = match protocol::lent_path(sender, 0, msg.data[0] as usize, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    let mode = msg.data[1] as u16;
    let made = if unsafe { FS_TYPE } != FsType::Ext2 {
        // FAT has files and directories and nothing else.
        Err(ERR_NOT_SUPPORTED)
    } else if mode & ext2::S_IFMT != ext2::S_IFIFO {
        Err(ERR_PERMISSION)
    } else if ext2_state().read_only {
        Err(ERR_READ_ONLY)
    } else if ext2_dir::dev_dir() == 0 && devices::refuses(path) {
        Err(ERR_PERMISSION)
    } else {
        let (uid, gid) = get_sender_uid_gid(sender);
        base_of(sender, msg.data[5]).and_then(|base| {
            ext2_ops::make_fifo(ext2_state_mut(), base, path, uid, gid, mode)
        })
    };
    match made {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

fn fat32_mkdir(disk: &DiskState, path: &[u8]) -> Result<(), u64> {
    {
        match resolve_path(disk, path) {
            Ok(_) => Err(ERR_EXISTS),
            Err(ERR_NOT_FOUND) => fat32_parent(disk, path)
                .and_then(|(parent, name)| create_dir_entry(disk, parent, &name, true))
                .map(|_| ()),
            Err(code) => Err(code),
        }
    }
}
/// Copy the first `n` bytes of `CLIENT_BUF` into what `sender` lent.
pub fn lend_out(sender: usize, n: usize) -> bool {
    let data = unsafe { core::slice::from_raw_parts(CLIENT_BUF as *const u8, n) };
    n == 0 || syscall::sys_lent_write(sender, 0, data) == Ok(n)
}

/// Copy `n` bytes of what `sender` lent into `CLIENT_BUF`.
pub fn lend_in(sender: usize, n: usize) -> bool {
    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, n) };
    n == 0 || syscall::sys_lent_read(sender, 0, buf) == Ok(n)
}

/// Reply to a read: the bytes go into what the caller lent, and the count into
/// the reply. A caller that lent too little gets an error, not a short read.
fn reply_read(sender: usize, result: Result<u32, u64>) {
    match result {
        Ok(n) if lend_out(sender, n as usize) => reply_count(sender, n as u64),
        Ok(_) => error_reply(sender, ERR_IO),
        Err(code) => error_reply(sender, code),
    }
}

/// TAG_READ: data[0]=handle, data[2]=offset, data[3]=max_bytes (at most a
/// page), with a buffer that long lent for writing.
/// Reply: tag=TAG_OK, data[0]=bytes_read  OR  tag=TAG_ERROR, data[0]=error_code
fn handle_read(disk: &DiskState, sender: usize, msg: &Message) {
    if unsafe { FS_TYPE } == FsType::Ext2 {
        handle_read_ext2(sender, msg);
        return;
    }

    let handle = msg.data[0] as usize;
    let offset = msg.data[2] as u32;
    let max_bytes = msg.data[3] as u32;

    match get_handle(handle, sender) {
        Some(file) => reply_read(sender, read_file_data(disk, file, offset, max_bytes)),
        None => error_reply(sender, ERR_INVALID_HANDLE),
    }
}

/// TAG_CLOSE: data[0]=handle
/// Reply: tag=TAG_OK  OR  tag=TAG_ERROR
fn handle_close(sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let space = space_of(sender);
    // Closing any handle on a file drops every lock the program holds on it,
    // as POSIX has it; the handle's own locks go with the handle.
    let key = get_handle(handle, sender).and_then(|f| handles::lock_key(f));
    match handles::close(handle, space) {
        Some(ino) => {
            reply_opened(sender, [0; 6]);
            // Another thread may be waiting for a lock through this handle.
            while let Some(w) = locks::drop_handle(handle) {
                error_reply(w.sender, ERR_INVALID_HANDLE);
            }
            if let Some(key) = key {
                locks::release(locks::Owner::Program(space), Some(key));
            }
            grant_waiters();
            settle(&[ino]);
        }
        None => error_reply(sender, ERR_INVALID_HANDLE),
    }
}

/// TAG_LOCK: `[handle, kind, start, len, flags]`. `kind` is 0 to unlock, 1
/// shared, 2 exclusive; `len` 0 runs to the end of the file and beyond. With
/// `LOCK_QUERY` the reply is `[kind, start, len, holder]` of the first lock in
/// the way (`kind` 0 if none, `holder` the program, or all ones for a
/// handle's). With `LOCK_WAIT` a lock that cannot be granted yet is answered
/// when it can be.
fn handle_lock(sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let [_, kind, start, len, flags, _] = msg.data;
    let space = space_of(sender);
    let Some(file) = get_handle(handle, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let Some(inode) = handles::lock_key(file) else {
        return error_reply(sender, ERR_NOT_SUPPORTED);
    };
    if kind > 2 || flags & !(LOCK_WAIT | LOCK_OFD | LOCK_QUERY) != 0 {
        return error_reply(sender, ERR_INVALID_PATH);
    }
    let end = if len == 0 {
        u64::MAX
    } else {
        match start.checked_add(len) {
            Some(end) => end,
            None => return error_reply(sender, ERR_INVALID_PATH),
        }
    };
    let owner = if flags & LOCK_OFD != 0 { locks::Owner::Handle(handle) } else { locks::Owner::Program(space) };
    let want = locks::Range { inode, owner, start, end, exclusive: kind == 2 };

    if flags & LOCK_QUERY != 0 {
        if kind == 0 {
            return error_reply(sender, ERR_INVALID_PATH);
        }
        let answer = match locks::conflict(&want) {
            Some(held) => [
                if held.exclusive { 2 } else { 1 },
                held.start,
                if held.end == u64::MAX { 0 } else { held.end - held.start },
                match held.owner {
                    locks::Owner::Program(s) => s,
                    locks::Owner::Handle(_) => u64::MAX,
                },
                0,
                0,
            ],
            None => [0; 6],
        };
        return reply_opened(sender, answer);
    }

    if kind == 0 {
        match locks::apply(&want, true) {
            Ok(()) => reply_opened(sender, [0; 6]),
            Err(code) => error_reply(sender, code),
        }
        return grant_waiters();
    }
    // A program's locks go when the program does, and a program that holds
    // only descriptors is not otherwise being watched. If the kernel will
    // not say when it goes, it has gone already — ended while this request
    // waited its turn — and a lock kept for it would be kept for good.
    if matches!(owner, locks::Owner::Program(_)) && syscall::sys_space_watch(space).is_err() {
        return error_reply(sender, ERR_TOO_MANY_OPEN);
    }
    match locks::conflict(&want) {
        None => match locks::apply(&want, false) {
            // An exclusive lock made shared may let a waiter in.
            Ok(()) => {
                reply_opened(sender, [0; 6]);
                grant_waiters();
            }
            Err(code) => error_reply(sender, code),
        },
        Some(_) if flags & LOCK_WAIT != 0 => {
            match locks::wait(locks::Waiter { sender, space, want }) {
                // No answer until it is granted. If the task waiting goes
                // first, the server is told, and forgets the request — and
                // if it has gone already, forgets it now.
                Ok(()) => {
                    if syscall::sys_task_watch(sender).is_err() {
                        locks::drop_task(sender);
                    }
                }
                Err(code) => error_reply(sender, code),
            }
        }
        Some(_) => error_reply(sender, ERR_WOULD_BLOCK),
    }
}

/// Answer every waiting lock request that can now be granted.
fn grant_waiters() {
    while let Some(w) = locks::grantable() {
        match locks::apply(&w.want, false) {
            Ok(()) => reply_opened(w.sender, [0; 6]),
            Err(code) => error_reply(w.sender, code),
        }
    }
}

/// A program has gone: its handles go with it.
fn client_died(space: u64) {
    let mut closed = [0u32; handles::MAX_OPEN_FILES];
    let n = handles::close_all(space, &mut closed);
    locks::drop_space(space);
    grant_waiters();
    settle(&closed[..n]);
    if let cwd::Where::Inode(ino) = cwd::forget(space) {
        settle(&[ino]);
    }
}

/// Free what a machine stopped while files were removed but in use left on
/// the orphan list. Before the first request: nothing can hold them now.
fn recover_orphans() {
    let mut freed = 0usize;
    let mut failed = false;
    // Bounded by the list's own bound; each pop is its own transaction.
    for _ in 0..ext2_state().total_inodes {
        let mut more = false;
        transacted(|| match ext2_ops::recover_orphan(ext2_state_mut()) {
            Ok(m) => more = m,
            Err(code) => {
                println!("[vfs] could not free an orphaned inode ({})", code);
                failed = true;
            }
        });
        if failed || !more {
            break;
        }
        freed += 1;
    }
    if freed > 0 {
        println!(
            "[vfs] freed {} orphaned inode{}",
            freed,
            if freed == 1 { "" } else { "s" }
        );
    }
}

/// Free any of these inodes that lost their last name while open and have
/// now lost their last handle.
fn settle(inodes: &[u32]) {
    for &ino in inodes {
        if !handles::is_orphan(ino) || handles::inode_is_open(ino) {
            continue;
        }
        handles::forget_orphan(ino);
        transacted(|| {
            if let Err(code) = ext2_ops::release(ext2_state_mut(), ino) {
                println!("[vfs] could not free inode {} ({})", ino, code);
            }
        });
    }
}

/// TAG_UNLINK and TAG_RMDIR lend a path, `data[0]` long. TAG_RENAME and
/// TAG_LINK lend two, end to end, `data[0]` and `data[1]` long (LINK's
/// `data[2]` may ask to follow a link at the source); TAG_SYMLINK lends the
/// target, then the new path.
fn handle_namespace(disk: &DiskState, sender: usize, msg: &Message) {
    if unsafe { FS_TYPE } != FsType::Ext2 {
        // FAT32 has one name to a file and no links: a name can be removed,
        // and that is all.
        let done = match msg.tag {
            TAG_UNLINK | TAG_RMDIR => protocol::lent_path(sender, 0, msg.data[0] as usize, 0)
                .and_then(|path| fat_path(disk, sender, msg.data[5], path))
                .and_then(|path| match devices::refuses(path) {
                    true => Err(ERR_PERMISSION),
                    false => fat32_remove(disk, path, msg.tag == TAG_RMDIR),
                }),
            _ => Err(ERR_NOT_SUPPORTED),
        };
        return match done {
            Ok(()) => reply_opened(sender, [0; 6]),
            Err(code) => error_reply(sender, code),
        };
    }
    if ext2_state().read_only {
        return error_reply(sender, ERR_READ_ONLY);
    }
    let first = match protocol::lent_path(sender, 0, msg.data[0] as usize, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    // With a /dev on the disk, the lookup keeps everything out of it. A link's
    // target is only text, and may name a device.
    let lexical = ext2_dir::dev_dir() == 0;
    if lexical && msg.tag != TAG_SYMLINK && devices::refuses(first) {
        return error_reply(sender, ERR_PERMISSION);
    }
    let base = match base_of(sender, msg.data[5]) {
        Ok(b) => b,
        Err(code) => return error_reply(sender, code),
    };
    let (uid, gid) = get_sender_uid_gid(sender);
    let e2 = ext2_state_mut();
    let done = match msg.tag {
        TAG_UNLINK => ext2_ops::unlink(e2, base, first, uid, gid),
        TAG_RMDIR => ext2_ops::rmdir(e2, base, first, uid, gid),
        tag => match protocol::lent_path(sender, msg.data[0] as usize, msg.data[1] as usize, 4096) {
            Ok(second) if lexical && devices::refuses(second) => Err(ERR_PERMISSION),
            Ok(second) => match tag {
                // The second path's own base; SYMLINK resolves only that path,
                // and takes the ordinary one for it.
                TAG_SYMLINK => ext2_ops::symlink(e2, first, base, second, uid, gid),
                _ => match base_of(sender, msg.data[4]) {
                    Ok(to_base) if tag == TAG_LINK => {
                        let follow = msg.data[2] & LINK_FOLLOW != 0;
                        ext2_ops::link(e2, base, first, to_base, second, uid, gid, follow)
                    }
                    Ok(to_base) => ext2_ops::rename(e2, base, first, to_base, second, uid, gid),
                    Err(code) => Err(code),
                },
            },
            Err(code) => Err(code),
        },
    };
    match done {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

/// TAG_READLINK: `data[0]` = the path's length, `data[1]` = the room after
/// it. One buffer is lent for reading and writing: the path, then that room,
/// where the target is written. The reply is the target's whole length, which
/// may be more than there was room for.
fn handle_readlink(disk: &DiskState, sender: usize, msg: &Message) {
    let path_len = msg.data[0] as usize;
    let path = match protocol::lent_path(sender, 0, path_len, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    if unsafe { FS_TYPE } != FsType::Ext2 {
        // No links on FAT32: whatever is there is not one.
        return match fat_path(disk, sender, msg.data[5], path).and_then(|p| resolve_path(disk, p)) {
            Ok(_) => error_reply(sender, ERR_INVALID_PATH),
            Err(code) => error_reply(sender, code),
        };
    }
    let base = match base_of(sender, msg.data[5]) {
        Ok(b) => b,
        Err(code) => return error_reply(sender, code),
    };
    let (uid, gid) = get_sender_uid_gid(sender);
    let e2 = ext2_state();
    let inode = match ext2_dir::resolve(e2, base, path, uid, gid, false) {
        Ok(ext2_dir::Found::Inode(_, inode, _)) => inode,
        Ok(ext2_dir::Found::Device(_)) => return error_reply(sender, ERR_INVALID_PATH),
        Err(code) => return error_reply(sender, code),
    };
    let len = match ext2_ops::read_link(e2, &inode) {
        Ok(len) => len,
        Err(code) => return error_reply(sender, code),
    };
    let target = ext2_ops::link_target(len);
    // What does not fit is not written; the length says there was more.
    let room = (msg.data[1] as usize).min(len);
    if room > 0 && syscall::sys_lent_write(sender, path_len, &target[..room]) != Ok(room) {
        return error_reply(sender, ERR_IO);
    }
    reply_opened(sender, [len as u64, 0, 0, 0, 0, 0]);
}

/// TAG_CHDIR: `data[0]` = the path's length, lent, from `data[5]`'s base.
/// TAG_FCHDIR: `data[0]` = an open directory handle. Either way the program
/// is now in that directory, which it must be able to search.
fn handle_chdir(disk: &DiskState, sender: usize, msg: &Message) {
    let space = space_of(sender);
    let (uid, gid) = get_sender_uid_gid(sender);
    let to = if unsafe { FS_TYPE } == FsType::Ext2 {
        let found = if msg.tag == TAG_FCHDIR {
            base_of(sender, msg.data[0] + 1)
        } else {
            protocol::lent_path(sender, 0, msg.data[0] as usize, 0).and_then(|path| {
                let base = base_of(sender, msg.data[5])?;
                ext2_dir::resolve_inode(ext2_state(), base, path, uid, gid, true).map(|f| f.0)
            })
        };
        found.and_then(|ino| {
            let dir = ext2::read_inode(ext2_state(), ino)?;
            if !dir.is_dir() {
                Err(ERR_NOT_DIR)
            } else if !ext2::check_permission(&dir, uid, gid, 1) {
                Err(ERR_PERMISSION)
            } else {
                move_to(sender, space, ino)
            }
        })
    } else if msg.tag == TAG_FCHDIR {
        Err(ERR_NOT_SUPPORTED)
    } else {
        protocol::lent_path(sender, 0, msg.data[0] as usize, 0)
            .and_then(|path| fat_path(disk, sender, msg.data[5], path))
            .and_then(|path| match resolve_path(disk, path)? {
                (_, _, true, _, _) if path == b"/" => cwd::set(space, cwd::Where::Root, b""),
                (_, _, true, _, _) => cwd::set(space, cwd::Where::Path(path.len()), path),
                _ => Err(ERR_NOT_DIR),
            })
    };
    match to {
        Ok(left) => {
            reply_opened(sender, [0; 6]);
            // The directory left may have been removed while the program was
            // in it, and now nothing holds it.
            if let cwd::Where::Inode(ino) = left {
                settle(&[ino]);
            }
        }
        Err(code) => error_reply(sender, code),
    }
}

/// Put `sender`'s program in directory `ino`, and say what the record kept
/// for the program held, so that a directory nobody is in any more can go.
///
/// The directory becomes a descriptor in the caller's table, in the slot the
/// kernel keeps for one. That is what makes where a program *is* follow it:
/// a forked child has a copy of the slot and the program it execs keeps it,
/// and this server is told nothing and needs to be. Kept here by program, a
/// working directory stopped at `fork`, because a child is another program.
fn move_to(sender: usize, space: u64, ino: u32) -> Result<cwd::Where, u64> {
    let dir = OpenFile {
        in_use: true,
        by_fd: true,
        is_dir: true,
        may_write: false,
        fs: FsFileData::Ext2 { inode_num: ino },
        ..OpenFile::empty()
    };
    if let Some(handle) = handles::alloc(dir) {
        if syscall::sys_fd_serve(sender, handle as u64, syscall::FD_CWD).is_ok() {
            // The slot answers from now on. What it replaced is the kernel's
            // to release, and arrives here as a descriptor closing.
            return cwd::set(space, cwd::Where::Root, b"");
        }
        let _ = handles::release(handle);
    }
    // No room for one more handle: kept by program, as a FAT32 directory is.
    let at = if ino == ext2::EXT2_ROOT_INO { cwd::Where::Root } else { cwd::Where::Inode(ino) };
    cwd::set(space, at, b"")
}

/// TAG_GETCWD: 4096 bytes lent for writing. Reply: the path's length.
fn handle_getcwd(sender: usize) {
    let path = match (mounts::cwd_remote(sender), cwd_of(sender)) {
        // In a mounted filesystem: where that is mounted, and then where
        // its server says the directory is.
        (Some(remote), _) => mounts::remote_path(&remote),
        (None, (cwd::Where::Inode(ino), _)) => ext2_dir::path_of(ext2_state(), ino),
        (None, (_, path)) => Ok(path),
    };
    match path {
        Ok(p) if syscall::sys_lent_write(sender, 0, p) == Ok(p.len()) => {
            reply_opened(sender, [p.len() as u64, 0, 0, 0, 0, 0])
        }
        Ok(_) => error_reply(sender, ERR_IO),
        Err(code) => error_reply(sender, code),
    }
}

/// TAG_GIVE_CWD: `data[0]` = a task the caller's program made for another
/// program, not yet necessarily running. That program starts in the
/// caller's directory.
fn handle_give_cwd(sender: usize, msg: &Message) {
    let me = space_of(sender);
    let child = msg.data[0] as usize;
    let theirs = space_of(child);
    let parent = syscall::sys_task_info(child).map(|(_, parent, _)| parent);
    let allowed = me != 0
        && theirs != 0
        && theirs != me
        && parent.is_ok_and(|p| p != 0 && space_of(p) == me);
    if !allowed {
        return error_reply(sender, ERR_PERMISSION);
    }
    // Where the caller is, whichever way that is kept. A spawner also copies
    // its directory's descriptor into the child, which is what the child's
    // own children will inherit; this is for a filesystem with no directory
    // handles to make a descriptor of.
    let (at, path) = cwd_of(sender);
    match cwd::set(theirs, at, path) {
        Ok(left) => {
            reply_opened(sender, [0; 6]);
            if let cwd::Where::Inode(ino) = left {
                settle(&[ino]);
            }
        }
        Err(code) => error_reply(sender, code),
    }
}

/// TAG_TRUNCATE: data[0] = handle, data[1] = the new size.
fn handle_truncate(disk: &DiskState, sender: usize, msg: &Message) {
    let Some(file) = get_handle(msg.data[0] as usize, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    if unsafe { FS_TYPE } != FsType::Ext2 {
        if file.by_fd && !file.may_write {
            return error_reply(sender, ERR_INVALID_HANDLE);
        }
        return match u32::try_from(msg.data[1]).map_err(|_| ERR_NOT_SUPPORTED).and_then(|size| fat32_truncate(disk, file, size)) {
            Ok(()) => reply_opened(sender, [0; 6]),
            Err(code) => error_reply(sender, code),
        };
    }
    if file.by_fd && !file.may_write {
        return error_reply(sender, ERR_INVALID_HANDLE);
    }
    if !file.writable {
        return error_reply(sender, ERR_PERMISSION);
    }
    let ino = file.inode_num();
    match ext2_ops::truncate(ext2_state_mut(), ino, msg.data[1]) {
        Ok(()) => {
            pager::resized(ino, msg.data[1]);
            reply_opened(sender, [0; 6])
        }
        Err(code) => error_reply(sender, code),
    }
}

/// TAG_STAT: data[0]=handle, with an 88-byte buffer lent for writing.
/// Reply: tag=TAG_OK, data[0]=88, the record in the buffer.
fn handle_stat(sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let Some(file) = get_handle(handle, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let record = match &file.fs {
        FsFileData::Fat32 { first_cluster, .. } => {
            let cluster = unsafe { FAT_CLUSTER_BYTES } as u64;
            StatRecord {
                id: *first_cluster as u64,
                size: file.file_size as u64,
                mode: if file.is_dir { FAT_DIR_MODE } else { FAT_FILE_MODE },
                links: 1,
                uid: 0,
                gid: 0,
                atime: 0,
                mtime: 0,
                ctime: 0,
                blocks: (file.file_size as u64 + 511) / 512,
                block_size: cluster,
            }
        }
        FsFileData::Ext2 { inode_num } => {
            let e2 = ext2_state();
            let inode = match ext2::read_inode(e2, *inode_num) {
                Ok(i) => i,
                Err(code) => return error_reply(sender, code),
            };
            StatRecord {
                id: *inode_num as u64,
                size: inode.size64(),
                mode: inode.i_mode as u64,
                links: inode.i_links_count as u64,
                uid: inode.i_uid as u64,
                gid: inode.i_gid as u64,
                atime: inode.i_atime as u64,
                mtime: inode.i_mtime as u64,
                ctime: inode.i_ctime as u64,
                blocks: inode.i_blocks as u64,
                block_size: e2.block_size as u64,
            }
        }
        FsFileData::Device(_)
        | FsFileData::Disk(_)
        | FsFileData::Remote(_)
        | FsFileData::DevDir
        | FsFileData::None => {
            return error_reply(sender, ERR_INVALID_HANDLE)
        }
    };
    match syscall::sys_lent_write(sender, 0, &record.to_bytes()) {
        Ok(n) if n == STAT_LEN => reply_opened(sender, [STAT_LEN as u64, 0, 0, 0, 0, 0]),
        _ => error_reply(sender, ERR_IO),
    }
}
/// TAG_WRITE: data[0]=handle, data[2]=offset, data[3]=len (at most a page),
/// with a buffer that long lent for reading.
/// Reply: tag=TAG_OK, data[0]=bytes_written  OR  tag=TAG_ERROR, data[0]=error_code
fn handle_write(disk: &DiskState, sender: usize, msg: &Message) {
    if unsafe { FS_TYPE } == FsType::Ext2 {
        handle_write_ext2(sender, msg);
        return;
    }

    let handle = msg.data[0] as usize;
    let offset = msg.data[2] as u32;
    let len = (msg.data[3] as u32).min(PAGE_SIZE as u32);
    if !lend_in(sender, len as usize) {
        error_reply(sender, ERR_IO);
        return;
    }

    match get_handle(handle, sender) {
        Some(file) => {
            let (dir_cluster, fat_name) = match &file.fs {
                FsFileData::Fat32 { dir_cluster, fat_name, .. } => (*dir_cluster, *fat_name),
                _ => { error_reply(sender, ERR_IO); return; }
            };
            match write_file_data(disk, file, offset, len) {
                Ok(bytes_written) => {
                    // Update directory entry with new size
                    let new_size = file.file_size;
                    let _ = update_dir_entry_size(disk, dir_cluster, &fat_name, new_size);
                    reply_count(sender, bytes_written as u64);
                }
                Err(code) => error_reply(sender, code),
            }
        }
        None => error_reply(sender, ERR_INVALID_HANDLE),
    }
}


/// TAG_READDIR_BULK: data[0] = handle, data[1] = the index of the first entry
/// wanted, data[2] = the length of the buffer lent for writing. Fills it with
/// directory records (see `protocol::put_dirent`) and replies
/// `[bytes, next index, end]`.
fn handle_readdir_bulk(disk: &DiskState, sender: usize, msg: &Message) {
    if unsafe { FS_TYPE } == FsType::Ext2 {
        handle_readdir_bulk_ext2(sender, msg);
        return;
    }

    let start = msg.data[1];
    let room = (msg.data[2] as usize).min(PAGE_SIZE);
    let dir_cluster = match get_handle(msg.data[0] as usize, sender) {
        Some(file) if file.is_dir => match &file.fs {
            FsFileData::Fat32 { first_cluster, .. } => *first_cluster,
            _ => return error_reply(sender, ERR_IO),
        },
        Some(_) => return error_reply(sender, ERR_NOT_DIR),
        None => return error_reply(sender, ERR_INVALID_HANDLE),
    };

    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, PAGE_SIZE) };
    let spc = disk.bpb.sectors_per_cluster;
    let mut cluster = dir_cluster;
    let mut index = 0u64;
    let mut used = 0usize;
    let mut next = start;
    let mut end = true;

    'outer: loop {
        let start_lba = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start_lba, spc);
        for sector in 0..spc {
            let sec_data = match disk.cached_read_sector(start_lba + sector) {
                Ok(d) => d,
                // What was read so far is still good; the next request
                // starts at the sector that failed and reports it.
                Err(_) if used > 0 => {
                    end = false;
                    break 'outer;
                }
                Err(_) => return error_reply(sender, ERR_IO),
            };
            let mut sec_buf = [0u8; 512];
            sec_buf.copy_from_slice(sec_data);

            for e in 0..16 {
                let off = e * 32;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 {
                    break 'outer;
                }
                let attr = sec_buf[off + 11];
                // Deleted, long-name pieces and the volume label are not entries.
                if first_byte == 0xE5 || attr & 0x0F == 0x0F || attr & 0x08 != 0 {
                    continue;
                }
                if index < start {
                    index += 1;
                    continue;
                }
                let mut name = [0u8; 12];
                let name_len = fat_display_name(&sec_buf[off..off + 11], &mut name);
                let hi = read_u16(&sec_buf, off + 20) as u64;
                let lo = read_u16(&sec_buf, off + 26) as u64;
                let size = read_u32(&sec_buf, off + 28) as u64;
                let kind = if attr & 0x10 != 0 { DT_DIR } else { DT_REG };
                match put_dirent(&mut buf[..room], used, (hi << 16) | lo, index + 1, size, kind, &name[..name_len]) {
                    Some(len) => {
                        used += len;
                        next = index + 1;
                        index += 1;
                    }
                    None => {
                        end = false;
                        break 'outer;
                    }
                }
            }
        }
        match disk.fat_next(cluster) {
            Some(n) => cluster = n,
            None => break,
        }
    }

    reply_dirents(sender, used, next, end);
}

/// A FAT short name as a name: `HELLO   ELF` is `HELLO.ELF`, `USR        `
/// is `USR`.
fn fat_display_name(raw: &[u8], out: &mut [u8; 12]) -> usize {
    let base = raw[..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    let ext = raw[8..11].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    out[..base].copy_from_slice(&raw[..base]);
    if ext == 0 {
        return base;
    }
    out[base] = b'.';
    out[base + 1..base + 1 + ext].copy_from_slice(&raw[8..8 + ext]);
    base + 1 + ext
}

/// Reply to a bulk readdir: `used` bytes of records from `CLIENT_BUF` into
/// what the caller lent, and where to carry on.
pub(crate) fn reply_dirents(sender: usize, used: usize, next: u64, end: bool) {
    if !lend_out(sender, used) {
        return error_reply(sender, ERR_IO);
    }
    unsafe { MOVED = Some(next) };
    reply_opened(sender, [used as u64, next, end as u64, 0, 0, 0]);
}

/// TAG_STATFS, with 64 bytes lent for writing.
fn handle_statfs(sender: usize) {
    let words: [u64; 8] = if unsafe { FS_TYPE } == FsType::Ext2 {
        let e2 = ext2_state();
        let free = e2.free_blocks_count as u64;
        [
            0xEF53,
            e2.block_size as u64,
            e2.total_blocks as u64,
            free,
            free.saturating_sub(e2.reserved_blocks as u64),
            e2.total_inodes as u64,
            e2.free_inodes_count as u64,
            MAX_NAME as u64,
        ]
    } else {
        // FAT keeps its free count in a hint nothing here reads; say what is
        // known and leave the counts empty.
        [0x4d44, unsafe { FAT_CLUSTER_BYTES } as u64, 0, 0, 0, 0, 0, 12]
    };
    let mut record = [0u8; STATFS_LEN];
    for (i, w) in words.iter().enumerate() {
        record[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
    }
    match syscall::sys_lent_write(sender, 0, &record) {
        Ok(n) if n == STATFS_LEN => reply_opened(sender, [STATFS_LEN as u64, 0, 0, 0, 0, 0]),
        _ => error_reply(sender, ERR_IO),
    }
}
// ---------------------------------------------------------------------------
// ext2 IPC handlers
// ---------------------------------------------------------------------------

pub fn get_sender_uid_gid(sender: usize) -> (u32, u32) {
    // The server this filesystem is mounted in asks for somebody else, and
    // has said who.
    if let Some(who) = mounts::acting(sender) {
        return who;
    }
    // A caller that has gone is nobody. It was root, which is the one
    // thing a caller nobody can vouch for should not be.
    syscall::sys_get_tuid(sender).unwrap_or((who::NOBODY, who::NOBODY))
}

/// A program has been given the directory it is in as a descriptor: the
/// record kept for it by program is let go, and with it the directory that
/// record held.
pub(crate) fn left_directory(space: u64) {
    if let Ok(cwd::Where::Inode(ino)) = cwd::set(space, cwd::Where::Root, b"") {
        settle(&[ino]);
    }
}


fn handle_read_ext2(sender: usize, msg: &Message) {
    let handle = msg.data[0] as usize;
    let offset = msg.data[2] as u32;
    let max_bytes = msg.data[3] as u32;

    match get_handle(handle, sender) {
        Some(file) => {
            let ino = file.inode_num();
            let inode = match ext2::read_inode(ext2_state(), ino) {
                Ok(inode) => inode,
                Err(code) => return error_reply(sender, code),
            };
            let read = ext2::read_file_data(ext2_state(), &inode, offset, max_bytes);
            // A shared mapping's writes are in the cache before the file.
            if let Ok(n) = read {
                pager::read_through(ino, offset as u64, n as usize);
            }
            reply_read(sender, read);
        }
        None => error_reply(sender, ERR_INVALID_HANDLE),
    }
}

/// Writes that have left their transaction open for the next to join.
static mut DEFERRED: u32 = 0;
/// How many may share one, and how long one waits for the next: 256 KiB of a
/// file, and a fiftieth of a second.
const MAX_DEFERRED: u32 = 64;
const FLUSH_TICKS: u64 = 2;
/// When the first of them began waiting, and how long any may: half a
/// second. The other limit is on quiet, and a server answering reads all
/// day is never quiet.
static mut WAITING_SINCE: u64 = 0;
const MAX_WAIT_TICKS: u64 = 50;
/// How many blocks a transaction may already hold when a request begins:
/// half of what it has room for, so that whatever the request changes fits.
const ROOM: usize = journal::MAX_TXN_BLOCKS / 2;

/// Commit the open transaction, if there is one, and write it where it
/// belongs.
pub(crate) fn commit_pending() {
    unsafe { DEFERRED = 0 };
    if !journal::in_transaction(journal_ref()) {
        return;
    }
    match journal::commit(journal_mut(), ext2_state()) {
        Ok(_) => {
            if let Err(e) = journal::checkpoint(journal_mut(), ext2_state()) {
                println!("[vfs] checkpoint failed ({}); the journal will replay it", e);
            }
        }
        Err(e) => {
            println!("[vfs] commit failed ({}); the change was abandoned", e);
            journal::abort(journal_mut());
        }
    }
}

/// Run one filesystem-modifying operation as a single transaction.
///
/// Everything it writes is held in the journal until it is complete, so a
/// machine that stops half way through leaves the filesystem as it was rather
/// than as neither one thing nor the other. Writes that were waiting for
/// company are committed with it.
fn transacted<F: FnOnce()>(body: F) {
    if journal::staged(journal_ref()) > ROOM {
        commit_pending();
    }
    journal::begin(journal_mut());
    body();
    commit_pending();
}

/// Run a write to a file, and leave its transaction open for the next.
///
/// A file is written a page at a time, and each page changes the same four
/// blocks: the inode, a bitmap, a group's counts and the filesystem's. A
/// transaction for each wrote those four twice over, with a descriptor, a
/// commit and the journal's own superblock twice, for every page: fourteen
/// blocks to store one. Left open, the transaction takes the next page's
/// changes to the same four blocks, and is committed once — when it has
/// been joined [`MAX_DEFERRED`] times, when anything else changes the
/// filesystem, or when nothing has asked for [`FLUSH_TICKS`].
///
/// Nothing is lost by it that was not already at risk: a write was always
/// answered before its transaction was committed. And nothing that frees a
/// block waits — only writes do, which only allocate — so a block is never
/// given to a second file while the disk still says it is the first's.
fn deferred<F: FnOnce()>(body: F) {
    if journal::staged(journal_ref()) > ROOM {
        commit_pending();
    }
    if !journal::in_transaction(journal_ref()) {
        unsafe { WAITING_SINCE = syscall::sys_ticks() };
    }
    journal::begin(journal_mut());
    body();
    unsafe { DEFERRED += 1 };
    if unsafe { DEFERRED } >= MAX_DEFERRED || journal::staged(journal_ref()) > ROOM {
        commit_pending();
    }
}

fn handle_write_ext2(sender: usize, msg: &Message) {
    if ext2_state().read_only {
        error_reply(sender, ERR_READ_ONLY);
        return;
    }
    let handle = msg.data[0] as usize;
    let offset = msg.data[2] as u32;
    let len = (msg.data[3] as u32).min(PAGE_SIZE as u32);

    match get_handle(handle, sender) {
        Some(file) => {
            if !file.writable {
                error_reply(sender, ERR_PERMISSION);
                return;
            }
            let inode_num = file.inode_num();
            let mut inode = match ext2::read_inode(ext2_state(), inode_num) {
                Ok(inode) => inode,
                Err(code) => return error_reply(sender, code),
            };
            if !lend_in(sender, len as usize) {
                error_reply(sender, ERR_IO);
                return;
            }
            let e2 = ext2_state_mut();
            match ext2::write_file_data(e2, &mut inode, inode_num, offset, len) {
                Ok(bytes_written) => {
                    // A mapping of the file sees what was written.
                    pager::wrote(inode_num, offset as u64, bytes_written as usize);
                    pager::resized(inode_num, inode.size64());
                    reply_count(sender, bytes_written as u64);
                }
                Err(code) => error_reply(sender, code),
            }
        }
        None => error_reply(sender, ERR_INVALID_HANDLE),
    }
}


fn handle_readdir_bulk_ext2(sender: usize, msg: &Message) {
    let start = msg.data[1];
    let room = (msg.data[2] as usize).min(PAGE_SIZE);
    let e2 = ext2_state();
    let dir = match get_handle(msg.data[0] as usize, sender) {
        Some(file) if file.is_dir => match ext2::read_inode(e2, file.inode_num()) {
            Ok(inode) => inode,
            Err(code) => return error_reply(sender, code),
        },
        Some(_) => return error_reply(sender, ERR_NOT_DIR),
        None => return error_reply(sender, ERR_INVALID_HANDLE),
    };

    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, PAGE_SIZE) };
    let mut used = 0usize;
    let mut next = start;
    let mut end = true;
    let walked = ext2_dir::for_each_entry(e2, &dir, |index, ino, kind, name| {
        if (index as u64) < start {
            return true;
        }
        // The inode says what the entry is even where the entry does not.
        let (size, dt) = match ext2::read_inode(e2, ino) {
            Ok(i) if i.is_dir() => (i.size64(), DT_DIR),
            Ok(i) if i.is_regular() => (i.size64(), DT_REG),
            Ok(i) if i.i_mode & ext2::S_IFMT == ext2::S_IFLNK => (i.size64(), DT_LNK),
            Ok(i) => (i.size64(), DT_UNKNOWN),
            Err(_) => (0, if kind == ext2::FT_DIR { DT_DIR } else { DT_UNKNOWN }),
        };
        match put_dirent(&mut buf[..room], used, ino as u64, index as u64 + 1, size, dt, name) {
            Some(len) => {
                used += len;
                next = index as u64 + 1;
                true
            }
            None => {
                end = false;
                false
            }
        }
    });
    if let Err(code) = walked {
        return error_reply(sender, code);
    }
    reply_dirents(sender, used, next, end);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[vfs] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
