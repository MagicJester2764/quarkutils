//! Filesystems mounted in this one, and this one mounted in another.
//!
//! **A mount is a server.** Each mounted filesystem is served by a file
//! server of its own — this program again, started on another volume — and
//! the server whose directory it is mounted on stands between it and
//! everybody else. A path that walks into that directory is not finished
//! here: what is left of it goes to the other server, which answers, and
//! the answer is passed back. A file opened there is a handle here that
//! names a handle there, and reading it is asking the other server to read.
//!
//! So nothing outside the file servers knows there are mounts. A client
//! calls the server it always called, a descriptor is this server's, and a
//! program built before any of this reads a file on a mounted disk. What it
//! costs is a second call for whatever crosses, which is nothing beside the
//! disk.
//!
//! What it buys is what a server always buys. The code serving a FAT
//! partition somebody has just mounted is in another address space from the
//! code serving the root, holds one volume, and takes down nothing but
//! itself.
//!
//! The server above speaks *for* its callers: before a request it says whose
//! it is ([`TAG_IDENTITY`]), and the server below checks permissions as that
//! user. It believes this of exactly one caller, the one that adopted it
//! ([`TAG_ADOPT`]) — and nobody else can call it at all, because a mounted
//! filesystem's server has no name to be looked up by: whoever started it
//! offers the capability for it, with the request to mount it, to the
//! server it is to be mounted in, and that is the only copy given out.
//!
//! A server that stops answering is treated as one that has gone: a request
//! passed on waits a minute and no longer. A mounted filesystem takes down
//! nothing but itself, whichever way it fails.
//!
//! Two things are not as they are on a system where one kernel holds every
//! filesystem, and both are at the edge of a mount:
//!
//! - `..` at a mounted filesystem's root leads back out only where this
//!   server can see it coming: straight after the mount point in a path
//!   (`/mnt/..`), or first in a path that starts from that root. A path that
//!   goes down into the mount and climbs back out past its root stays at the
//!   root.
//! - A symbolic link in a mounted filesystem whose target begins with `/`
//!   is followed from that filesystem's root.

use crate::ext2::EXT2_ROOT_INO;
use crate::ext2_dir;
use crate::handles::{self, FsFileData, OpenFile};
use crate::protocol::*;
use crate::{
    error_reply, get_handle, get_sender_uid_gid, lend_in, lend_out, reply_count, reply_dirents,
    reply_opened, space_of, DiskState, CLIENT_BUF, PAGE_SIZE,
};
use quark_rt::ipc::Message;
use quark_rt::syscall::{CallOutcome, CallWith};
use quark_rt::{println, syscall};

pub const MAX_MOUNTS: usize = 8;
/// What is written down of a mount: where it came from and where it is.
const RECORD_MAX: usize = 512;

/// Where a request for another server is put together: two paths, or a path
/// and a page.
pub const RELAY_BUF: usize = 0x89_0000_0000;
pub const RELAY_PAGES: usize = 3;

fn relay() -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(RELAY_BUF as *mut u8, RELAY_PAGES * PAGE_SIZE) }
}

/// Something in a mounted filesystem, as a handle here holds it.
#[derive(Clone, Copy)]
pub struct Remote {
    /// Which mount.
    pub mount: u8,
    /// Its server's handle.
    pub handle: u64,
    /// The file's number there, which is what a lock on it is kept by.
    pub id: u64,
    /// The mounted filesystem's root directory.
    pub at_root: bool,
}

struct Mount {
    in_use: bool,
    /// Its server has gone. The entry stays, and everything through it
    /// fails, until it is detached.
    dead: bool,
    /// The directory it is mounted on.
    dir: u32,
    tid: usize,
    /// Where the capability for its server is.
    slot: usize,
    pid: u64,
    /// What its server calls its root, and what kind of filesystem it is.
    root_id: u64,
    kind: u64,
    /// Whose requests its server was last told it is answering.
    told: Option<Who>,
    record: [u8; RECORD_MAX],
    record_len: usize,
}

const NO_MOUNT: Mount = Mount {
    in_use: false,
    dead: false,
    dir: 0,
    tid: 0,
    slot: 0,
    pid: 0,
    root_id: 0,
    kind: 0,
    told: None,
    record: [0; RECORD_MAX],
    record_len: 0,
};

static mut MOUNTS: [Mount; MAX_MOUNTS] = [NO_MOUNT; MAX_MOUNTS];
/// How many are in use, so that a server with none looks at nothing.
static mut COUNT: usize = 0;

fn mounts() -> &'static mut [Mount; MAX_MOUNTS] {
    unsafe { &mut *core::ptr::addr_of_mut!(MOUNTS) }
}

fn count() -> usize {
    unsafe { COUNT }
}

/// The mount on directory `ino`, if there is one.
pub fn at(ino: u32) -> Option<usize> {
    if count() == 0 {
        return None;
    }
    mounts().iter().position(|m| m.in_use && m.dir == ino)
}

/// How long a mounted filesystem's server has to answer a request: a
/// minute. One page of a file, or one name, is the most a request is.
const RELAY_TICKS: u64 = 6000;
/// And how long something offered as a server has to say that it is one.
/// The real thing is waiting to be asked.
const ADOPT_TICKS: u64 = 30;

// ---------------------------------------------------------------------------
// This server, mounted in another
// ---------------------------------------------------------------------------

/// Whether this server was started to be mounted in another. The root was
/// not, and is nobody's to adopt: a server believes whoever adopts it about
/// whose requests it is passing on, and ends when they do.
static mut MOUNTABLE: bool = false;

/// This server is a filesystem to be mounted, not the root.
pub fn to_be_mounted() {
    unsafe { MOUNTABLE = true };
}

/// The server above: the one this filesystem is mounted in.
static mut PARENT_TID: usize = 0;
static mut PARENT_SPACE: u64 = 0;
/// Whose requests the server above is passing on. Nobody's, until it says.
static mut ACTING: (u32, u32) = (65534, 65534);
/// And the groups that caller is in besides its own.
static mut ACTING_GROUPS: ([u32; crate::who::MAX], usize) = ([0; crate::who::MAX], 0);

/// A caller as a server below is told of it: its user and group, and the
/// groups it is in besides.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Who {
    ids: (u32, u32),
    groups: [u32; crate::who::MAX],
    count: usize,
}

impl Who {
    fn of(sender: usize) -> Who {
        let mut groups = [0u32; crate::who::MAX];
        let count = crate::who::groups_of(sender, &mut groups);
        // Past the count is nothing, so that two tellings of one caller
        // compare the same.
        groups[count..].fill(0);
        Who { ids: get_sender_uid_gid(sender), groups, count }
    }
}

/// Tell mount `m`'s server whose requests come next.
fn tell(m: usize, who: &Who) -> Result<(), u64> {
    let msg = Message {
        sender: 0,
        tag: TAG_IDENTITY,
        data: [who.ids.0 as u64, who.ids.1 as u64, who.count as u64, 0, 0, 0],
    };
    let mut bytes = [0u8; crate::who::MAX * 4];
    for (i, group) in who.groups[..who.count].iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&group.to_le_bytes());
    }
    let lend = if who.count == 0 { Lend::Nothing } else { Lend::Out(&bytes[..who.count * 4]) };
    request(m, &msg, lend).map(|_| ())
}

/// What this server serves, for whoever asks what is mounted: `/dev/` and
/// the volume's name, a NUL, `/`, a NUL.
static mut SELF_RECORD: [u8; 40] = [0; 40];
static mut SELF_RECORD_LEN: usize = 0;
static mut SELF_ROOT: u64 = 0;
static mut SELF_KIND: u64 = 0;

/// Say what this server serves: a driver's volume, its root's id, its kind.
pub fn describe(driver: &[u8], volume: u64, root: u64, kind: u64) {
    let rec = unsafe { &mut *core::ptr::addr_of_mut!(SELF_RECORD) };
    let mut len = 0;
    let mut put = |bytes: &[u8]| {
        let n = bytes.len().min(rec.len() - len);
        rec[len..len + n].copy_from_slice(&bytes[..n]);
        len += n;
    };
    put(b"/dev/");
    put(&driver[..driver.len().min(16)]);
    if volume > 0 {
        put(b"p");
        if volume >= 10 {
            put(&[b'0' + (volume / 10 % 10) as u8]);
        }
        put(&[b'0' + (volume % 10) as u8]);
    }
    put(b"\0/\0");
    unsafe {
        SELF_RECORD_LEN = len;
        SELF_ROOT = root;
        SELF_KIND = kind;
    }
}

/// Whether `space` is the server above.
pub fn is_parent(space: u64) -> bool {
    space != 0 && space == unsafe { PARENT_SPACE }
}

/// Who `sender`'s request is for, if `sender` is the server above passing
/// somebody's on.
pub fn acting(sender: usize) -> Option<(u32, u32)> {
    let parent = unsafe { PARENT_TID };
    (parent != 0 && sender == parent).then(|| unsafe { ACTING })
}

/// And the groups that somebody is in besides their own.
pub fn acting_groups(sender: usize) -> Option<([u32; crate::who::MAX], usize)> {
    let parent = unsafe { PARENT_TID };
    (parent != 0 && sender == parent).then(|| unsafe { ACTING_GROUPS })
}

/// ADOPT: the caller is the server this filesystem is mounted in.
fn adopt(sender: usize) {
    if !unsafe { MOUNTABLE } {
        return error_reply(sender, ERR_PERMISSION);
    }
    if unsafe { PARENT_TID } != 0 {
        return error_reply(sender, ERR_BUSY);
    }
    let space = space_of(sender);
    if space == 0 {
        return error_reply(sender, ERR_PERMISSION);
    }
    // Told when it goes: a filesystem mounted in nothing is nobody's. One
    // that has gone already is not mounting anything.
    if syscall::sys_space_watch(space).is_err() {
        return error_reply(sender, ERR_PERMISSION);
    }
    unsafe {
        PARENT_TID = sender;
        PARENT_SPACE = space;
    }
    let read_only = crate::is_ext2() && crate::ext2_state().read_only;
    reply_opened(sender, [unsafe { SELF_ROOT }, unsafe { SELF_KIND }, read_only as u64, 0, 0, 0]);
}

/// Have every filesystem mounted here put what it was told to write on its
/// disk.
pub fn sync() {
    for m in 0..MAX_MOUNTS {
        if mounts()[m].in_use && !mounts()[m].dead {
            let msg = Message { sender: 0, tag: TAG_SYNC, data: [0; 6] };
            let _ = request(m, &msg, Lend::Nothing);
        }
    }
}

/// The server above has gone, or has said to stop: nothing more will ask.
fn end() -> ! {
    crate::commit_pending();
    let (driver, volume) = crate::disk::root();
    let _ = quark_rt::block::release(driver, volume);
    syscall::sys_exit();
}

/// A program has gone. If it was the server above, so does this.
pub fn program_gone(space: u64) {
    if is_parent(space) {
        end();
    }
}

/// RETIRE: let the volume go and end. Not while anything is open here, or
/// mounted here.
fn retire(sender: usize) {
    if sender != unsafe { PARENT_TID } {
        return error_reply(sender, ERR_PERMISSION);
    }
    if count() > 0 || handles::any() {
        return error_reply(sender, ERR_BUSY);
    }
    reply_opened(sender, [0; 6]);
    end();
}

/// PATH_OF: where an open directory is, from this filesystem's root.
fn path_of(disk: &DiskState, sender: usize, msg: &Message) {
    let path = match get_handle(msg.data[0] as usize, sender) {
        Some(file) => dir_path(disk, file),
        None => Err(ERR_INVALID_HANDLE),
    };
    match path {
        Ok(p) if syscall::sys_lent_write(sender, 0, p) == Ok(p.len()) => {
            reply_opened(sender, [p.len() as u64, 0, 0, 0, 0, 0])
        }
        Ok(_) => error_reply(sender, ERR_IO),
        Err(code) => error_reply(sender, code),
    }
}

/// The path of an open directory from this filesystem's root.
fn dir_path(disk: &DiskState, file: &OpenFile) -> Result<&'static [u8], u64> {
    if !file.is_dir {
        return Err(ERR_NOT_DIR);
    }
    match file.fs {
        FsFileData::Ext2 { inode_num } => ext2_dir::path_of(crate::ext2_state(), inode_num),
        FsFileData::Fat32 { first_cluster, .. } => crate::fat_dir_path(disk, first_cluster),
        FsFileData::DevDir => Ok(b"/dev"),
        FsFileData::Remote(r) => remote_path(&r),
        _ => Err(ERR_NOT_DIR),
    }
}

// ---------------------------------------------------------------------------
// Asking a mounted filesystem's server
// ---------------------------------------------------------------------------

/// What is lent with a request.
enum Lend<'a> {
    Nothing,
    /// To be read.
    Out(&'a [u8]),
    /// To be filled.
    In(&'a mut [u8]),
    Both(&'a mut [u8]),
}

/// Send mount `m`'s server a request and wait for its answer, which may be
/// an error. No answer at all means the server has gone.
fn send(m: usize, msg: &Message, lend: Lend) -> Result<Message, u64> {
    let mount = &mut mounts()[m];
    if !mount.in_use || mount.dead {
        return Err(ERR_IO);
    }
    let tid = mount.tid;
    let mut reply = Message::empty();
    let (at, len, access) = match lend {
        Lend::Nothing => (0, 0, 0),
        Lend::Out(buf) => (buf.as_ptr() as u64, buf.len(), syscall::LEND_READ),
        Lend::In(buf) => (buf.as_ptr() as u64, buf.len(), syscall::LEND_WRITE),
        Lend::Both(buf) => (buf.as_ptr() as u64, buf.len(), syscall::LEND_READ | syscall::LEND_WRITE),
    };
    // Nothing to lend is nothing lent: a buffer of no length is not one the
    // kernel will lend, and a call that could not be made is not a server
    // that has gone.
    let with = CallWith {
        buf: if len == 0 { 0 } else { at },
        len_access: if len == 0 { 0 } else { len as u64 | access },
        ticks: RELAY_TICKS,
        ..CallWith::PLAIN
    };
    match syscall::sys_call_with(tid, msg, &mut reply, &with) {
        CallOutcome::Replied => Ok(reply),
        gone => {
            mount.dead = true;
            println!(
                "[vfs] the server of a mounted filesystem {} (pid {})",
                if gone == CallOutcome::TimedOut { "has stopped answering" } else { "has gone" },
                mount.pid
            );
            Err(ERR_IO)
        }
    }
}

/// [`send`], with an error for an answer made an error.
fn request(m: usize, msg: &Message, lend: Lend) -> Result<Message, u64> {
    let reply = send(m, msg, lend)?;
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok(reply) }
}

/// [`request`], for `sender`: the server is told whose request it is first,
/// if that is not who it was last told.
fn request_for(sender: usize, m: usize, msg: &Message, lend: Lend) -> Result<Message, u64> {
    let who = Who::of(sender);
    if mounts()[m].told != Some(who) {
        tell(m, &who)?;
        mounts()[m].told = Some(who);
    }
    request(m, msg, lend)
}

/// An id from mount `m`, made one that no file here has and no file in
/// another mount: the mounts a file is under are written above the fortieth
/// bit, four bits to a mount, innermost last.
fn fold(m: usize, id: u64) -> u64 {
    const LOW: u64 = (1 << 40) - 1;
    (id & LOW) | ((((id >> 40) << 4) | (m as u64 + 1)) << 40)
}

/// The handle another server gave has no more use here.
pub fn closed(r: &Remote) {
    let msg = Message { sender: 0, tag: TAG_CLOSE, data: [r.handle, 0, 0, 0, 0, 0] };
    let _ = send(r.mount as usize, &msg, Lend::Nothing);
}

/// How long a file in a mounted filesystem is.
pub fn size_of(r: &Remote) -> Result<u64, u64> {
    let mut record = [0u8; STAT_LEN];
    let msg = Message { sender: 0, tag: TAG_STAT, data: [r.handle, 0, 0, 0, 0, 0] };
    request(r.mount as usize, &msg, Lend::In(&mut record))?;
    Ok(u64::from_le_bytes(record[8..16].try_into().unwrap_or([0; 8])))
}

/// The directory a mounted filesystem's root covers: where `..` from that
/// root starts. For anything else in a mount, the root here — a path that
/// starts from it and is not absolute has already gone to the other server.
pub fn covered(r: &Remote) -> u32 {
    let mount = &mounts()[r.mount as usize];
    if r.at_root && mount.in_use { mount.dir } else { EXT2_ROOT_INO }
}

/// The directory `sender` is in, if that is in a mounted filesystem.
pub fn cwd_remote(sender: usize) -> Option<Remote> {
    if count() == 0 {
        return None;
    }
    let cookie = syscall::sys_fd_cookie(sender, syscall::FD_CWD)?;
    match handles::descriptor(cookie as usize) {
        Some(file) if file.is_dir => match file.fs {
            FsFileData::Remote(r) => Some(r),
            _ => None,
        },
        _ => None,
    }
}

/// The path of a directory in a mounted filesystem: where the filesystem is
/// mounted, then where its server says the directory is.
pub fn remote_path(r: &Remote) -> Result<&'static [u8], u64> {
    let m = r.mount as usize;
    if !mounts()[m].in_use {
        return Err(ERR_NOT_FOUND);
    }
    let here = ext2_dir::path_of(crate::ext2_state(), mounts()[m].dir)?;
    let buf = relay();
    let n = here.len();
    buf[..n].copy_from_slice(here);
    let ask = Message { sender: 0, tag: TAG_PATH_OF, data: [r.handle, 0, 0, 0, 0, 0] };
    let reply = request(m, &ask, Lend::In(&mut buf[n..n + MAX_PATH + 1]))?;
    let there = (reply.data[0] as usize).min(MAX_PATH + 1);
    // Its root is the directory it is mounted on, by that directory's name.
    if there <= 1 {
        return Ok(&buf[..n]);
    }
    if n + there > MAX_PATH {
        return Err(ERR_NAME_TOO_LONG);
    }
    Ok(&buf[..n + there])
}

// ---------------------------------------------------------------------------
// Where a path leads
// ---------------------------------------------------------------------------

/// Where a path that leaves this filesystem goes: the mount, what the path
/// starts from there (0 for its root or the caller's place, else a handle
/// and one), and how long the path is as that server should be given it.
struct Away {
    mount: usize,
    base: u64,
    len: usize,
}

enum Start {
    Local(u32),
    Remote(Remote),
}

fn start_of(sender: usize, word: u64) -> Option<Start> {
    if word == 0 {
        return Some(match cwd_remote(sender) {
            Some(r) => Start::Remote(r),
            None => Start::Local(crate::local_cwd(sender)),
        });
    }
    let file = get_handle((word - 1) as usize, sender)?;
    if !file.is_dir {
        return None;
    }
    match file.fs {
        FsFileData::Remote(r) => Some(Start::Remote(r)),
        FsFileData::Ext2 { inode_num } => Some(Start::Local(inode_num)),
        FsFileData::DevDir if ext2_dir::dev_dir() != 0 => Some(Start::Local(ext2_dir::dev_dir())),
        _ => None,
    }
}

/// Find out whether one of a request's paths leaves this filesystem.
///
/// The path is `len` bytes at `lent_at` in what `sender` lent, and starts
/// from `base_word`. If it leads into a mount, what the mount's server
/// should be given of it is written at `at` in the relay buffer. `Ok(None)`
/// is a path that stays here, or one the request's own handler will have
/// something to say about; an error is one that could only have gone to
/// another server and cannot.
fn locate(
    sender: usize,
    base_word: u64,
    lent_at: usize,
    len: usize,
    scratch: usize,
    at: usize,
    follow: bool,
    cross_last: bool,
) -> Result<Option<Away>, u64> {
    let start = start_of(sender, base_word);
    let from_mount = matches!(start, Some(Start::Remote(_)));
    let path = match lent_path(sender, lent_at, len, scratch) {
        Ok(path) => path,
        // A handler here would start it from the wrong place.
        Err(code) if from_mount => return Err(code),
        Err(_) => return Ok(None),
    };
    let buf = relay();
    let base = match start {
        Some(Start::Remote(r)) if path[0] != b'/' && !(r.at_root && ext2_dir::goes_back(path)) => {
            buf[at..at + path.len()].copy_from_slice(path);
            return Ok(Some(Away { mount: r.mount as usize, base: r.handle + 1, len: path.len() }));
        }
        // `..` from the top of a mounted filesystem starts at the directory
        // it is mounted on; an absolute path starts here whatever it is from.
        Some(Start::Remote(r)) => covered(&r),
        Some(Start::Local(ino)) => ino,
        None => return Ok(None),
    };
    let (uid, gid) = get_sender_uid_gid(sender);
    match ext2_dir::resolve_to(crate::ext2_state(), base, path, uid, gid, follow, cross_last) {
        Err(ERR_ELSEWHERE) => {
            let (mount, rest) = ext2_dir::crossing();
            // Nothing left is the mounted filesystem's root.
            let rest = if rest.is_empty() { &b"/"[..] } else { rest };
            buf[at..at + rest.len()].copy_from_slice(rest);
            Ok(Some(Away { mount, base: 0, len: rest.len() }))
        }
        _ => Ok(None),
    }
}

/// Whether a path given to a mount's server names that filesystem's root.
fn is_root(away: &Away, at: usize) -> bool {
    away.base == 0 && relay()[at..at + away.len].iter().all(|&b| b == b'/')
}

// ---------------------------------------------------------------------------
// Requests that name a path
// ---------------------------------------------------------------------------

/// Take a request before its handler does, if it is one for another server
/// or about one. True if it was answered.
pub(crate) fn intercept(disk: &DiskState, sender: usize, msg: &Message) -> bool {
    // What one server says to another, and what is said about mounts.
    match msg.tag {
        TAG_ADOPT => adopt(sender),
        TAG_IDENTITY if sender == unsafe { PARENT_TID } => {
            // The groups are lent, four bytes each: more than a message has
            // words for.
            let mut groups = [0u32; crate::who::MAX];
            let count = (msg.data[2] as usize).min(crate::who::MAX);
            let mut bytes = [0u8; crate::who::MAX * 4];
            let got = count == 0 || syscall::sys_lent_read(sender, 0, &mut bytes[..count * 4]) == Ok(count * 4);
            if !got {
                return {
                    error_reply(sender, ERR_IO);
                    true
                };
            }
            for (i, group) in groups[..count].iter_mut().enumerate() {
                *group = u32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
            }
            unsafe {
                ACTING = (msg.data[0] as u32, msg.data[1] as u32);
                ACTING_GROUPS = (groups, count);
            }
            reply_opened(sender, [0; 6]);
        }
        TAG_IDENTITY => error_reply(sender, ERR_PERMISSION),
        TAG_RETIRE => retire(sender),
        TAG_PATH_OF => path_of(disk, sender, msg),
        TAG_MOUNTS => list(sender, msg),
        // A FAT filesystem has no directory that could say what is on it.
        TAG_ATTACH | TAG_DETACH if !crate::is_ext2() => error_reply(sender, ERR_NOT_SUPPORTED),
        // With nothing mounted every path is this filesystem's.
        TAG_ATTACH if count() == 0 => attach(sender, msg),
        TAG_DETACH if count() == 0 => detach(sender, msg),
        _ if count() == 0 || !crate::is_ext2() => return false,
        _ => return crossing(sender, msg),
    }
    true
}

/// A request while something is mounted here: answer it if it crosses.
fn crossing(sender: usize, msg: &Message) -> bool {
    let len = msg.data[0] as usize;
    let one = |follow: bool, cross_last: bool| locate(sender, msg.data[5], 0, len, 0, 0, follow, cross_last);
    let buf = relay();
    let found = match msg.tag {
        TAG_OPEN => one(msg.data[1] & OPEN_NOFOLLOW == 0, true),
        TAG_CHDIR => one(true, true),
        TAG_MKDIR | TAG_MKNOD | TAG_UNLINK | TAG_RMDIR | TAG_READLINK => one(false, true),
        TAG_SETATTR if len != 0 => one(msg.data[2] == 0, true),
        TAG_ATTACH | TAG_DETACH => one(true, false),
        TAG_RENAME | TAG_LINK => return two(sender, msg),
        // The target is only text; the new name is the path.
        TAG_SYMLINK => locate(sender, msg.data[5], len, msg.data[1] as usize, 4096, len.min(MAX_PATH), false, true),
        TAG_SETATTR | TAG_FCHDIR | TAG_STATFS | TAG_MAP => return by_handle(sender, msg),
        _ => return false,
    };
    let away = match found {
        Ok(Some(away)) => away,
        Ok(None) => {
            // Here after all — and these two are answered here.
            match msg.tag {
                TAG_ATTACH => attach(sender, msg),
                TAG_DETACH => detach(sender, msg),
                _ => return false,
            }
            return true;
        }
        Err(code) => {
            error_reply(sender, code);
            return true;
        }
    };
    let m = away.mount;
    let n = away.len;
    let words: [u64; 6] = match msg.tag {
        TAG_OPEN => {
            open_there(sender, msg, &away);
            return true;
        }
        TAG_CHDIR => {
            enter(sender, &away);
            return true;
        }
        // The top of a mounted filesystem is there, and is in use.
        TAG_MKDIR | TAG_MKNOD if is_root(&away, 0) => {
            error_reply(sender, ERR_EXISTS);
            return true;
        }
        TAG_UNLINK | TAG_RMDIR if is_root(&away, 0) => {
            error_reply(sender, ERR_BUSY);
            return true;
        }
        // A named pipe's ends are kept by the kernel for the server its name
        // is in, and a descriptor for one there could not be given here.
        TAG_MKNOD => {
            error_reply(sender, ERR_NOT_SUPPORTED);
            return true;
        }
        TAG_MKDIR => [n as u64, msg.data[1], 0, 0, 0, away.base],
        TAG_UNLINK | TAG_RMDIR | TAG_DETACH => [n as u64, 0, 0, 0, 0, away.base],
        TAG_READLINK => {
            read_link_there(sender, msg, &away);
            return true;
        }
        TAG_SETATTR => {
            if syscall::sys_lent_read(sender, len, &mut buf[n..n + ATTR_LEN]) != Ok(ATTR_LEN) {
                error_reply(sender, ERR_INVALID_PATH);
                return true;
            }
            return pass(sender, m, msg.tag, [n as u64, msg.data[1], msg.data[2], 0, 0, away.base], n + ATTR_LEN);
        }
        TAG_SYMLINK => {
            // A target is text of any length a name can be, and no more.
            if len == 0 || len > MAX_PATH {
                error_reply(sender, if len == 0 { ERR_INVALID_PATH } else { ERR_NAME_TOO_LONG });
                return true;
            }
            if is_root(&away, len) {
                error_reply(sender, ERR_EXISTS);
                return true;
            }
            // The target goes in front of the path, as it was lent.
            if syscall::sys_lent_read(sender, 0, &mut buf[..len]) != Ok(len) {
                error_reply(sender, ERR_INVALID_PATH);
                return true;
            }
            return pass(sender, m, msg.tag, [len as u64, n as u64, 0, 0, 0, away.base], len + n);
        }
        TAG_ATTACH => {
            attach_there(sender, msg, &away);
            return true;
        }
        _ => return false,
    };
    pass(sender, m, msg.tag, words, n)
}

/// Send a request whose lent bytes are the first `lent` of the relay buffer
/// to mount `m`'s server, and answer `sender` with what it says.
fn pass(sender: usize, m: usize, tag: u64, words: [u64; 6], lent: usize) -> bool {
    let msg = Message { sender: 0, tag, data: words };
    match request_for(sender, m, &msg, Lend::Out(&relay()[..lent])) {
        Ok(reply) => reply_opened(sender, reply.data),
        Err(code) => error_reply(sender, code),
    }
    true
}

/// RENAME and LINK: two paths, which have to lead to one filesystem.
fn two(sender: usize, msg: &Message) -> bool {
    let (first, second) = (msg.data[0] as usize, msg.data[1] as usize);
    let follow = msg.tag == TAG_LINK && msg.data[2] & LINK_FOLLOW != 0;
    let from = match locate(sender, msg.data[5], 0, first, 0, 0, follow, true) {
        Ok(found) => found,
        Err(code) => {
            error_reply(sender, code);
            return true;
        }
    };
    // After the first, wherever that ended up being put.
    let at = from.as_ref().map_or(0, |a| a.len);
    let to = match locate(sender, msg.data[4], first, second, 4096, at, false, true) {
        Ok(found) => found,
        Err(code) => {
            error_reply(sender, code);
            return true;
        }
    };
    match (from, to) {
        (None, None) => false,
        (Some(from), Some(to)) if from.mount == to.mount => {
            if is_root(&from, 0) || is_root(&to, at) {
                error_reply(sender, ERR_BUSY);
                return true;
            }
            let words = [from.len as u64, to.len as u64, msg.data[2], 0, to.base, from.base];
            pass(sender, from.mount, msg.tag, words, from.len + to.len)
        }
        _ => {
            error_reply(sender, ERR_CROSS_DEVICE);
            true
        }
    }
}

/// OPEN, of something in a mounted filesystem: its server opens it, and a
/// handle here stands for the one it gives.
fn open_there(sender: usize, msg: &Message, away: &Away) {
    let flags = msg.data[1];
    // The descriptor, if one is wanted, is this server's to make. A handle a
    // program holds for itself is one it reads through, and is checked as
    // that.
    let theirs = if flags & OPEN_DESCRIPTOR != 0 {
        (flags & !OPEN_DESCRIPTOR) | OPEN_PROXIED
    } else if asks(flags) {
        // Only to be asked about: nothing of it will be read.
        flags | OPEN_PROXIED
    } else {
        flags | OPEN_READ | OPEN_PROXIED
    };
    let ask = Message {
        sender: 0,
        tag: TAG_OPEN,
        data: [away.len as u64, theirs, msg.data[2], 0, 0, away.base],
    };
    let r = match request_for(sender, away.mount, &ask, Lend::Out(&relay()[..away.len])) {
        Ok(r) => r,
        Err(code) => return error_reply(sender, code),
    };
    let [handle, size, is_dir, mode, access, id] = r.data;
    // An end of a named pipe is a descriptor the kernel keeps for the server
    // the pipe's name is in, and that server is not the one being asked.
    if mode & 0o170000 == 0o010000 && flags & OPEN_DESCRIPTOR != 0 && flags & (OPEN_READ | OPEN_WRITE) != 0 {
        closed(&Remote { mount: away.mount as u8, handle, id, at_root: false });
        return error_reply(sender, ERR_NOT_SUPPORTED);
    }
    let remote = Remote {
        mount: away.mount as u8,
        handle,
        id,
        at_root: is_dir != 0 && id == mounts()[away.mount].root_id,
    };
    let file = OpenFile {
        in_use: true,
        owner: space_of(sender),
        is_dir: is_dir != 0,
        writable: access & 2 != 0 && !asks(flags),
        link: mode & 0o170000 == 0o120000 || asks(flags),
        fs: FsFileData::Remote(remote),
        ..OpenFile::empty()
    };
    // If no handle comes of it, the table closes the one over there.
    crate::opened(sender, flags, file, [0, size, is_dir, mode, access, fold(away.mount, id)]);
}

/// CHDIR into a mounted filesystem: the directory is opened there, and what
/// stands for it here becomes the caller's working directory.
fn enter(sender: usize, away: &Away) {
    let ask = Message {
        sender: 0,
        tag: TAG_OPEN,
        data: [away.len as u64, OPEN_DIRECTORY | OPEN_PROXIED, 0, 0, 0, away.base],
    };
    let r = match request_for(sender, away.mount, &ask, Lend::Out(&relay()[..away.len])) {
        Ok(r) => r,
        Err(code) => return error_reply(sender, code),
    };
    let remote = Remote {
        mount: away.mount as u8,
        handle: r.data[0],
        id: r.data[5],
        at_root: r.data[5] == mounts()[away.mount].root_id,
    };
    // A directory to be in is one the caller may search.
    if r.data[4] & 1 == 0 {
        closed(&remote);
        return error_reply(sender, ERR_PERMISSION);
    }
    let dir = OpenFile {
        in_use: true,
        by_fd: true,
        is_dir: true,
        may_write: false,
        fs: FsFileData::Remote(remote),
        ..OpenFile::empty()
    };
    let Some(handle) = handles::alloc(dir) else {
        return error_reply(sender, ERR_TOO_MANY_OPEN);
    };
    if syscall::sys_fd_serve(sender, handle as u64, syscall::FD_CWD).is_err() {
        let _ = handles::release(handle);
        return error_reply(sender, ERR_TOO_MANY_OPEN);
    }
    reply_opened(sender, [0; 6]);
    crate::left_directory(space_of(sender));
}

/// READLINK of a link in a mounted filesystem: the answer comes back after
/// the path in what was lent to its server, and goes after the path in what
/// the caller lent.
fn read_link_there(sender: usize, msg: &Message, away: &Away) {
    let room = (msg.data[1] as usize).min(PAGE_SIZE);
    let n = away.len;
    let ask = Message { sender: 0, tag: TAG_READLINK, data: [n as u64, room as u64, 0, 0, 0, away.base] };
    let buf = relay();
    let reply = match request_for(sender, away.mount, &ask, Lend::Both(&mut buf[..n + room])) {
        Ok(r) => r,
        Err(code) => return error_reply(sender, code),
    };
    let fits = (reply.data[0] as usize).min(room);
    if fits > 0 && syscall::sys_lent_write(sender, msg.data[0] as usize, &buf[n..n + fits]) != Ok(fits) {
        return error_reply(sender, ERR_IO);
    }
    reply_opened(sender, reply.data);
}

// ---------------------------------------------------------------------------
// Requests that name a handle
// ---------------------------------------------------------------------------

/// The mounted filesystem's file a handle of `sender`'s stands for.
fn remote_of(sender: usize, handle: u64) -> Option<(Remote, bool)> {
    let file = get_handle(handle as usize, sender)?;
    match file.fs {
        FsFileData::Remote(r) => Some((r, file.is_dir)),
        _ => None,
    }
}

/// SETATTR, FCHDIR, STATFS and MAP on a handle that stands for something in
/// a mounted filesystem. True if it was one.
fn by_handle(sender: usize, msg: &Message) -> bool {
    let handle = match msg.tag {
        TAG_FCHDIR | TAG_MAP => msg.data[0],
        TAG_SETATTR => msg.data[5].wrapping_sub(1),
        // 0 is this server's own filesystem.
        TAG_STATFS if msg.data[0] != 0 => msg.data[0] - 1,
        _ => return false,
    };
    let Some((r, is_dir)) = remote_of(sender, handle) else {
        return false;
    };
    let m = r.mount as usize;
    let buf = relay();
    match msg.tag {
        TAG_FCHDIR if !is_dir => error_reply(sender, ERR_NOT_DIR),
        TAG_FCHDIR => {
            buf[0] = b'.';
            enter(sender, &Away { mount: m, base: r.handle + 1, len: 1 });
        }
        TAG_SETATTR => {
            if syscall::sys_lent_read(sender, 0, &mut buf[..ATTR_LEN]) != Ok(ATTR_LEN) {
                error_reply(sender, ERR_INVALID_PATH);
            } else {
                pass(sender, m, TAG_SETATTR, [0, msg.data[1], msg.data[2], 0, 0, r.handle + 1], ATTR_LEN);
            }
        }
        TAG_STATFS => {
            let ask = Message { sender: 0, tag: TAG_STATFS, data: [r.handle + 1, 0, 0, 0, 0, 0] };
            match request_for(sender, m, &ask, Lend::In(&mut buf[..STATFS_LEN])) {
                Ok(reply) if syscall::sys_lent_write(sender, 0, &buf[..STATFS_LEN]) == Ok(STATFS_LEN) => {
                    reply_opened(sender, reply.data)
                }
                Ok(_) => error_reply(sender, ERR_IO),
                Err(code) => error_reply(sender, code),
            }
        }
        // A mapping is a memory object of this server's, filled from its own
        // disk; there is none for a file another server holds.
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
    true
}

/// Whether `msg` names a handle that stands for something in a mounted
/// filesystem.
pub fn is_ours(sender: usize, msg: &Message) -> bool {
    count() != 0 && remote_of(sender, msg.data[0]).is_some()
}

/// READ, WRITE, STAT, READDIR_BULK and TRUNCATE on a handle [`is_ours`] said
/// is ours: the mounted filesystem's server does it, through the page data
/// passes through here.
pub fn serve(sender: usize, msg: &Message) {
    let Some(file) = get_handle(msg.data[0] as usize, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let FsFileData::Remote(r) = file.fs else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let may_write = !file.by_fd || file.may_write;
    let m = r.mount as usize;
    let page = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, PAGE_SIZE) };
    match msg.tag {
        TAG_READ => {
            let len = (msg.data[3] as usize).min(PAGE_SIZE);
            let ask = Message { sender: 0, tag: TAG_READ, data: [r.handle, 0, msg.data[2], len as u64, 0, 0] };
            match request_for(sender, m, &ask, Lend::In(&mut page[..len])) {
                Ok(reply) => {
                    let n = (reply.data[0] as usize).min(len);
                    if lend_out(sender, n) { reply_count(sender, n as u64) } else { error_reply(sender, ERR_IO) }
                }
                Err(code) => error_reply(sender, code),
            }
        }
        TAG_WRITE => {
            let len = (msg.data[3] as usize).min(PAGE_SIZE);
            if !lend_in(sender, len) {
                return error_reply(sender, ERR_IO);
            }
            let ask = Message { sender: 0, tag: TAG_WRITE, data: [r.handle, 0, msg.data[2], len as u64, 0, 0] };
            match request_for(sender, m, &ask, Lend::Out(&page[..len])) {
                Ok(reply) => reply_count(sender, reply.data[0]),
                Err(code) => error_reply(sender, code),
            }
        }
        TAG_STAT => {
            let ask = Message { sender: 0, tag: TAG_STAT, data: [r.handle, 0, 0, 0, 0, 0] };
            match request_for(sender, m, &ask, Lend::In(&mut page[..STAT_LEN])) {
                Ok(_) => {
                    let id = u64::from_le_bytes(page[..8].try_into().unwrap_or([0; 8]));
                    page[..8].copy_from_slice(&fold(m, id).to_le_bytes());
                    match syscall::sys_lent_write(sender, 0, &page[..STAT_LEN]) {
                        Ok(n) if n == STAT_LEN => reply_opened(sender, [STAT_LEN as u64, 0, 0, 0, 0, 0]),
                        _ => error_reply(sender, ERR_IO),
                    }
                }
                Err(code) => error_reply(sender, code),
            }
        }
        TAG_READDIR_BULK => {
            let room = (msg.data[2] as usize).min(PAGE_SIZE);
            let ask = Message { sender: 0, tag: TAG_READDIR_BULK, data: [r.handle, msg.data[1], room as u64, 0, 0, 0] };
            match request_for(sender, m, &ask, Lend::In(&mut page[..room])) {
                Ok(reply) => {
                    let used = (reply.data[0] as usize).min(room);
                    // Each entry's id, as STAT would give it.
                    let mut at = 0;
                    while at + DIRENT_HEADER <= used {
                        let id = u64::from_le_bytes(page[at..at + 8].try_into().unwrap_or([0; 8]));
                        page[at..at + 8].copy_from_slice(&fold(m, id).to_le_bytes());
                        let reclen = u16::from_le_bytes([page[at + 24], page[at + 25]]) as usize;
                        if reclen < DIRENT_HEADER {
                            break;
                        }
                        at += reclen;
                    }
                    reply_dirents(sender, used, reply.data[1], reply.data[2] != 0)
                }
                Err(code) => error_reply(sender, code),
            }
        }
        TAG_TRUNCATE if !may_write => error_reply(sender, ERR_INVALID_HANDLE),
        TAG_TRUNCATE => {
            let ask = Message { sender: 0, tag: TAG_TRUNCATE, data: [r.handle, msg.data[1], 0, 0, 0, 0] };
            match request_for(sender, m, &ask, Lend::Nothing) {
                Ok(_) => reply_opened(sender, [0; 6]),
                Err(code) => error_reply(sender, code),
            }
        }
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
}

// ---------------------------------------------------------------------------
// Mounting
// ---------------------------------------------------------------------------

/// ATTACH, for a directory of this filesystem: the server on offer becomes
/// the filesystem there.
fn attach(sender: usize, msg: &Message) {
    match attach_here(sender, msg) {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

fn attach_here(sender: usize, msg: &Message) -> Result<(), u64> {
    let (uid, gid) = get_sender_uid_gid(sender);
    if uid != 0 {
        return Err(ERR_PERMISSION);
    }
    let (len, record_len, tid) = (msg.data[0] as usize, msg.data[1] as usize, msg.data[2] as usize);
    if record_len == 0 || record_len > RECORD_MAX {
        return Err(ERR_INVALID_PATH);
    }
    let path = lent_path(sender, 0, len, 0)?;
    let base = crate::base_of(sender, msg.data[5])?;
    let dir = match ext2_dir::resolve_to(crate::ext2_state(), base, path, uid, gid, true, false)? {
        ext2_dir::Found::Inode(ino, inode, _) if inode.is_dir() => ino,
        _ => return Err(ERR_NOT_DIR),
    };
    // One filesystem to a directory, and not on the root or on /dev, which
    // are this server's own.
    if dir == EXT2_ROOT_INO || dir == ext2_dir::dev_dir() || mounts().iter().any(|m| m.in_use && m.dir == dir) {
        return Err(ERR_BUSY);
    }
    let free = mounts().iter().position(|m| !m.in_use).ok_or(ERR_TOO_MANY_OPEN)?;
    let mut record = [0u8; RECORD_MAX];
    if syscall::sys_lent_read(sender, len, &mut record[..record_len]) != Ok(record_len) {
        return Err(ERR_INVALID_PATH);
    }
    // The capability for the server came with the request.
    let slot = syscall::sys_cap_take_any(sender).map_err(|()| ERR_INVALID_HANDLE)?;
    // One the kernel found here already is a mount's: a server that is a
    // filesystem here is not a second one.
    if mounts().iter().any(|m| m.in_use && m.slot == slot) {
        return Err(ERR_BUSY);
    }
    let give_up = |code: u64| {
        let _ = syscall::sys_cap_delete(slot);
        code
    };
    // Whatever was offered has to say it is a filesystem waiting to be
    // mounted, and soon: a task that is not one would not answer at all,
    // and this server would wait with everybody waiting on it.
    let adopt = Message { sender: 0, tag: TAG_ADOPT, data: [0; 6] };
    let mut reply = Message::empty();
    if syscall::sys_call_timeout(tid, &adopt, &mut reply, ADOPT_TICKS) != CallOutcome::Replied {
        return Err(give_up(ERR_IO));
    }
    if reply.tag != TAG_OK || !(KIND_EXT2..=KIND_FAT).contains(&reply.data[1]) {
        return Err(give_up(if reply.tag == TAG_ERROR && reply.data[0] == ERR_BUSY { ERR_BUSY } else { ERR_IO }));
    }
    mounts()[free] = Mount {
        in_use: true,
        dead: false,
        dir,
        tid,
        slot,
        pid: syscall::sys_pid(tid).unwrap_or(0),
        root_id: reply.data[0],
        kind: reply.data[1],
        told: None,
        record,
        record_len,
    };
    unsafe { COUNT += 1 };
    Ok(())
}

/// ATTACH, for a directory in a mounted filesystem: the request goes to
/// that filesystem's server, and the capability that came with it goes too.
fn attach_there(sender: usize, msg: &Message, away: &Away) {
    let m = away.mount;
    let (len, record_len) = (msg.data[0] as usize, msg.data[1] as usize);
    if record_len == 0 || record_len > RECORD_MAX {
        return error_reply(sender, ERR_INVALID_PATH);
    }
    let buf = relay();
    let n = away.len;
    if syscall::sys_lent_read(sender, len, &mut buf[n..n + record_len]) != Ok(record_len) {
        return error_reply(sender, ERR_INVALID_PATH);
    }
    let Ok(slot) = syscall::sys_cap_take_any(sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    // One the kernel found here already belongs to a mount of this server's.
    let mine = mounts().iter().any(|mount| mount.in_use && mount.slot == slot);
    let done = (|| -> Result<Message, u64> {
        if mine {
            return Err(ERR_BUSY);
        }
        // The server below answers for the caller, who is the one mounting.
        let who = Who::of(sender);
        tell(m, &who)?;
        mounts()[m].told = Some(who);
        let ask = Message {
            sender: 0,
            tag: TAG_ATTACH,
            data: [n as u64, record_len as u64, msg.data[2], 0, 0, away.base],
        };
        let with = CallWith {
            buf: buf.as_ptr() as u64,
            len_access: (n + record_len) as u64 | syscall::LEND_READ,
            offer: slot as u64,
            ticks: RELAY_TICKS,
        };
        let mut reply = Message::empty();
        match syscall::sys_call_with(mounts()[m].tid, &ask, &mut reply, &with) {
            CallOutcome::Replied if reply.tag == TAG_ERROR => Err(reply.data[0]),
            CallOutcome::Replied => Ok(reply),
            _ => {
                mounts()[m].dead = true;
                Err(ERR_IO)
            }
        }
    })();
    // The server below has its own copy now, or nothing came of it.
    if !mine {
        let _ = syscall::sys_cap_delete(slot);
    }
    match done {
        Ok(reply) => reply_opened(sender, reply.data),
        Err(code) => error_reply(sender, code),
    }
}

/// DETACH: the filesystem mounted on a directory of this one is taken away,
/// and its server ends. Not while anything in it is open.
fn detach(sender: usize, msg: &Message) {
    let done = (|| -> Result<(), u64> {
        let (uid, gid) = get_sender_uid_gid(sender);
        if uid != 0 {
            return Err(ERR_PERMISSION);
        }
        let path = lent_path(sender, 0, msg.data[0] as usize, 0)?;
        let base = crate::base_of(sender, msg.data[5])?;
        let dir = match ext2_dir::resolve_to(crate::ext2_state(), base, path, uid, gid, true, false)? {
            ext2_dir::Found::Inode(ino, _, _) => ino,
            ext2_dir::Found::Device(_) => return Err(ERR_INVALID_PATH),
        };
        // Not a directory anything is mounted on.
        let m = at(dir).ok_or(ERR_INVALID_PATH)?;
        if handles::any_in_mount(m) {
            return Err(ERR_BUSY);
        }
        if !mounts()[m].dead {
            let stop = Message { sender: 0, tag: TAG_RETIRE, data: [0; 6] };
            // No answer is a server that has gone already, which is what
            // was wanted.
            if let Ok(reply) = send(m, &stop, Lend::Nothing) {
                if reply.tag == TAG_ERROR {
                    return Err(reply.data[0]);
                }
            }
        }
        let _ = syscall::sys_cap_delete(mounts()[m].slot);
        mounts()[m] = NO_MOUNT;
        unsafe { COUNT -= 1 };
        Ok(())
    })();
    match done {
        Ok(()) => reply_opened(sender, [0; 6]),
        Err(code) => error_reply(sender, code),
    }
}

/// MOUNTS: `[index]`, with room lent for a record. Every mount under this
/// server is counted — its own first, each followed by whatever is mounted
/// inside it. Past the last, the error's second word is how many there are.
fn list(sender: usize, msg: &Message) {
    let mut n = msg.data[0];
    let mut total = 0u64;
    let give = |record: &[u8], kind: u64, pid: u64| match syscall::sys_lent_write(sender, 0, record) {
        Ok(len) if len == record.len() => reply_opened(sender, [len as u64, kind, pid, 0, 0, 0]),
        _ => error_reply(sender, ERR_IO),
    };
    // A server mounted in nothing is the root, and speaks for itself.
    if unsafe { PARENT_TID } == 0 {
        if n == 0 {
            let record = unsafe { &(&*core::ptr::addr_of!(SELF_RECORD))[..SELF_RECORD_LEN] };
            return give(record, unsafe { SELF_KIND }, syscall::sys_pid_self());
        }
        n -= 1;
        total += 1;
    }
    for m in 0..MAX_MOUNTS {
        if !mounts()[m].in_use {
            continue;
        }
        if n == 0 {
            let mount = &mounts()[m];
            return give(&mount.record[..mount.record_len], mount.kind, mount.pid);
        }
        n -= 1;
        total += 1;
        // And what is mounted in it.
        let page = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, RECORD_MAX) };
        let ask = Message { sender: 0, tag: TAG_MOUNTS, data: [n, 0, 0, 0, 0, 0] };
        match send(m, &ask, Lend::In(page)) {
            Ok(reply) if reply.tag != TAG_ERROR => {
                let len = (reply.data[0] as usize).min(RECORD_MAX);
                return give(&page[..len], reply.data[1], reply.data[2]);
            }
            Ok(reply) => {
                n -= reply.data[1].min(n);
                total += reply.data[1];
            }
            Err(_) => {}
        }
    }
    let reply = Message { sender: 0, tag: TAG_ERROR, data: [ERR_NOT_FOUND, total, 0, 0, 0, 0] };
    let _ = syscall::sys_reply(sender, &reply);
}
