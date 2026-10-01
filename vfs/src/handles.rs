//! The open-file table.
//!
//! A handle names a file, not a copy of it: an ext2 handle holds the inode
//! number and the inode is read when the handle is used. Two handles on one
//! file therefore never disagree about its size or where its blocks are, and
//! a file shortened through one cannot be written past its end through a
//! stale block map in the other.
//!
//! A handle is held one of two ways.
//!
//! **By a program.** It belongs to the program that opened it — every thread
//! of it may use it — and goes when the program does. Programs are named by
//! their address space's id, which the kernel never reuses; a TID would have
//! made a file one thread opened useless to its siblings, and, being recycled,
//! would have handed a dead task's files to whatever took its slot. The server
//! watches every program it gives such a handle to. This is what quark-rt's
//! clients use.
//!
//! **By a descriptor.** The kernel counts who holds it: the handle is the
//! cookie of a descriptor in the opener's table (`SYS_FD_SERVE`), and whoever
//! holds a descriptor for it — the opener, a child it forked, the program it
//! became — may use it. It is what an open file *is* to a C program: one
//! position, shared by every descriptor made from the first, and closed when
//! the last of them is. The server keeps nothing per program for it and
//! watches nobody: the kernel says when the last descriptor has gone.

use quark_rt::syscall;

/// Handles for the whole system.
pub const MAX_OPEN_FILES: usize = 512;
/// Handles one program may hold: a quarter, so that one program opening files
/// in a loop cannot stop every other program opening any. Comfortably above
/// the hundred `dtest files` opens at once, which is the most anything here
/// asks for.
pub const MAX_PER_PROGRAM: usize = MAX_OPEN_FILES / 4;

pub enum FsFileData {
    Fat32 {
        first_cluster: u32,
        cur_cluster: u32,
        cur_cluster_offset: u32,
        dir_cluster: u32,
        fat_name: [u8; 11],
    },
    Ext2 {
        inode_num: u32,
    },
    /// One of `/dev`'s devices, which no disk holds.
    Device(crate::devices::Device),
    /// `/dev` itself.
    DevDir,
    None,
}

pub struct OpenFile {
    pub in_use: bool,
    /// The owning program's space id. 0 for a descriptor's.
    pub owner: u64,
    /// Held by descriptors rather than by a program.
    pub by_fd: bool,
    /// A descriptor's position: where the next read or write that names none
    /// happens. Here rather than in the client because it is shared by every
    /// descriptor for the file, in whichever programs they have ended up.
    pub pos: u64,
    /// A directory descriptor's position: the index of the next entry.
    pub dirpos: u64,
    /// Writes go to the end of the file, wherever the position is.
    pub append: bool,
    /// What the descriptor was opened to do.
    pub may_read: bool,
    pub may_write: bool,
    /// FAT32's size, which lives in its directory entry. An ext2 handle reads
    /// the inode instead.
    pub file_size: u32,
    pub is_dir: bool,
    pub writable: bool,
    /// A symbolic link opened as itself: it can be asked about, and nothing
    /// else.
    pub link: bool,
    pub read_offset: u32,
    pub fs: FsFileData,
}

impl OpenFile {
    pub const fn empty() -> Self {
        OpenFile {
            in_use: false,
            owner: 0,
            by_fd: false,
            pos: 0,
            dirpos: 0,
            append: false,
            may_read: true,
            may_write: true,
            file_size: 0,
            is_dir: false,
            writable: false,
            link: false,
            read_offset: 0,
            fs: FsFileData::None,
        }
    }

    /// The inode an ext2 handle names, or 0.
    pub fn inode_num(&self) -> u32 {
        match self.fs {
            FsFileData::Ext2 { inode_num } => inode_num,
            _ => 0,
        }
    }
}

static mut TABLE: [OpenFile; MAX_OPEN_FILES] = {
    const EMPTY: OpenFile = OpenFile::empty();
    [EMPTY; MAX_OPEN_FILES]
};

fn table() -> &'static mut [OpenFile; MAX_OPEN_FILES] {
    unsafe { &mut *core::ptr::addr_of_mut!(TABLE) }
}

/// Put `file` in the table and return its handle, or None if it is full.
pub fn alloc(file: OpenFile) -> Option<usize> {
    let t = table();
    if file.by_fd {
        // No share to keep to: a program holds no more of these than its
        // descriptor table has room for, and the kernel keeps that.
        let i = t.iter().position(|f| !f.in_use)?;
        t[i] = file;
        t[i].in_use = true;
        t[i].owner = 0;
        return Some(i);
    }
    let owner = file.owner;
    if owner == 0 {
        return None;
    }
    if t.iter().filter(|f| f.in_use && !f.by_fd && f.owner == owner).count() >= MAX_PER_PROGRAM {
        return None;
    }
    let i = t.iter().position(|f| !f.in_use)?;
    t[i] = file;
    t[i].in_use = true;
    // Told when the program is gone, so its handles go with it. Watching a
    // program twice is the same as watching it once, and a program already
    // gone cannot be calling.
    let _ = syscall::sys_space_watch(owner);
    Some(i)
}

/// Program `space`'s handle `handle`, if it is one.
pub fn get(handle: usize, space: u64) -> Option<&'static mut OpenFile> {
    let f = table().get_mut(handle)?;
    if f.in_use && !f.by_fd && space != 0 && f.owner == space { Some(f) } else { None }
}

/// Whether `handle` is one descriptors hold.
pub fn is_descriptor(handle: usize) -> bool {
    table().get(handle).is_some_and(|f| f.in_use && f.by_fd)
}

/// The handle descriptors hold as `handle`, whoever holds them. The caller
/// has asked the kernel whether the task in front of it does.
pub fn descriptor(handle: usize) -> Option<&'static mut OpenFile> {
    let f = table().get_mut(handle)?;
    if f.in_use && f.by_fd { Some(f) } else { None }
}

/// The last descriptor for `handle` has gone. Returns the inode it named (0
/// for one with none), or None if it was not a descriptor's handle.
pub fn release(handle: usize) -> Option<u32> {
    let f = descriptor(handle)?;
    let ino = f.inode_num();
    *f = OpenFile::empty();
    Some(ino)
}

/// Close program `space`'s handle `handle`. Returns the inode it named (0 for
/// FAT32), or None if it was not one of that program's.
pub fn close(handle: usize, space: u64) -> Option<u32> {
    let f = get(handle, space)?;
    let ino = f.inode_num();
    *f = OpenFile::empty();
    Some(ino)
}

/// Close every handle program `space` held. The inodes they named are written
/// to `closed`, and their number returned.
pub fn close_all(space: u64, closed: &mut [u32; MAX_OPEN_FILES]) -> usize {
    let mut n = 0;
    for (i, f) in table().iter_mut().enumerate() {
        if f.in_use && f.owner == space {
            closed[n] = f.inode_num();
            n += 1;
            *f = OpenFile::empty();
            // The program is going, so whoever waited through it is too.
            while crate::locks::drop_handle(i).is_some() {}
        }
    }
    n
}

/// What locks on `file` are keyed by: its inode, or for a file with none,
/// something as stable. `None` for a handle nothing can lock.
pub fn lock_key(file: &OpenFile) -> Option<u32> {
    if file.link {
        return None;
    }
    match &file.fs {
        FsFileData::Ext2 { inode_num } => Some(*inode_num),
        // An empty FAT32 file has no cluster: its directory and name stand in,
        // folded into the half of the numbers clusters never reach.
        FsFileData::Fat32 { first_cluster, dir_cluster, fat_name, .. } => {
            if *first_cluster != 0 {
                return Some(*first_cluster);
            }
            let mut h = *dir_cluster ^ 0x9E37_79B9;
            for &b in fat_name {
                h = h.rotate_left(5) ^ b as u32;
            }
            Some(0x8000_0000 | h)
        }
        FsFileData::Device(dev) => Some(crate::devices::id_of(*dev) as u32),
        FsFileData::DevDir => Some(crate::devices::DIR_ID as u32),
        FsFileData::None => None,
    }
}

/// Whether any handle still names inode `ino`.
pub fn inode_is_open(ino: u32) -> bool {
    ino != 0
        && (table().iter().any(|f| f.in_use && f.inode_num() == ino)
            || crate::cwd::holds(ino)
            || crate::pager::holds(ino))
}

/// Inodes whose last name went while a handle or a working directory still
/// named them. Each is held by one of those, so there can never be more than
/// the two tables hold.
const MAX_ORPHANS: usize = MAX_OPEN_FILES + crate::cwd::MAX_PROGRAMS;
static mut ORPHANS: [u32; MAX_ORPHANS] = [0; MAX_ORPHANS];

fn orphans() -> &'static mut [u32; MAX_ORPHANS] {
    unsafe { &mut *core::ptr::addr_of_mut!(ORPHANS) }
}

pub fn add_orphan(ino: u32) {
    if is_orphan(ino) {
        return;
    }
    if let Some(slot) = orphans().iter_mut().find(|o| **o == 0) {
        *slot = ino;
    }
}

pub fn is_orphan(ino: u32) -> bool {
    ino != 0 && orphans().contains(&ino)
}

pub fn forget_orphan(ino: u32) {
    for o in orphans().iter_mut().filter(|o| **o == ino) {
        *o = 0;
    }
}
