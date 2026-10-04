//! The VFS protocol as this server speaks it: request tags, flags, error
//! codes, and the records a request's lent buffer carries.
//!
//! `docs/vfs.md` is the contract. The clients — quark-rt, the C library, the
//! Linux translation layer — keep their own copies of these numbers.

use quark_rt::syscall;

pub const TAG_OPEN: u64 = 1;
pub const TAG_READ: u64 = 2;
pub const TAG_CLOSE: u64 = 3;
// 4 read one directory entry at a time and cut its name to 32 bytes;
// TAG_READDIR_BULK replaces it and the number stays taken.
pub const TAG_STAT: u64 = 5;
pub const TAG_WRITE: u64 = 6;
// 7 was CREATE, whose path travelled in the message and was cut to 40 bytes.
// OPEN_CREATE and TAG_MKDIR replace it; the number stays taken.
pub const TAG_READDIR_BULK: u64 = 8;
pub const TAG_MKDIR: u64 = 9;
/// Remove a name; the file goes with its last one.
pub const TAG_UNLINK: u64 = 10;
pub const TAG_RMDIR: u64 = 11;
/// Two paths, lent end to end.
pub const TAG_RENAME: u64 = 12;
pub const TAG_TRUNCATE: u64 = 13;
/// What the filesystem is and how full.
pub const TAG_STATFS: u64 = 14;
/// A second name for a file: two paths, lent end to end as RENAME lends them.
pub const TAG_LINK: u64 = 15;
/// A symbolic link: the target, then the new path, lent end to end.
pub const TAG_SYMLINK: u64 = 16;
/// What a symbolic link says: the path, then room for the answer, in one
/// buffer lent for reading and writing.
pub const TAG_READLINK: u64 = 17;
/// Move the caller's program into a directory, by path or by handle.
pub const TAG_CHDIR: u64 = 18;
pub const TAG_FCHDIR: u64 = 19;
/// Where the caller's program is, as a path.
pub const TAG_GETCWD: u64 = 20;
/// Start a program the caller is making in the caller's directory.
pub const TAG_GIVE_CWD: u64 = 21;
/// Take, drop or ask about a record lock on an open file.
pub const TAG_LOCK: u64 = 22;
/// A capability to map an open file.
pub const TAG_MAP: u64 = 23;
/// Move a descriptor's position: `[handle, offset, whence]`.
pub const TAG_SEEK: u64 = 24;
/// Change what a file's inode says of it — mode, owner, times.
pub const TAG_SETATTR: u64 = 25;
/// Make something that is not a file or a directory: `[path length, mode]`,
/// where the mode's type says what. A named pipe is the only kind there is.
pub const TAG_MKNOD: u64 = 26;
/// Ask something of the device behind a handle: `[handle, operation]`.
pub const TAG_DEVCTL: u64 = 27;
/// DEVCTL: have a disk's driver read its partition table again.
pub const DEVCTL_RESCAN: u64 = 1;
/// Put the filesystem a server serves at a directory: `[path length, record
/// length, the server's task]`, with the path lent and after it what is
/// written down of the mount — where it came from and where it is, each
/// ended by a NUL — and with a capability for the server offered.
pub const TAG_ATTACH: u64 = 28;
/// Take it away again: `[path length]`, the path of the directory.
pub const TAG_DETACH: u64 = 29;
/// What is mounted: `[index]`, with room lent for one mount's record. The
/// reply is `[record length, kind, the server's process id]`; past the last
/// mount it is an error whose second word is how many there are.
pub const TAG_MOUNTS: u64 = 30;
// Between a server and the one its filesystem is mounted in. Nobody else
// can say these: a mounted filesystem's server is called by whoever started
// it and by the server above it, and by nobody else.
/// "You are mine": the caller is the server above. Reply: `[root id, kind,
/// read-only]`. A server that was not started to be mounted refuses.
pub const TAG_ADOPT: u64 = 31;
/// Who the requests that follow are for: `[uid, gid]`.
pub const TAG_IDENTITY: u64 = 32;
/// Stop: let the volume go and end. Refused while anything is open.
pub const TAG_RETIRE: u64 = 33;
/// The path of a directory handle, from this filesystem's root, into what
/// is lent.
pub const TAG_PATH_OF: u64 = 34;
/// Have everything that was written be on the disk before answering.
pub const TAG_SYNC: u64 = 35;
pub const TAG_OK: u64 = 0;
pub const TAG_ERROR: u64 = u64::MAX;

/// A mount's kind, as [`TAG_MOUNTS`] and [`TAG_ADOPT`] say it.
pub const KIND_EXT2: u64 = 1;
pub const KIND_EXT4: u64 = 2;
pub const KIND_FAT: u64 = 3;
/// ext2 in its server's own memory (`mount -t tmpfs`).
pub const KIND_TMPFS: u64 = 4;

/// OPEN makes the file if the name is free.
pub const OPEN_CREATE: u64 = 1;
/// With `OPEN_CREATE`, the name must be free.
pub const OPEN_EXCLUSIVE: u64 = 2;
/// Empty a regular file the caller may write.
pub const OPEN_TRUNCATE: u64 = 4;
/// The path must name a directory.
pub const OPEN_DIRECTORY: u64 = 8;
/// A symbolic link at the end of the path is opened itself, not followed.
pub const OPEN_NOFOLLOW: u64 = 16;
/// The handle is the kernel's to count: the caller is given a descriptor for
/// it, and the reply's first word is `handle << 32 | descriptor`.
pub const OPEN_DESCRIPTOR: u64 = 0x20;
/// Every write through the descriptor goes to the end of the file.
pub const OPEN_APPEND: u64 = 0x40;
/// What the descriptor may do. Checked when it is opened, against the file's
/// mode, and again on every read and write.
pub const OPEN_READ: u64 = 0x80;
pub const OPEN_WRITE: u64 = 0x100;
/// The caller will not wait for what it opens. It matters for one thing: a
/// named pipe opened to write with nobody reading is refused
/// ([`ERR_NO_PEER`]) rather than given an end.
pub const OPEN_NOWAIT: u64 = 0x200;
/// From the server above, for somebody else: [`OPEN_READ`] and [`OPEN_WRITE`]
/// are checked as they are for a descriptor, and what is given is a handle.
/// The descriptor is the server above's to make.
pub const OPEN_PROXIED: u64 = 0x400;

/// The file is opened to be asked about, and for nothing else: the handle
/// answers STAT and what its filesystem is, as a symbolic link opened as
/// itself does. Nothing of the file's own mode is asked for — knowing how
/// big a file is and whose takes the right to look in its directory, not
/// the right to read it, and without this `ls -l` of a directory showed
/// nothing of anybody else's private files but an error. It means this only
/// by itself: with anything that reads, writes, makes or keeps, it means
/// nothing.
pub const OPEN_ASK: u64 = 0x800;

/// Whether an OPEN's flags are [`OPEN_ASK`] and nothing that asks for more.
pub fn asks(flags: u64) -> bool {
    let more = OPEN_CREATE | OPEN_TRUNCATE | OPEN_DESCRIPTOR | OPEN_READ | OPEN_WRITE | OPEN_APPEND;
    flags & OPEN_ASK != 0 && flags & more == 0
}

/// In a READ's or a WRITE's offset, or a READDIR_BULK's start: wherever the
/// descriptor is. The position moves past what is transferred.
pub const AT_POSITION: u64 = u64::MAX;

/// Set in a word that carries permission bits for something being made —
/// OPEN's `data[2]`, MKDIR's `data[1]` — to say the bits below it are meant.
/// A word of zero is a client that says nothing, and gets 0644 or 0755.
pub const MODE_GIVEN: u64 = 1 << 16;

pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

/// SETATTR's `data[1]`: which of the five words lent after the path to use.
pub const ATTR_MODE: u64 = 1;
pub const ATTR_UID: u64 = 2;
pub const ATTR_GID: u64 = 4;
pub const ATTR_ATIME: u64 = 8;
pub const ATTR_MTIME: u64 = 16;
/// The time the request arrives, rather than one the caller names.
pub const ATTR_ATIME_NOW: u64 = 32;
pub const ATTR_MTIME_NOW: u64 = 64;
/// `[mode, uid, gid, atime, mtime]`, eight bytes each.
pub const ATTR_LEN: usize = 40;

/// LINK: follow a symbolic link at the end of the source path.
pub const LINK_FOLLOW: u64 = 1;

/// LOCK: answer when the lock is granted rather than refusing now.
pub const LOCK_WAIT: u64 = 1;
/// LOCK: the lock is the handle's, not the program's.
pub const LOCK_OFD: u64 = 2;
/// LOCK: grant nothing; say what would be in the way.
pub const LOCK_QUERY: u64 = 4;

// Error codes, in an error reply's first word.
pub const ERR_NOT_FOUND: u64 = 1;
pub const ERR_INVALID_HANDLE: u64 = 2;
pub const ERR_IO: u64 = 3;
pub const ERR_TOO_MANY_OPEN: u64 = 4;
pub const ERR_INVALID_PATH: u64 = 5;
pub const ERR_NOT_DIR: u64 = 6;
pub const ERR_IS_DIR: u64 = 7;
pub const ERR_PERMISSION: u64 = 8;
/// The filesystem was mounted read-only, because it uses something a writer
/// would have to maintain and this does not.
pub const ERR_READ_ONLY: u64 = 9;
pub const ERR_EXISTS: u64 = 10;
pub const ERR_NOT_EMPTY: u64 = 11;
/// Something this filesystem cannot do, as opposed to something that failed.
pub const ERR_NOT_SUPPORTED: u64 = 12;
/// A path over [`MAX_PATH`] bytes, or a name over [`MAX_NAME`].
pub const ERR_NAME_TOO_LONG: u64 = 13;
/// Nowhere to put what was written.
pub const ERR_NO_SPACE: u64 = 14;
/// A lookup followed more symbolic links than it may.
pub const ERR_LOOP: u64 = 15;
/// A lock that would have to wait, asked for without waiting.
pub const ERR_WOULD_BLOCK: u64 = 16;
/// Waiting for this lock would wait for ever.
pub const ERR_DEADLOCK: u64 = 17;
/// The file has as many names as the filesystem allows.
pub const ERR_TOO_MANY_LINKS: u64 = 18;
/// A named pipe, opened to write by somebody who will not wait, that nobody
/// has open to read.
pub const ERR_NO_PEER: u64 = 19;
/// A device that somebody else is using: a disk with a filesystem mounted
/// from it, or the one this system is running from.
pub const ERR_BUSY: u64 = 20;
/// Two names in two filesystems, asked for as one file.
pub const ERR_CROSS_DEVICE: u64 = 21;
/// Never sent: what a lookup says when the path leaves this filesystem
/// through a directory another is mounted on. Whoever asked reads where it
/// went from [`crate::ext2_dir::crossing`].
pub const ERR_ELSEWHERE: u64 = 0xE15E;

pub const MAX_PATH: usize = 4095;
/// What an ext2 directory entry's one-byte length allows.
pub const MAX_NAME: usize = 255;

/// Where a lent path is copied to be read: room for the two a rename names.
pub const PATH_BUF: usize = 0x88_0000_0000;
pub const PATH_BUF_PAGES: usize = 2;

/// Copy the `len` bytes of path `sender` lent, from `offset` in what it lent,
/// to `at` in [`PATH_BUF`], and return them.
///
/// A path that is too long is refused, never shortened: the one thing worse
/// than failing to open a file is opening a different one.
pub fn lent_path(sender: usize, offset: usize, len: usize, at: usize) -> Result<&'static [u8], u64> {
    if len == 0 {
        return Err(ERR_INVALID_PATH);
    }
    if len > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    if at + len > PATH_BUF_PAGES * 4096 {
        return Err(ERR_INVALID_PATH);
    }
    let buf = unsafe { core::slice::from_raw_parts_mut((PATH_BUF + at) as *mut u8, len) };
    match syscall::sys_lent_read(sender, offset, buf) {
        Ok(n) if n == len => {}
        _ => return Err(ERR_INVALID_PATH),
    }
    if buf.contains(&0) {
        return Err(ERR_INVALID_PATH);
    }
    Ok(buf)
}

/// A directory record: `id`, `next`, `size` (8 bytes each), `reclen` (2),
/// `type` (1), `namelen` (1), then the name and a NUL, padded to 8.
pub const DIRENT_HEADER: usize = 28;
pub const DT_UNKNOWN: u8 = 0;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;

/// Write one directory record at `at` in `buf`. Returns its length, or None
/// if it does not fit.
pub fn put_dirent(buf: &mut [u8], at: usize, id: u64, next: u64, size: u64, kind: u8, name: &[u8]) -> Option<usize> {
    let reclen = (DIRENT_HEADER + name.len() + 1 + 7) & !7;
    if name.len() > MAX_NAME || at + reclen > buf.len() {
        return None;
    }
    let r = &mut buf[at..at + reclen];
    r.fill(0);
    r[0..8].copy_from_slice(&id.to_le_bytes());
    r[8..16].copy_from_slice(&next.to_le_bytes());
    r[16..24].copy_from_slice(&size.to_le_bytes());
    r[24..26].copy_from_slice(&(reclen as u16).to_le_bytes());
    r[26] = kind;
    r[27] = name.len() as u8;
    r[DIRENT_HEADER..DIRENT_HEADER + name.len()].copy_from_slice(name);
    Some(reclen)
}

/// What STATFS fills a lent buffer with: eight little-endian words — magic,
/// block size, blocks, free, free to anybody, inodes, free inodes, the longest
/// name.
pub const STATFS_LEN: usize = 64;

pub const STAT_LEN: usize = 88;

/// What `STAT` fills a lent buffer with: eleven little-endian words.
pub struct StatRecord {
    pub id: u64,
    pub size: u64,
    /// With the file-type bits.
    pub mode: u64,
    pub links: u64,
    pub uid: u64,
    pub gid: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    /// In 512-byte units.
    pub blocks: u64,
    pub block_size: u64,
}

impl StatRecord {
    pub fn to_bytes(&self) -> [u8; STAT_LEN] {
        let words = [
            self.id,
            self.size,
            self.mode,
            self.links,
            self.uid,
            self.gid,
            self.atime,
            self.mtime,
            self.ctime,
            self.blocks,
            self.block_size,
        ];
        let mut out = [0u8; STAT_LEN];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}
