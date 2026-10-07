//! inotify: what a program is told of changes to the files it watches.
//!
//! An instance is a descriptor this server makes for a program that asks
//! (`TAG_INOTIFY`, [`INOTIFY_INIT`]). Its cookie has [`NOT_A_FILE`] set, so
//! the C library takes it for one of the kernel's: a read goes through the
//! kernel, which says whether the reader would wait, and a poll asks the
//! kernel, which answers what this server last said it was ready for.
//!
//! A watch is of an inode of this filesystem, for one instance, with a mask
//! of Linux's events and a number the instance is told, which it never gives
//! out twice. Whatever changes something — and every open, read, write and
//! close of a file — says so here once it has ([`created`], [`removed`],
//! [`moved`], [`linked`], [`changed`], and for a handle [`opened`],
//! [`touched`], [`closed_handle`]); with no watches at all, that is one
//! comparison. An event about an inode goes to the watches of the inode, and
//! to the watches of the directory it was found in, with its name there.
//!
//! What an instance has not read waits in a queue of whole
//! `inotify_event`s, in the order they happened. One the same as the last
//! still waiting is not queued again, as Linux has it; one there is no room
//! for is IN_Q_OVERFLOW, once, and nothing more is queued until that has
//! been read.
//!
//! A read that may wait and finds nothing is held, and answered once a
//! request has made an event ([`settle`]); one that may not is told nothing
//! yet. A held read is let go when its task asks anything else — it is not
//! waiting any more: it was ended, and its number is somebody else's — or
//! dies.
//!
//! Only this server's own filesystem is watched, and only ext2's: a path
//! into a filesystem mounted here, into /dev or /proc, or on FAT is refused.
//! A watch on an inode whose last name goes is told IN_DELETE_SELF and
//! IN_IGNORED together, and is no more: Linux waits for the last descriptor
//! for the inode to close before IN_IGNORED.

use crate::handles::MAX_OPEN_FILES;
use crate::protocol::*;
use crate::{error_reply, ext2, ext2_dir, reply_opened};
use quark_rt::ipc::Message;
use quark_rt::syscall;

const MAX_INSTANCES: usize = 64;
/// What an instance's unread events may come to, in bytes.
const QUEUE: usize = 4096;
const MAX_WATCHES: usize = 2048;
/// Reads waiting on one instance for its next event.
const MAX_HELD: usize = 4;
/// An `inotify_event` without its name.
const HEAD: usize = 16;

pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
const IN_CLOSE_WRITE: u32 = 0x8;
const IN_CLOSE_NOWRITE: u32 = 0x10;
const IN_OPEN: u32 = 0x20;
const IN_MOVED_FROM: u32 = 0x40;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const IN_DELETE_SELF: u32 = 0x400;
const IN_MOVE_SELF: u32 = 0x800;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_EXCL_UNLINK: u32 = 0x0400_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
/// Every event a watch can ask for.
const ALL_EVENTS: u32 = 0xFFF;

struct Instance {
    used: bool,
    /// The last watch number given; the next is one more.
    last_wd: i32,
    queue: [u8; QUEUE],
    len: usize,
    /// Where the last event waiting begins, while any is.
    last: usize,
    /// Reads waiting for an event: the task, and the room it has.
    held: [(usize, usize); MAX_HELD],
    nheld: usize,
    /// Something was queued since its readers were last seen to.
    stirred: bool,
    /// What the kernel was last told: that there is something to read.
    said: bool,
}

const UNUSED: Instance = Instance {
    used: false,
    last_wd: 0,
    queue: [0; QUEUE],
    len: 0,
    last: 0,
    held: [(0, 0); MAX_HELD],
    nheld: 0,
    stirred: false,
    said: false,
};

#[derive(Clone, Copy)]
struct Watch {
    /// The instance plus one; 0 for a slot nobody has.
    instance: u16,
    wd: i32,
    ino: u32,
    mask: u32,
}

const FREE: Watch = Watch { instance: 0, wd: 0, ino: 0, mask: 0 };

/// What a handle was opened as, so that what is done through it can be said
/// of the file and of the directory it was found in.
#[derive(Clone, Copy)]
struct Opened {
    ino: u32,
    dir: u32,
    is_dir: bool,
    write: bool,
}

const NOT_OPENED: Opened = Opened { ino: 0, dir: 0, is_dir: false, write: false };

static mut INSTANCES: [Instance; MAX_INSTANCES] = [UNUSED; MAX_INSTANCES];
static mut WATCHES: [Watch; MAX_WATCHES] = [FREE; MAX_WATCHES];
/// How many watches there are: with none, nothing here is asked anything.
static mut WATCHING: usize = 0;
/// The last rename's cookie, which ties its two halves together.
static mut COOKIE: u32 = 0;
/// Some instance has events its readers have not been given.
static mut STIRRED: bool = false;
/// What each handle was opened on, as it grows with the handles' table.
static mut OPENED: crate::blocks::Blocks<Opened> = crate::blocks::Blocks::new(MAX_OPEN_FILES);

fn instances() -> &'static mut [Instance; MAX_INSTANCES] {
    unsafe { &mut *core::ptr::addr_of_mut!(INSTANCES) }
}

fn watches() -> &'static mut [Watch; MAX_WATCHES] {
    unsafe { &mut *core::ptr::addr_of_mut!(WATCHES) }
}

fn opened_as() -> &'static mut crate::blocks::Blocks<Opened> {
    unsafe { &mut *core::ptr::addr_of_mut!(OPENED) }
}

fn watching() -> bool {
    unsafe { WATCHING != 0 }
}

/// Whether `cookie` names an instance rather than a file.
pub fn is_instance(cookie: u64) -> bool {
    cookie & NOT_A_FILE != 0
}

/// The instance `cookie` names, if `sender` holds a descriptor for it.
fn held_by(sender: usize, cookie: u64) -> Option<usize> {
    let i = (cookie & !NOT_A_FILE) as usize;
    (is_instance(cookie) && i < MAX_INSTANCES && instances()[i].used && syscall::sys_fd_holds(sender, cookie))
        .then_some(i)
}

fn answer(sender: usize, word: u64) {
    reply_opened(sender, [word, 0, 0, 0, 0, 0]);
}

fn word(q: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([q[at], q[at + 1], q[at + 2], q[at + 3]])
}

/// TAG_INOTIFY: `[op, watch, instance]`.
pub fn request(sender: usize, msg: &Message) {
    match msg.data[0] {
        INOTIFY_INIT => init(sender),
        INOTIFY_REMOVE => match held_by(sender, msg.data[2]) {
            Some(i) if remove(i, msg.data[1] as i32) => answer(sender, 0),
            Some(_) => error_reply(sender, ERR_NOT_FOUND),
            None => error_reply(sender, ERR_INVALID_HANDLE),
        },
        INOTIFY_QUEUED => match held_by(sender, msg.data[2]) {
            Some(i) => answer(sender, instances()[i].len as u64),
            None => error_reply(sender, ERR_INVALID_HANDLE),
        },
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
}

/// A new instance, and the caller's descriptor for it.
fn init(sender: usize) {
    let Some(i) = instances().iter().position(|n| !n.used) else {
        return error_reply(sender, ERR_TOO_MANY_OPEN);
    };
    match syscall::sys_fd_serve_ready(sender, NOT_A_FILE | i as u64, syscall::ANY_FD) {
        Ok(fd) => {
            let n = &mut instances()[i];
            n.used = true;
            n.last_wd = 0;
            n.len = 0;
            n.nheld = 0;
            n.stirred = false;
            n.said = false;
            answer(sender, fd as u64);
        }
        Err(()) => error_reply(sender, ERR_TOO_MANY_OPEN),
    }
}

/// The last descriptor for an instance has gone, and so has it, with its
/// watches.
pub fn closed(cookie: u64) {
    let i = (cookie & !NOT_A_FILE) as usize;
    if i >= MAX_INSTANCES || !instances()[i].used {
        return;
    }
    let n = &mut instances()[i];
    for &(task, _) in &n.held[..n.nheld] {
        error_reply(task, ERR_INVALID_HANDLE);
    }
    n.nheld = 0;
    n.used = false;
    for w in watches().iter_mut().filter(|w| w.instance as usize == i + 1) {
        *w = FREE;
        unsafe { WATCHING -= 1 };
    }
}

/// `task` is not waiting to read any more: it has asked something else, or
/// has died.
pub fn drop_task(task: usize) {
    for n in instances().iter_mut().filter(|n| n.used && n.nheld > 0) {
        let mut k = 0;
        while k < n.nheld {
            if n.held[k].0 == task {
                n.held.copy_within(k + 1..n.nheld, k);
                n.nheld -= 1;
            } else {
                k += 1;
            }
        }
    }
}

/// The kernel reads or writes an instance for `sender`: `[cookie, room, do
/// not wait]`, with the room lent.
pub fn io(sender: usize, msg: &Message) {
    let Some(i) = held_by(sender, msg.data[0]) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    // Nothing is written to one.
    if msg.tag == quark_rt::ipc::TAG_FD_WRITE {
        return error_reply(sender, ERR_NOT_SUPPORTED);
    }
    let room = msg.data[1] as usize;
    let n = &mut instances()[i];
    if n.len > 0 {
        hand_over(i, sender, room);
        return ready(i);
    }
    if msg.data[2] & syscall::FD_IO_DO_NOT_WAIT != 0 {
        return answer(sender, syscall::FD_IO_NOTHING_YET);
    }
    if n.nheld == MAX_HELD {
        return error_reply(sender, ERR_BUSY);
    }
    n.held[n.nheld] = (sender, room);
    n.nheld += 1;
    // If it dies waiting this server is told; if it is dead already, it was
    // waiting for nothing.
    if syscall::sys_task_watch(sender).is_err() {
        drop_task(sender);
    }
}

/// Give `task`, with `room` bytes to read into, as many whole events as fit.
/// Not one fitting is a read refused: Linux's EINVAL.
fn hand_over(i: usize, task: usize, room: usize) {
    let n = &mut instances()[i];
    let mut take = 0;
    while take < n.len {
        let size = HEAD + word(&n.queue, take + 12) as usize;
        if take + size > room {
            break;
        }
        take += size;
    }
    if take == 0 {
        return error_reply(task, ERR_NO_SPACE);
    }
    if syscall::sys_lent_write(task, 0, &n.queue[..take]) != Ok(take) {
        return error_reply(task, ERR_IO);
    }
    n.queue.copy_within(take..n.len, 0);
    n.len -= take;
    n.last = n.last.saturating_sub(take);
    answer(task, take as u64);
}

/// Tell the kernel whether instance `i` has something to read, when that has
/// changed.
fn ready(i: usize) {
    let n = &mut instances()[i];
    let now = n.len > 0;
    if now != n.said {
        n.said = now;
        let _ = syscall::sys_fd_ready(NOT_A_FILE | i as u64, if now { syscall::FD_READY_READ } else { 0 });
    }
}

/// After a request: whoever waits to read the events it made is given them,
/// and whoever polls is told there are some.
pub fn settle() {
    if !unsafe { STIRRED } {
        return;
    }
    unsafe { STIRRED = false };
    for i in 0..MAX_INSTANCES {
        if !instances()[i].used || !instances()[i].stirred {
            continue;
        }
        instances()[i].stirred = false;
        loop {
            let n = &mut instances()[i];
            if n.nheld == 0 || n.len == 0 {
                break;
            }
            let (task, room) = n.held[0];
            n.held.copy_within(1..n.nheld, 0);
            n.nheld -= 1;
            hand_over(i, task, room);
        }
        ready(i);
    }
}

/// Queue an event for instance `i`.
fn queue(i: usize, wd: i32, mask: u32, cookie: u32, name: &[u8]) {
    let n = &mut instances()[i];
    let padded = if name.is_empty() { 0 } else { (name.len() + 1).next_multiple_of(HEAD) };
    if n.len > 0 {
        let at = n.last;
        // Overflowed, and not read since: nothing more until it is.
        if word(&n.queue, at + 4) == IN_Q_OVERFLOW {
            return;
        }
        // The same as the last one waiting: said already.
        if word(&n.queue, at) == wd as u32
            && word(&n.queue, at + 4) == mask
            && word(&n.queue, at + 8) == cookie
            && word(&n.queue, at + 12) as usize == padded
            && n.queue[at + HEAD..at + HEAD + name.len()] == *name
            && (name.is_empty() || n.queue[at + HEAD + name.len()] == 0)
        {
            return;
        }
    }
    // Room is kept for saying there was no room.
    let (wd, mask, cookie, name, padded) = if n.len + HEAD + padded + HEAD > QUEUE {
        (-1, IN_Q_OVERFLOW, 0, &[][..], 0)
    } else {
        (wd, mask, cookie, name, padded)
    };
    let at = n.len;
    n.queue[at..at + 4].copy_from_slice(&wd.to_le_bytes());
    n.queue[at + 4..at + 8].copy_from_slice(&mask.to_le_bytes());
    n.queue[at + 8..at + 12].copy_from_slice(&cookie.to_le_bytes());
    n.queue[at + 12..at + 16].copy_from_slice(&(padded as u32).to_le_bytes());
    n.queue[at + HEAD..at + HEAD + padded].fill(0);
    n.queue[at + HEAD..at + HEAD + name.len()].copy_from_slice(name);
    n.last = at;
    n.len = at + HEAD + padded;
    n.stirred = true;
    unsafe { STIRRED = true };
}

/// TAG_INOTIFY_ADD: `[path_len, mask, instance, 0, 0, base]`, the path lent.
/// What the mask asks for is the C library's to have checked.
pub fn add(sender: usize, msg: &Message) {
    let Some(i) = held_by(sender, msg.data[2]) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    if unsafe { crate::FS_TYPE } != crate::FsType::Ext2 {
        return error_reply(sender, ERR_NOT_SUPPORTED);
    }
    let mask = msg.data[1] as u32;
    let path = match lent_path(sender, 0, msg.data[0] as usize, 0) {
        Ok(p) => p,
        Err(code) => return error_reply(sender, code),
    };
    let base = match crate::base_of(sender, msg.data[5]) {
        Ok(b) => b,
        Err(code) => return error_reply(sender, code),
    };
    let (uid, gid) = crate::get_sender_uid_gid(sender);
    let follow = mask & IN_DONT_FOLLOW == 0;
    let (ino, inode) = match ext2_dir::resolve(crate::ext2_state(), base, path, uid, gid, follow) {
        Ok(ext2_dir::Found::Inode(ino, inode, _)) if ino != ext2_dir::dev_dir() && ino != ext2_dir::proc_dir() => {
            (ino, inode)
        }
        // What is another filesystem's, or not on a disk at all.
        Ok(_) | Err(ERR_ELSEWHERE) => return error_reply(sender, ERR_NOT_SUPPORTED),
        Err(code) => return error_reply(sender, code),
    };
    if mask & IN_ONLYDIR != 0 && !inode.is_dir() {
        return error_reply(sender, ERR_NOT_DIR);
    }
    // Linux's rule: to watch a file is to read what happens to it.
    if !ext2::check_permission(&inode, uid, gid, 4) {
        return error_reply(sender, ERR_PERMISSION);
    }
    let keep = mask & (ALL_EVENTS | IN_ONESHOT | IN_EXCL_UNLINK);
    if let Some(w) = watches().iter_mut().find(|w| w.instance as usize == i + 1 && w.ino == ino) {
        if mask & IN_MASK_CREATE != 0 {
            return error_reply(sender, ERR_EXISTS);
        }
        w.mask = if mask & IN_MASK_ADD != 0 { w.mask | keep } else { keep };
        return answer(sender, w.wd as u64);
    }
    let Some(w) = watches().iter_mut().find(|w| w.instance == 0) else {
        return error_reply(sender, ERR_NO_SPACE);
    };
    let n = &mut instances()[i];
    n.last_wd += 1;
    *w = Watch { instance: i as u16 + 1, wd: n.last_wd, ino, mask: keep };
    unsafe { WATCHING += 1 };
    answer(sender, w.wd as u64);
}

/// Instance `i`'s watch `wd` is no more. False if it has none of that number.
fn remove(i: usize, wd: i32) -> bool {
    match watches().iter().position(|w| w.instance as usize == i + 1 && w.wd == wd) {
        Some(k) => {
            unwatch(k);
            true
        }
        None => false,
    }
}

/// Watch `k` is no more, and its instance is told so.
fn unwatch(k: usize) {
    let w = watches()[k];
    watches()[k] = FREE;
    unsafe { WATCHING -= 1 };
    queue(w.instance as usize - 1, w.wd, IN_IGNORED, 0, &[]);
}

fn isdir(is_dir: bool) -> u32 {
    if is_dir { IN_ISDIR } else { 0 }
}

/// Watch `k` is told; one that was to be told once is no more.
fn tell(k: usize, mask: u32, cookie: u32, name: &[u8]) {
    let w = watches()[k];
    queue(w.instance as usize - 1, w.wd, mask, cookie, name);
    if w.mask & IN_ONESHOT != 0 {
        unwatch(k);
    }
}

/// `mask` happened to `ino` itself: the watches of it that asked are told.
fn to_self(ino: u32, mask: u32) {
    for k in 0..MAX_WATCHES {
        let w = watches()[k];
        if w.instance != 0 && w.ino == ino && w.mask & mask & ALL_EVENTS != 0 {
            tell(k, mask, 0, &[]);
        }
    }
}

/// `mask` happened to what directory `dir` has as `name`.
fn to_dir(dir: u32, name: &[u8], mask: u32, cookie: u32) {
    for k in 0..MAX_WATCHES {
        let w = watches()[k];
        if w.instance != 0 && w.ino == dir && w.mask & mask & ALL_EVENTS != 0 {
            tell(k, mask, cookie, name);
        }
    }
}

/// `ino` is gone: its watches are told IN_IGNORED, and are no more.
fn forget(ino: u32) {
    for k in 0..MAX_WATCHES {
        let w = watches()[k];
        if w.instance != 0 && w.ino == ino {
            unwatch(k);
        }
    }
}

/// What directory `dir` calls `ino`, into `buf`: its first name there, and
/// how long that is; 0 for none.
fn name_in(dir: u32, ino: u32, buf: &mut [u8; 256]) -> usize {
    let e2 = crate::ext2_state();
    let Ok(d) = ext2::read_inode(e2, dir) else {
        return 0;
    };
    let mut len = 0;
    let _ = ext2_dir::for_each_entry(e2, &d, |_, at, _, name| {
        if at == ino && name != b"." && name != b".." {
            len = name.len().min(255);
            buf[..len].copy_from_slice(&name[..len]);
            false
        } else {
            true
        }
    });
    len
}

/// `name` was made in directory `dir`.
pub fn created(dir: u32, name: &[u8], is_dir: bool) {
    if watching() {
        to_dir(dir, name, IN_CREATE | isdir(is_dir), 0);
    }
}

/// `ino` was given another name: `name`, in `dir`.
pub fn linked(dir: u32, name: &[u8], ino: u32) {
    if watching() {
        to_self(ino, IN_ATTRIB);
        to_dir(dir, name, IN_CREATE, 0);
    }
}

/// The name `name` in `dir` was taken from `ino`, and was its `last`, or not.
pub fn removed(dir: u32, name: &[u8], ino: u32, is_dir: bool, last: bool) {
    if !watching() {
        return;
    }
    if !is_dir {
        to_self(ino, IN_ATTRIB);
    }
    if last {
        to_self(ino, IN_DELETE_SELF);
    }
    to_dir(dir, name, IN_DELETE | isdir(is_dir), 0);
    if last {
        forget(ino);
    }
}

/// `ino`, which was `from_name` in `from`, is `to_name` in `to`; and what had
/// that name before, if anything did, lost it — its last name, or not.
pub fn moved(from: u32, from_name: &[u8], to: u32, to_name: &[u8], ino: u32, is_dir: bool, replaced: Option<(u32, bool)>) {
    if !watching() {
        return;
    }
    let cookie = unsafe {
        COOKIE = COOKIE.wrapping_add(1).max(1);
        COOKIE
    };
    to_dir(from, from_name, IN_MOVED_FROM | isdir(is_dir), cookie);
    to_dir(to, to_name, IN_MOVED_TO | isdir(is_dir), cookie);
    if let Some((victim, _)) = replaced {
        to_self(victim, IN_ATTRIB);
    }
    to_self(ino, IN_MOVE_SELF);
    if let Some((victim, true)) = replaced {
        to_self(victim, IN_DELETE_SELF);
        forget(victim);
    }
}

/// `mask` — an open, a read, a write, a change of attributes, a close —
/// happened to `ino`, which was found in directory `dir` (0 if that is not
/// known).
pub fn changed(ino: u32, dir: u32, is_dir: bool, mask: u32) {
    if !watching() {
        return;
    }
    let mask = mask | isdir(is_dir);
    if dir != 0 && watches().iter().any(|w| w.instance != 0 && w.ino == dir && w.mask & mask & ALL_EVENTS != 0) {
        let mut name = [0u8; 256];
        let len = name_in(dir, ino, &mut name);
        if len > 0 {
            to_dir(dir, &name[..len], mask, 0);
        }
    }
    to_self(ino, mask);
}

/// [`changed`], for a change of attributes, which does not say whether
/// `ino` is a directory.
pub fn attrib(ino: u32, dir: u32) {
    if watching() {
        let is_dir = ext2::read_inode(crate::ext2_state(), ino).is_ok_and(|i| i.is_dir());
        changed(ino, dir, is_dir, IN_ATTRIB);
    }
}

/// Handle `handle` was opened on `ino`, found in directory `dir`, to write or
/// not: IN_OPEN, and what is done through it from now on is said of it.
pub fn opened(handle: usize, ino: u32, dir: u32, is_dir: bool, write: bool) {
    // With no room to remember it by, what is done through it is not said
    // of it: the open is, below, and the rest is quiet.
    if opened_as().grow_to(handle, || NOT_OPENED) {
        if let Some(o) = opened_as().get_mut(handle) {
            *o = Opened { ino, dir, is_dir, write };
        }
    }
    changed(ino, dir, is_dir, IN_OPEN);
}

/// `mask` was done through `handle`, which is open on `ino`.
pub fn touched(handle: usize, ino: u32, mask: u32) {
    if !watching() || ino == 0 {
        return;
    }
    let Some(o) = opened_as().get(handle).copied() else { return };
    if o.ino == ino {
        changed(ino, o.dir, o.is_dir, mask);
    }
}

/// `handle`, which was open on `ino`, has closed.
pub fn closed_handle(handle: usize, ino: u32) {
    let Some(slot) = opened_as().get_mut(handle) else { return };
    let o = core::mem::replace(slot, NOT_OPENED);
    if o.ino == ino && ino != 0 {
        changed(ino, o.dir, o.is_dir, if o.write { IN_CLOSE_WRITE } else { IN_CLOSE_NOWRITE });
    }
}
