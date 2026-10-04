//! The file server, as a client sees it.
//!
//! `docs/vfs.md` is the protocol. Paths are lent with the call, so their
//! length is the filesystem's business rather than the message's.

use crate::ipc::Message;
use crate::syscall;

// VFS IPC tags
const TAG_OPEN: u64 = 1;
const TAG_READ: u64 = 2;
const TAG_CLOSE: u64 = 3;
const TAG_STAT: u64 = 5;
const TAG_WRITE: u64 = 6;
const TAG_READDIR_BULK: u64 = 8;
const TAG_MKDIR: u64 = 9;
const TAG_UNLINK: u64 = 10;
const TAG_RMDIR: u64 = 11;
const TAG_RENAME: u64 = 12;
const TAG_LINK: u64 = 15;
const TAG_SYMLINK: u64 = 16;
const TAG_READLINK: u64 = 17;
const TAG_CHDIR: u64 = 18;
const TAG_FCHDIR: u64 = 19;
const TAG_GETCWD: u64 = 20;
const TAG_GIVE_CWD: u64 = 21;
const TAG_LOCK: u64 = 22;
const TAG_MAP: u64 = 23;
const TAG_TRUNCATE: u64 = 13;
const TAG_STATFS: u64 = 14;
const TAG_SEEK: u64 = 24;
const TAG_SETATTR: u64 = 25;
const TAG_MKNOD: u64 = 26;
const TAG_BIND: u64 = 36;
const TAG_CONNECT: u64 = 37;
const TAG_DEVCTL: u64 = 27;
const TAG_ATTACH: u64 = 28;
const TAG_DETACH: u64 = 29;
const TAG_MOUNTS: u64 = 30;
const TAG_SYNC: u64 = 35;
const TAG_ERROR: u64 = u64::MAX;

/// What kind of filesystem a mount is, as [`mounted`] says it.
pub const KIND_EXT2: u64 = 1;
pub const KIND_EXT4: u64 = 2;
pub const KIND_FAT: u64 = 3;
/// ext2 in its server's own memory (`mount -t tmpfs`).
pub const KIND_TMPFS: u64 = 4;

/// [`devctl`]'s operations: have a disk's driver read its partition table
/// again.
pub const DEVCTL_RESCAN: u64 = 1;

/// The most one read or write carries.
pub const MAX_IO: usize = 4096;
/// The longest path the server takes. A longer one is refused, not cut.
pub const MAX_PATH: usize = 4095;

/// [`open_with`] makes the file if the name is free.
pub const OPEN_CREATE: u64 = 1;
/// With [`OPEN_CREATE`], the name must be free.
pub const OPEN_EXCLUSIVE: u64 = 2;
/// Empty a regular file the caller may write.
pub const OPEN_TRUNCATE: u64 = 4;
/// The path must name a directory.
pub const OPEN_DIRECTORY: u64 = 8;
/// A symbolic link at the end of the path is opened itself: the handle
/// answers `stat_full` and nothing else.
pub const OPEN_NOFOLLOW: u64 = 16;
/// The file is opened to be asked about and for nothing else, which takes
/// no permission over the file itself: only over the directories that lead
/// to it. The handle answers `stat_full` and `statfs_of`.
pub const OPEN_ASK: u64 = 0x800;
/// The file is opened as a *descriptor*: a number in this program's
/// descriptor table, which a forked child has a copy of and the program this
/// one execs keeps. See [`open_fd`].
pub const OPEN_DESCRIPTOR: u64 = 0x20;
/// Every write through the descriptor goes to the end of the file.
pub const OPEN_APPEND: u64 = 0x40;
/// What the descriptor may do.
pub const OPEN_READ: u64 = 0x80;
pub const OPEN_WRITE: u64 = 0x100;
/// Do not wait for what is opened. A named pipe opened to write with nobody
/// reading is then refused ([`ERR_NO_PEER`]), and one opened to read is
/// given at once.
pub const OPEN_NOWAIT: u64 = 0x200;
/// Set in a mode word to say the permission bits below it are meant.
pub const MODE_GIVEN: u64 = 1 << 16;

/// As `lseek`'s.
pub const SEEK_SET: u64 = 0;
pub const SEEK_CUR: u64 = 1;
pub const SEEK_END: u64 = 2;

/// Which of a file's attributes [`set_attr`] changes.
pub const ATTR_MODE: u64 = 1;
pub const ATTR_UID: u64 = 2;
pub const ATTR_GID: u64 = 4;
pub const ATTR_ATIME: u64 = 8;
pub const ATTR_MTIME: u64 = 16;
pub const ATTR_ATIME_NOW: u64 = 32;
pub const ATTR_MTIME_NOW: u64 = 64;

// Error codes (match VFS server)
pub const ERR_NOT_FOUND: u64 = 1;
pub const ERR_INVALID_HANDLE: u64 = 2;
pub const ERR_IO: u64 = 3;
pub const ERR_TOO_MANY_OPEN: u64 = 4;
pub const ERR_INVALID_PATH: u64 = 5;
pub const ERR_NOT_DIR: u64 = 6;
pub const ERR_IS_DIR: u64 = 7;
pub const ERR_PERMISSION: u64 = 8;
pub const ERR_READ_ONLY: u64 = 9;
pub const ERR_EXISTS: u64 = 10;
pub const ERR_NOT_EMPTY: u64 = 11;
pub const ERR_NOT_SUPPORTED: u64 = 12;
pub const ERR_NAME_TOO_LONG: u64 = 13;
pub const ERR_NO_SPACE: u64 = 14;
pub const ERR_LOOP: u64 = 15;
pub const ERR_WOULD_BLOCK: u64 = 16;
pub const ERR_DEADLOCK: u64 = 17;

/// `lock`'s kinds.
pub const LOCK_UNLOCK: u64 = 0;
pub const LOCK_SHARED: u64 = 1;
pub const LOCK_EXCLUSIVE: u64 = 2;
/// `lock`'s flags: wait to be granted; the lock is the handle's rather than
/// the program's; grant nothing and say what is in the way.
pub const LOCK_WAIT: u64 = 1;
pub const LOCK_OFD: u64 = 2;
pub const LOCK_QUERY: u64 = 4;
pub const ERR_TOO_MANY_LINKS: u64 = 18;
pub const ERR_NO_PEER: u64 = 19;
/// A disk somebody else is using, or the one the system is running from; a
/// mounted filesystem something still has open.
pub const ERR_BUSY: u64 = 20;
/// Two names in two filesystems, asked for as one file.
pub const ERR_CROSS_DEVICE: u64 = 21;
/// A signal the program handles ended the wait for the other end of a named
/// pipe. This side's own: the server never says it.
pub const ERR_INTERRUPTED: u64 = 254;

/// File-type bits of a mode, as [`Stat::mode`] carries them.
pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
/// A named pipe.
pub const S_IFIFO: u32 = 0o010000;

/// What [`open_with`] learns about the file it opened.
#[derive(Clone, Copy, Debug)]
pub struct Opened {
    pub handle: usize,
    pub size: u64,
    pub is_dir: bool,
    /// With the file-type bits.
    pub mode: u32,
    /// What this caller may do with it: 4 read, 2 write, 1 execute.
    pub access: u32,
    /// The inode number, stable while the file exists.
    pub id: u64,
}

/// A file's attributes, as `STAT` reports them.
#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub id: u64,
    pub size: u64,
    pub mode: u32,
    pub links: u32,
    pub uid: u32,
    pub gid: u32,
    /// Seconds since boot; see `docs/vfs.md`.
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    /// In 512-byte units.
    pub blocks: u64,
    pub block_size: u32,
}

/// Call the server with `path` lent, its length in `data[0]`.
fn call_with_path(vfs_tid: usize, tag: u64, path: &[u8], mut data: [u64; 6]) -> Result<Message, u64> {
    if path.is_empty() {
        return Err(ERR_INVALID_PATH);
    }
    if path.len() > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    data[0] = path.len() as u64;
    let msg = Message { sender: 0, tag, data };
    let mut reply = Message::empty();
    if syscall::sys_call_lend(vfs_tid, &msg, &mut reply, path).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(reply)
}

/// One entry of a directory, as a bulk read reports it.
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name: [u8; 255],
    pub name_len: usize,
    pub size: u64,
    pub is_dir: bool,
    /// The inode number (FAT32: the first cluster).
    pub id: u64,
    /// `DT_DIR`, `DT_REG`, `DT_LNK`, `DT_CHR` or `DT_UNKNOWN`.
    pub kind: u8,
}

pub const DT_UNKNOWN: u8 = 0;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;

impl DirEntry {
    pub const fn empty() -> Self {
        Self { name: [0u8; 255], name_len: 0, size: 0, is_dir: false, id: 0, kind: DT_UNKNOWN }
    }

    /// Return the name as a byte slice.
    pub fn name_bytes(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

/// What one bulk read returned: how many entries, where the next read should
/// start, and whether that is the end.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    pub count: usize,
    pub next: u64,
    pub end: bool,
}

/// What `STATFS` reports.
#[derive(Clone, Copy, Debug)]
pub struct FsStat {
    /// 0xEF53 for ext2 and ext4, 0x4d44 for FAT.
    pub magic: u64,
    pub block_size: u64,
    pub blocks: u64,
    pub free_blocks: u64,
    /// Free to anybody, not only the superuser.
    pub avail_blocks: u64,
    pub files: u64,
    pub free_files: u64,
    pub name_max: u64,
}

/// Open a file or directory by path, with `OPEN_*` flags.
pub fn open_with(vfs_tid: usize, path: &[u8], flags: u64) -> Result<Opened, u64> {
    opened_with(vfs_tid, path, flags, 0)
}

/// [`open_with`], for a file this may make: `mode` is the permission bits
/// the file has from the moment it exists. Made 0644 and changed afterwards,
/// a file is everybody's to read until the change — which is a long time
/// for a file of password hashes.
pub fn open_new(vfs_tid: usize, path: &[u8], flags: u64, mode: u32) -> Result<Opened, u64> {
    opened_with(vfs_tid, path, flags, MODE_GIVEN | (mode as u64 & 0o7777))
}

fn opened_with(vfs_tid: usize, path: &[u8], flags: u64, mode_word: u64) -> Result<Opened, u64> {
    let r = call_with_path(vfs_tid, TAG_OPEN, path, [0, flags, mode_word, 0, 0, 0])?;
    Ok(Opened {
        handle: r.data[0] as usize,
        size: r.data[1],
        is_dir: r.data[2] != 0,
        mode: r.data[3] as u32,
        access: r.data[4] as u32,
        id: r.data[5],
    })
}

/// Open a file as a descriptor, and return its number.
///
/// What comes back is in this program's descriptor table like a pipe is: read
/// and written with `sys_fd_read` and `sys_fd_write` from wherever it has got
/// to, copied with `sys_fd_dup`, closed with `sys_fd_close`, inherited by a
/// forked child and kept across an exec. `flags` are the `OPEN_*` ones, of
/// which [`OPEN_READ`] and [`OPEN_WRITE`] say what it is for; `mode` is the
/// permission bits for a file this makes.
///
/// A named pipe opened this way is an end of a pipe, and the open waits for
/// somebody to open the other end unless `flags` say [`OPEN_NOWAIT`].
pub fn open_fd(vfs_tid: usize, path: &[u8], flags: u64, mode: u32) -> Result<usize, u64> {
    let (fd, wait) = open_end(vfs_tid, path, flags, mode)?;
    if wait != 0 && flags & OPEN_NOWAIT == 0 {
        if let Err(signal) = syscall::sys_pipe_peer(fd, wait) {
            let _ = syscall::sys_fd_close(fd);
            return Err(if signal { ERR_INTERRUPTED } else { ERR_IO });
        }
    }
    Ok(fd)
}

/// [`open_fd`] without the wait: the descriptor, and for an end of a named
/// pipe whose other end nobody holds, what [`syscall::sys_pipe_peer`] takes
/// to wait for it. 0 for anything else.
pub fn open_end(vfs_tid: usize, path: &[u8], flags: u64, mode: u32) -> Result<(usize, u64), u64> {
    let words = [0, flags | OPEN_DESCRIPTOR, MODE_GIVEN | (mode as u64 & 0o7777), 0, 0, 0];
    let r = call_with_path(vfs_tid, TAG_OPEN, path, words)?;
    let fd = (r.data[0] & 0xFFFF_FFFF) as usize;
    // For a pipe the reply's second word is what to wait on, where a file's
    // size would be.
    let is_pipe = r.data[3] as u32 & S_IFMT == S_IFIFO && flags & (OPEN_READ | OPEN_WRITE) != 0;
    Ok((fd, if is_pipe { r.data[1] } else { 0 }))
}

/// Make a named pipe: a name two programs open to be given the two ends of
/// one pipe. `mode` is its permission bits.
pub fn mkfifo(vfs_tid: usize, path: &[u8], mode: u32) -> Result<(), u64> {
    let words = [0, (S_IFIFO | (mode & 0o7777)) as u64, 0, 0, 0, 0];
    call_with_path(vfs_tid, TAG_MKNOD, path, words).map(|_| ())
}

/// Name local socket `fd` (`syscall::sys_socket_local`) at `path`, which
/// must be free, with permission bits `mode`: the server makes the name, and
/// the kernel knows the socket by it. A name that is taken is `ERR_EXISTS`.
pub fn bind_local(vfs_tid: usize, path: &[u8], fd: usize, mode: u32) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_BIND, path, [0, fd as u64, (mode & 0o7777) as u64, 0, 0, 0]).map(|_| ())
}

/// Connect local socket `fd` to whatever listens at `path`: `ERR_NO_PEER`
/// if nothing does, `ERR_WOULD_BLOCK` if it has as many waiting as it has
/// room for.
pub fn connect_local(vfs_tid: usize, path: &[u8], fd: usize) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_CONNECT, path, [0, fd as u64, 0, 0, 0, 0]).map(|_| ())
}

/// The server's handle behind descriptor `fd`, if it is one of `vfs_tid`'s.
pub fn handle_of(vfs_tid: usize, fd: usize) -> Result<usize, u64> {
    match syscall::sys_fd_served(fd) {
        Ok((server, cookie)) if server == vfs_tid => Ok(cookie as usize),
        _ => Err(ERR_INVALID_HANDLE),
    }
}

/// Move a descriptor's position, and say where it now is.
pub fn seek(vfs_tid: usize, fd: usize, offset: i64, whence: u64) -> Result<u64, u64> {
    let handle = handle_of(vfs_tid, fd)?;
    simple_call(vfs_tid, TAG_SEEK, [handle as u64, offset as u64, whence, 0, 0, 0])
        .map(|r| r.data[0])
}

/// What a refusal means, to somebody reading it: the words a program puts
/// after a path. "Not found" for everything is how a file somebody may not
/// read came to look like a file that is not there.
pub fn why(code: u64) -> &'static str {
    match code {
        ERR_NOT_FOUND => "no such file or directory",
        ERR_PERMISSION => "permission denied",
        ERR_NOT_DIR => "not a directory",
        ERR_IS_DIR => "is a directory",
        ERR_EXISTS => "it is there already",
        ERR_NOT_EMPTY => "the directory is not empty",
        ERR_READ_ONLY => "the filesystem cannot be written",
        ERR_NO_SPACE => "no room left on the filesystem",
        ERR_NAME_TOO_LONG => "the name is too long",
        ERR_LOOP => "too many symbolic links",
        ERR_TOO_MANY_OPEN => "too many files are open",
        ERR_BUSY => "it is in use",
        ERR_CROSS_DEVICE => "that is on another filesystem",
        ERR_NOT_SUPPORTED => "that cannot be done to it",
        ERR_INVALID_PATH => "that is not a name a file can have",
        ERR_IO => "the disk could not be read or written",
        _ => "the file server refused",
    }
}

/// Change a file's mode, owner or times. `which` is a set of `ATTR_*` saying
/// which of the rest are meant.
pub fn set_attr(
    vfs_tid: usize,
    path: &[u8],
    which: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    atime: u64,
    mtime: u64,
) -> Result<(), u64> {
    if path.is_empty() {
        return Err(ERR_INVALID_PATH);
    }
    if path.len() > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    // The path, then the five words.
    let mut lent = [0u8; MAX_PATH + 40];
    lent[..path.len()].copy_from_slice(path);
    for (i, w) in [mode as u64, uid as u64, gid as u64, atime, mtime].iter().enumerate() {
        let at = path.len() + i * 8;
        lent[at..at + 8].copy_from_slice(&w.to_le_bytes());
    }
    let msg = Message {
        sender: 0,
        tag: TAG_SETATTR,
        data: [path.len() as u64, which, 0, 0, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call_lend(vfs_tid, &msg, &mut reply, &lent[..path.len() + 40]).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok(()) }
}

/// Open an existing file or directory by path.
/// Returns (handle, file_size, is_dir).
pub fn open(vfs_tid: usize, path: &[u8]) -> Result<(usize, u32, bool), u64> {
    let o = open_with(vfs_tid, path, 0)?;
    Ok((o.handle, o.size as u32, o.is_dir))
}

/// Make a directory.
pub fn mkdir(vfs_tid: usize, path: &[u8]) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_MKDIR, path, [0; 6]).map(|_| ())
}

/// Read from `offset` into `buf`, at most [`MAX_IO`] bytes of it, which the
/// VFS is lent for the call. Returns bytes actually read.
pub fn read(vfs_tid: usize, handle: usize, buf: &mut [u8], offset: u32) -> Result<u32, u64> {
    let len = buf.len().min(MAX_IO);
    let msg = Message {
        sender: 0,
        tag: TAG_READ,
        data: [handle as u64, 0, offset as u64, len as u64, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, &mut buf[..len]).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(reply.data[0] as u32)
}

/// Ask something of the device `handle` is open on: a `DEVCTL_*`. The answer
/// is the operation's — for [`DEVCTL_RESCAN`], how many volumes the disk now
/// has.
pub fn devctl(vfs_tid: usize, handle: usize, operation: u64) -> Result<u64, u64> {
    let msg = Message { sender: 0, tag: TAG_DEVCTL, data: [handle as u64, operation, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call(vfs_tid, &msg, &mut reply).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(reply.data[0])
}

/// Close an open file/directory handle.
pub fn close(vfs_tid: usize, handle: usize) -> Result<(), u64> {
    let msg = Message {
        sender: 0,
        tag: TAG_CLOSE,
        data: [handle as u64, 0, 0, 0, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call(vfs_tid, &msg, &mut reply).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(())
}

/// The entry of a directory at `index`, or None past its end.
pub fn readdir(vfs_tid: usize, handle: usize, index: u32) -> Result<Option<DirEntry>, u64> {
    let mut one = [DirEntry::empty(); 1];
    let page = readdir_bulk(vfs_tid, handle, index as u64, &mut one)?;
    Ok(if page.count == 1 { Some(one[0]) } else { None })
}

/// Read a directory's entries from `start`, as many as fit `out` and a page.
/// `Page::next` is where the next call should start.
pub fn readdir_bulk(vfs_tid: usize, handle: usize, start: u64, out: &mut [DirEntry]) -> Result<Page, u64> {
    let mut buf = [0u8; 4096];
    let msg = Message {
        sender: 0,
        tag: TAG_READDIR_BULK,
        data: [handle as u64, start, buf.len() as u64, 0, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, &mut buf).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    let used = (reply.data[0] as usize).min(buf.len());
    let mut page = Page { count: 0, next: reply.data[1], end: reply.data[2] != 0 };
    if used == 0 && !page.end {
        return Err(ERR_INVALID_PATH); // not even one entry fits a page
    }
    let mut at = 0;
    while at + 28 <= used && page.count < out.len() {
        let r = &buf[at..used];
        let word = |i: usize| u64::from_le_bytes(r[i..i + 8].try_into().unwrap());
        let reclen = u16::from_le_bytes([r[24], r[25]]) as usize;
        let len = r[27] as usize;
        if reclen < 28 + len || reclen > r.len() {
            return Err(ERR_IO);
        }
        let mut e = DirEntry::empty();
        e.name[..len].copy_from_slice(&r[28..28 + len]);
        e.name_len = len;
        e.id = word(0);
        e.size = word(16);
        e.kind = r[26];
        e.is_dir = r[26] == DT_DIR;
        out[page.count] = e;
        page.count += 1;
        page.next = word(8);
        at += reclen;
    }
    // Entries the server sent that `out` had no room for are read again.
    if at < used {
        page.end = false;
    }
    Ok(page)
}

/// What the root filesystem is and how full.
pub fn statfs(vfs_tid: usize) -> Result<FsStat, u64> {
    statfs_from(vfs_tid, 0)
}

fn statfs_from(vfs_tid: usize, word: u64) -> Result<FsStat, u64> {
    let mut rec = [0u8; 64];
    let msg = Message { sender: 0, tag: TAG_STATFS, data: [word, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, &mut rec).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    let w = |i: usize| u64::from_le_bytes(rec[i * 8..i * 8 + 8].try_into().unwrap());
    Ok(FsStat {
        magic: w(0),
        block_size: w(1),
        blocks: w(2),
        free_blocks: w(3),
        avail_blocks: w(4),
        files: w(5),
        free_files: w(6),
        name_max: w(7),
    })
}

/// Write at most [`MAX_IO`] bytes of `buf` at `offset`, lending them to the
/// VFS for the call. Returns bytes actually written.
pub fn write(vfs_tid: usize, handle: usize, buf: &[u8], offset: u32) -> Result<u32, u64> {
    let len = buf.len().min(MAX_IO);
    let msg = Message {
        sender: 0,
        tag: TAG_WRITE,
        data: [handle as u64, 0, offset as u64, len as u64, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call_lend(vfs_tid, &msg, &mut reply, &buf[..len]).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(reply.data[0] as u32)
}

/// Remove a file's name; the file goes with its last one.
pub fn unlink(vfs_tid: usize, path: &[u8]) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_UNLINK, path, [0; 6]).map(|_| ())
}

/// Remove an empty directory.
pub fn rmdir(vfs_tid: usize, path: &[u8]) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_RMDIR, path, [0; 6]).map(|_| ())
}

/// Give the file at `from` the name `to`, replacing whatever had it.
pub fn rename(vfs_tid: usize, from: &[u8], to: &[u8]) -> Result<(), u64> {
    two_paths(vfs_tid, TAG_RENAME, from, to, 0)
}

/// Give the file at `from` a second name, `to`. A symbolic link at `from` gets
/// the name itself.
pub fn link(vfs_tid: usize, from: &[u8], to: &[u8]) -> Result<(), u64> {
    two_paths(vfs_tid, TAG_LINK, from, to, 0)
}

/// Make `path` a symbolic link to `target`, which is kept as it is given.
pub fn symlink(vfs_tid: usize, target: &[u8], path: &[u8]) -> Result<(), u64> {
    two_paths(vfs_tid, TAG_SYMLINK, target, path, 0)
}

/// What the symbolic link at `path` says, as much as fits in `out`. The
/// answer is the target's whole length, which may be more than `out` holds.
pub fn readlink(vfs_tid: usize, path: &[u8], out: &mut [u8]) -> Result<usize, u64> {
    if path.is_empty() {
        return Err(ERR_INVALID_PATH);
    }
    if path.len() > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    // The path and the room for the answer, lent as one buffer.
    let room = out.len().min(4096);
    let mut both = [0u8; MAX_PATH + 4096];
    both[..path.len()].copy_from_slice(path);
    let msg = Message {
        sender: 0,
        tag: TAG_READLINK,
        data: [path.len() as u64, room as u64, 0, 0, 0, 0],
    };
    let mut reply = Message::empty();
    let lent = &mut both[..path.len() + room];
    if syscall::sys_call_lend_rw(vfs_tid, &msg, &mut reply, lent).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    let len = reply.data[0] as usize;
    let n = len.min(room);
    out[..n].copy_from_slice(&lent[path.len()..path.len() + n]);
    Ok(len)
}

/// Move this program into directory `path`. A relative path, here and in
/// every other call, starts from where the program is.
pub fn chdir(vfs_tid: usize, path: &[u8]) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_CHDIR, path, [0; 6]).map(|_| ())
}

/// Move this program into the directory open as `handle`.
pub fn fchdir(vfs_tid: usize, handle: usize) -> Result<(), u64> {
    simple_call(vfs_tid, TAG_FCHDIR, [handle as u64, 0, 0, 0, 0, 0]).map(|_| ())
}

/// Where this program is, written into `out`; the length is returned.
/// `ERR_NOT_FOUND` if the directory has been removed, and `ERR_NAME_TOO_LONG`
/// if `out` cannot hold the path.
pub fn getcwd(vfs_tid: usize, out: &mut [u8]) -> Result<usize, u64> {
    let mut path = [0u8; MAX_PATH + 1];
    let msg = Message { sender: 0, tag: TAG_GETCWD, data: [0; 6] };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, &mut path).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    let len = (reply.data[0] as usize).min(path.len());
    if len > out.len() {
        return Err(ERR_NAME_TOO_LONG);
    }
    out[..len].copy_from_slice(&path[..len]);
    Ok(len)
}

/// Start `child`, a program this one is making, in this program's directory.
/// Call it before starting the child.
pub fn give_cwd(vfs_tid: usize, child: usize) -> Result<(), u64> {
    // A directory is a descriptor, in the slot the kernel keeps for one, and
    // the child is given a copy of it like any other descriptor it starts
    // with. That copy is what *its* children inherit. A program that has
    // never moved has nothing there, and neither then has its child.
    let _ = syscall::sys_fd_dup(child, syscall::FD_CWD, syscall::FD_CWD);
    // And the record the server keeps by program, which is all a filesystem
    // with no directory handles has.
    simple_call(vfs_tid, TAG_GIVE_CWD, [child as u64, 0, 0, 0, 0, 0]).map(|_| ())
}

/// Take, drop or ask about a lock on bytes `start..start + len` (`len` 0: to
/// the end and beyond) of the file open as `handle`. A query answers
/// `[kind, start, len, holder]` of the first lock in the way, `kind` 0 if
/// none; anything else answers zeroes.
pub fn lock(
    vfs_tid: usize,
    handle: usize,
    kind: u64,
    start: u64,
    len: u64,
    flags: u64,
) -> Result<[u64; 4], u64> {
    let r = simple_call(vfs_tid, TAG_LOCK, [handle as u64, kind, start, len, flags, 0])?;
    Ok([r.data[0], r.data[1], r.data[2], r.data[3]])
}

/// A capability to map the file open as `handle`, granted into a slot of this
/// task's CSpace: read access, and write access through a shared mapping if
/// `write_shared` (the handle must be writable). Returns the slot and the
/// file's size; `syscall::sys_object_map` maps it.
pub fn map(vfs_tid: usize, handle: usize, write_shared: bool) -> Result<(usize, u64), u64> {
    let r = simple_call(vfs_tid, TAG_MAP, [handle as u64, write_shared as u64, 0, 0, 0, 0])?;
    Ok((r.data[0] as usize, r.data[1]))
}

fn simple_call(vfs_tid: usize, tag: u64, data: [u64; 6]) -> Result<Message, u64> {
    let msg = Message { sender: 0, tag, data };
    let mut reply = Message::empty();
    if syscall::sys_call(vfs_tid, &msg, &mut reply).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(reply)
}

/// What `path` is, without following a symbolic link at its end.
pub fn lstat(vfs_tid: usize, path: &[u8]) -> Result<Stat, u64> {
    let o = open_with(vfs_tid, path, OPEN_NOFOLLOW | OPEN_ASK)?;
    let st = stat_full(vfs_tid, o.handle);
    let _ = close(vfs_tid, o.handle);
    st
}

fn two_paths(vfs_tid: usize, tag: u64, from: &[u8], to: &[u8], extra: u64) -> Result<(), u64> {
    if from.is_empty() || to.is_empty() {
        return Err(ERR_INVALID_PATH);
    }
    if from.len() > MAX_PATH || to.len() > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    // Both lent in one buffer, one after the other.
    let mut both = [0u8; 2 * MAX_PATH];
    both[..from.len()].copy_from_slice(from);
    both[from.len()..from.len() + to.len()].copy_from_slice(to);
    let msg = Message {
        sender: 0,
        tag,
        data: [from.len() as u64, to.len() as u64, extra, 0, 0, 0],
    };
    let mut reply = Message::empty();
    if syscall::sys_call_lend(vfs_tid, &msg, &mut reply, &both[..from.len() + to.len()]).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(())
}

/// Mount the filesystem `server` serves on the directory `target`.
///
/// `server` is a file server this program started on a volume
/// (`vfs DRIVER VOLUME mount`), and so one it may hand on: the capability to
/// call it goes to the file server `target` is in, which stands between it
/// and everybody else from then on. `source` is what the mount is written
/// down as having come from, and `target` should be the whole path, since
/// that is written down too. Only root mounts.
pub fn mount(vfs_tid: usize, server: usize, source: &[u8], target: &[u8]) -> Result<(), u64> {
    if target.is_empty() || source.is_empty() {
        return Err(ERR_INVALID_PATH);
    }
    if source.len() > 200 || target.len() > 300 {
        return Err(ERR_NAME_TOO_LONG);
    }
    // The path, then what to write down: the source and the target, each
    // ended by a NUL.
    let mut lent = [0u8; 1024];
    let mut len = 0;
    for part in [target, source, b"\0", target, b"\0"] {
        lent[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    let record = len - target.len();
    let msg = Message {
        sender: 0,
        tag: TAG_ATTACH,
        data: [target.len() as u64, record as u64, server as u64, 0, 0, 0],
    };
    // The capability for the server goes with the request, for the length
    // of the call.
    let slot = syscall::mint_scratch(syscall::CAP_TYPE_ENDPOINT, server as u64, 0).map_err(|()| ERR_PERMISSION)?;
    let with = syscall::CallWith {
        buf: lent.as_ptr() as u64,
        len_access: len as u64 | syscall::LEND_READ,
        offer: slot as u64,
        ticks: 0,
    };
    let mut reply = Message::empty();
    let outcome = syscall::sys_call_with(vfs_tid, &msg, &mut reply, &with);
    let _ = syscall::sys_cap_delete(slot);
    if outcome != syscall::CallOutcome::Replied {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok(()) }
}

/// Take away the filesystem mounted on `target`; its server ends. Refused
/// ([`ERR_BUSY`]) while anything in it is open or anything is mounted in it.
pub fn unmount(vfs_tid: usize, target: &[u8]) -> Result<(), u64> {
    call_with_path(vfs_tid, TAG_DETACH, target, [0; 6]).map(|_| ())
}

/// One mounted filesystem, as [`mounted`] reports it.
#[derive(Clone, Copy, Debug)]
pub struct Mounted {
    /// How much of the buffer the record took: where the filesystem came
    /// from and where it is, each ended by a NUL ([`mount_record`]).
    pub len: usize,
    /// One of the `KIND_*`.
    pub kind: u64,
    /// The process that serves it.
    pub pid: u64,
}

/// The `index`th mounted filesystem, the root first and each followed by
/// what is mounted inside it; its record is written to `record`. `None`
/// past the last.
pub fn mounted(vfs_tid: usize, index: u64, record: &mut [u8]) -> Result<Option<Mounted>, u64> {
    let msg = Message { sender: 0, tag: TAG_MOUNTS, data: [index, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, record).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return if reply.data[0] == ERR_NOT_FOUND { Ok(None) } else { Err(reply.data[0]) };
    }
    Ok(Some(Mounted { len: (reply.data[0] as usize).min(record.len()), kind: reply.data[1], pid: reply.data[2] }))
}

/// A mount's record as its two paths: where the filesystem came from, and
/// where it is.
pub fn mount_record(record: &[u8]) -> (&[u8], &[u8]) {
    let mut parts = record.split(|&b| b == 0);
    (parts.next().unwrap_or(b""), parts.next().unwrap_or(b""))
}

/// What a kind of filesystem is called.
pub fn kind_name(kind: u64) -> &'static str {
    match kind {
        KIND_EXT2 => "ext2",
        KIND_EXT4 => "ext4",
        KIND_FAT => "vfat",
        KIND_TMPFS => "tmpfs",
        _ => "unknown",
    }
}

/// Write `/etc/mtab`: what is mounted, a line for each, as programs that
/// were written for Unix look for it — `mke2fs` reads it to refuse a disk
/// with a mounted filesystem. It is a file and not a view of the truth, so
/// whoever changes what is mounted writes it again, and `init` writes it at
/// boot, when whatever the last system left in it is wrong.
pub fn write_mtab(vfs_tid: usize) -> Result<(), u64> {
    let mut text = [0u8; 2048];
    let mut len = 0;
    let mut record = [0u8; 512];
    for index in 0.. {
        let Some(m) = mounted(vfs_tid, index, &mut record)? else { break };
        let (source, target) = mount_record(&record[..m.len]);
        let line = [source, b" ", target, b" ", kind_name(m.kind).as_bytes(), b" rw 0 0\n"];
        if len + line.iter().map(|p| p.len()).sum::<usize>() > text.len() {
            break;
        }
        for part in line {
            text[len..len + part.len()].copy_from_slice(part);
            len += part.len();
        }
    }
    let file = open_with(vfs_tid, b"/etc/mtab", OPEN_CREATE | OPEN_TRUNCATE)?;
    let mut at = 0;
    let mut result = Ok(());
    while at < len {
        match write(vfs_tid, file.handle, &text[at..len], at as u32) {
            Ok(n) if n > 0 => at += n as usize,
            Ok(_) => {
                result = Err(ERR_IO);
                break;
            }
            Err(code) => {
                result = Err(code);
                break;
            }
        }
    }
    let _ = close(vfs_tid, file.handle);
    result
}

/// Have everything written so far be on its disk: in the root filesystem and
/// in every one mounted. A write is answered before the filesystem has
/// recorded it for good — for a fiftieth of a second, or until something
/// else is changed — and this is how to wait for that.
pub fn sync(vfs_tid: usize) -> Result<(), u64> {
    simple_call(vfs_tid, TAG_SYNC, [0; 6]).map(|_| ())
}

/// What the filesystem holding the open file `handle` is and how full:
/// [`statfs`], for a file that may be in a mounted filesystem.
pub fn statfs_of(vfs_tid: usize, handle: usize) -> Result<FsStat, u64> {
    statfs_from(vfs_tid, handle as u64 + 1)
}

/// Make an open file `size` bytes long.
pub fn truncate(vfs_tid: usize, handle: usize, size: u64) -> Result<(), u64> {
    let msg = Message { sender: 0, tag: TAG_TRUNCATE, data: [handle as u64, size, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call(vfs_tid, &msg, &mut reply).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    Ok(())
}

/// Create a new file or directory, which must not exist, and open it.
/// Returns (handle, size=0, is_dir).
pub fn create(vfs_tid: usize, path: &[u8], is_dir: bool) -> Result<(usize, u32, bool), u64> {
    if is_dir {
        mkdir(vfs_tid, path)?;
        return open(vfs_tid, path);
    }
    let o = open_with(vfs_tid, path, OPEN_CREATE | OPEN_EXCLUSIVE)?;
    Ok((o.handle, 0, false))
}

/// Everything the server knows about an open file.
pub fn stat_full(vfs_tid: usize, handle: usize) -> Result<Stat, u64> {
    let mut rec = [0u8; 88];
    let msg = Message { sender: 0, tag: TAG_STAT, data: [handle as u64, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_mut(vfs_tid, &msg, &mut reply, &mut rec).is_err() {
        return Err(ERR_IO);
    }
    if reply.tag == TAG_ERROR {
        return Err(reply.data[0]);
    }
    let w = |i: usize| u64::from_le_bytes(rec[i * 8..i * 8 + 8].try_into().unwrap());
    Ok(Stat {
        id: w(0),
        size: w(1),
        mode: w(2) as u32,
        links: w(3) as u32,
        uid: w(4) as u32,
        gid: w(5) as u32,
        atime: w(6),
        mtime: w(7),
        ctime: w(8),
        blocks: w(9),
        block_size: w(10) as u32,
    })
}

/// An open file's size and whether it is a directory.
pub fn stat(vfs_tid: usize, handle: usize) -> Result<(u32, bool), u64> {
    let s = stat_full(vfs_tid, handle)?;
    Ok((s.size as u32, s.mode & S_IFMT == S_IFDIR))
}
