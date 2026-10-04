//! `/proc`: what the kernel and this server can say of the system as it is,
//! as files to read.
//!
//! None of it is on a disk, as none of `/dev` is. The root filesystem
//! carries an empty `/proc` so that listing `/` shows it, and a path that
//! walks into it is answered here (`ext2_dir::resolve_to`); on a root with
//! none, a path is matched as it is written, as `/dev`'s is. There is
//! `self`, a link to the caller's process id; `cpuinfo`, `meminfo`,
//! `mounts`, `uptime` and `version`; and a directory for each program, by
//! its process id, holding `cmdline`, `comm`, `mounts`, `stat` and `status`
//! — each in Linux's form, since that is what a program that reads one was
//! written for.
//!
//! A file is made when it is read, from what the kernel says at that moment:
//! one read in pieces may see it change between them, as on Linux. Each says
//! it is empty to `stat`, and is read to its end like any other.
//!
//! A program's name is what it was started as: its spawner says, and so does
//! its own `exec` (`SYS_PROGRAM_NAME`). One started by something that said
//! nothing has an empty `cmdline`, and is called by its process id.
//!
//! A program's directory is no inode, and the walk cannot stand in one. As a
//! working directory, or the start of a relative path, it is a number past
//! every inode's ([`base`]), which the walk turns back into a path through
//! `/proc`.

use crate::handles::{FsFileData, OpenFile};
use crate::protocol::*;
use crate::{error_reply, lend_out, reply_opened, space_of, CLIENT_BUF, PAGE_SIZE};
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::fmt::Write;
use quark_rt::ipc::Message;
use quark_rt::syscall;

/// Something under `/proc`.
#[derive(Clone, Copy, PartialEq)]
pub enum Node {
    /// `/proc` itself.
    Dir,
    /// `self`, as the link it is rather than followed.
    Myself,
    /// One of the system's files.
    File(File),
    /// A program's directory, by its process id.
    Process(u64),
    /// A file in one.
    Of(u64, Each),
}

#[derive(Clone, Copy, PartialEq)]
pub enum File {
    Cpuinfo,
    Meminfo,
    Mounts,
    Uptime,
    Version,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Each {
    Cmdline,
    Comm,
    Mounts,
    Stat,
    Status,
}

const FILES: [(&[u8], File); 5] = [
    (b"cpuinfo", File::Cpuinfo),
    (b"meminfo", File::Meminfo),
    (b"mounts", File::Mounts),
    (b"uptime", File::Uptime),
    (b"version", File::Version),
];

const EACH: [(&[u8], Each); 5] = [
    (b"cmdline", Each::Cmdline),
    (b"comm", Each::Comm),
    (b"mounts", Each::Mounts),
    (b"stat", Each::Stat),
    (b"status", Each::Status),
];

const DIR_MODE: u64 = 0o040555;
const FILE_MODE: u64 = 0o100444;
const LINK_MODE: u64 = 0o120777;

/// Ids beside anything a filesystem or `/dev` hands out: past four
/// gigabytes, where no inode is. `/proc` itself is the directory on the disk
/// where there is one.
const ID: u64 = 1 << 32;
/// The root directory's inode, which is what `/proc`'s `..` names.
const ROOT_ID: u64 = 2;

fn id_of(node: Node) -> u64 {
    match node {
        Node::Dir => match crate::ext2_dir::proc_dir() {
            0 => ID,
            dir => dir as u64,
        },
        Node::Myself => ID + 1,
        Node::File(f) => ID + 2 + f as u64,
        Node::Process(pid) => (ID + 0x100).wrapping_add(pid.wrapping_mul(8)),
        Node::Of(pid, e) => (ID + 0x101 + e as u64).wrapping_add(pid.wrapping_mul(8)),
    }
}

/// Whether there is a `/proc` here at all. A filesystem mounted in another
/// has none, as it has no `/dev`: its `proc` is a directory like any other.
static mut ENABLED: bool = true;

/// This server's filesystem has no `/proc` of the server's own.
pub fn disable() {
    unsafe { ENABLED = false };
}

fn enabled() -> bool {
    unsafe { ENABLED }
}

/// Whose request is being served: whose `self` it is.
static mut CALLER: usize = 0;

/// Say whose request this is, before anything in it is looked up.
pub fn serving(sender: usize) {
    unsafe { CALLER = sender };
}

// ---------------------------------------------------------------------------
// Programs, as the kernel's tasks say them
// ---------------------------------------------------------------------------

/// The kernel's tasks, by slot. Task 0 is its idle task, in no program.
const MAX_TASKS: usize = 64;

/// What the kernel says of a program, gathered from its tasks.
struct Program {
    /// One of its tasks to ask about it by: the first.
    tid: usize,
    /// The process that made it: whose program made the one of its tasks
    /// that another program made.
    parent: u64,
    /// Linux's letter for it: running, sleeping, stopped or a zombie.
    state: u8,
    /// How many of its tasks have not ended.
    threads: u64,
}

const DEAD: u8 = 3;

/// The program whose process id is `pid`, if it has a task.
fn program(pid: u64) -> Option<Program> {
    if pid == 0 {
        return None;
    }
    let rank = |c: u8| match c {
        b'R' => 0,
        b'S' => 1,
        b'T' => 2,
        _ => 3,
    };
    let mut found: Option<Program> = None;
    for tid in 1..MAX_TASKS {
        let Ok((state, parent, _)) = syscall::sys_task_info(tid) else { continue };
        if syscall::sys_pid(tid) != Some(pid) {
            continue;
        }
        let p = found.get_or_insert(Program { tid, parent: 0, state: b'Z', threads: 0 });
        let letter = match state {
            0 | 1 => b'R',
            2 => b'S',
            4 => b'T',
            _ => b'Z',
        };
        if rank(letter) < rank(p.state) {
            p.state = letter;
        }
        if state != DEAD {
            p.threads += 1;
        }
        // A task's own number for its creator is 0 when it has none, and
        // asked about 0 the kernel answers for the caller.
        if p.parent == 0 && parent != 0 {
            match syscall::sys_pid(parent) {
                Some(theirs) if theirs != pid => p.parent = theirs,
                _ => {}
            }
        }
    }
    // The program asking is waiting for this answer, and running as far as
    // it is concerned: on Linux a program reading its own state reads R.
    let caller = unsafe { CALLER };
    if let Some(p) = found.as_mut() {
        if caller != 0 && syscall::sys_pid(caller) == Some(pid) {
            p.state = b'R';
        }
    }
    found
}

/// The process ids there are, from `from` up and in order, into `out`: how
/// many.
fn processes(from: u64, out: &mut [u64; MAX_TASKS]) -> usize {
    let mut n = 0;
    for tid in 1..MAX_TASKS {
        if syscall::sys_task_info(tid).is_err() {
            continue;
        }
        let Some(pid) = syscall::sys_pid(tid) else { continue };
        if pid < from || out[..n].contains(&pid) {
            continue;
        }
        out[n] = pid;
        n += 1;
    }
    out[..n].sort_unstable();
    n
}

/// `name` as a process id that is there: digits, none of them a leading
/// nought.
fn pid_named(name: &[u8]) -> Option<u64> {
    if name.is_empty() || name.len() > 19 || name[0] == b'0' {
        return None;
    }
    let pid = name.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))?;
    program(pid).map(|_| pid)
}

/// `n` in decimal, into `buf`.
pub fn decimal(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buf[i..]
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// What walking into `/proc` came to.
pub enum Walked {
    /// The path ended here.
    At(Node),
    /// It came back up to `/proc` itself, having used this much of what it
    /// was given: the rest is the filesystem's to walk on from there.
    Back(usize),
}

/// Walk `rest` — what is left of a path that has reached `/proc`, starting
/// with a name in it — for as long as it stays below `/proc`. `self` is
/// followed unless it is the last name and `follow_last` says not to, or a
/// slash comes after it.
pub fn walk(rest: &[u8], follow_last: bool) -> Result<Walked, u64> {
    let mut at = Node::Dir;
    let mut i = 0;
    loop {
        while i < rest.len() && rest[i] == b'/' {
            i += 1;
        }
        if i == rest.len() {
            return Ok(Walked::At(at));
        }
        let start = i;
        while i < rest.len() && rest[i] != b'/' {
            i += 1;
        }
        let name = &rest[start..i];
        let last = i == rest.len();
        at = match at {
            Node::Dir => match name {
                // `/proc`'s own are the disk's.
                b"." | b".." => return Ok(Walked::Back(start)),
                b"self" if last && !follow_last => Node::Myself,
                b"self" => {
                    let caller = unsafe { CALLER };
                    Node::Process(syscall::sys_pid(caller).filter(|_| caller != 0).ok_or(ERR_NOT_FOUND)?)
                }
                _ => match FILES.iter().find(|(n, _)| *n == name) {
                    Some((_, f)) => Node::File(*f),
                    None => Node::Process(pid_named(name).ok_or(ERR_NOT_FOUND)?),
                },
            },
            Node::Process(pid) => match name {
                b"." => at,
                b".." => return Ok(Walked::Back(i)),
                _ => match EACH.iter().find(|(n, _)| *n == name) {
                    Some((_, e)) => Node::Of(pid, *e),
                    None => return Err(ERR_NOT_FOUND),
                },
            },
            // A file has nothing in it.
            _ => return Err(ERR_NOT_DIR),
        };
    }
}

/// What `path`, made whole, names under `/proc` on a root with none to walk
/// into: matched as it is written, `.` and `..` taken as they would be
/// walked. `None` for a path that is not under `/proc`.
pub fn lookup(path: &[u8], follow_last: bool) -> Option<Result<Node, u64>> {
    if !enabled() {
        return None;
    }
    // Nothing under /proc is more than two deep, so three components and a
    // count of the ones past them are all that matter.
    let mut kept: [&[u8]; 3] = [b""; 3];
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
    if depth == 0 || kept[0] != b"proc" {
        return None;
    }
    if extra > 0 {
        return Some(Err(ERR_NOT_FOUND));
    }
    let mut rest = [0u8; 48];
    let mut len = 0;
    for part in &kept[1..depth] {
        if len + part.len() + 1 > rest.len() {
            return Some(Err(ERR_NOT_FOUND));
        }
        rest[len..len + part.len()].copy_from_slice(part);
        rest[len + part.len()] = b'/';
        len += part.len() + 1;
    }
    // A slash after the last name follows `self`, as it does in a walk.
    let len = if path.ends_with(b"/") { len } else { len.saturating_sub(1) };
    Some(match walk(&rest[..len], follow_last) {
        Ok(Walked::At(node)) => Ok(node),
        Ok(Walked::Back(_)) => Ok(Node::Dir),
        Err(code) => Err(code),
    })
}

/// Whether a request that changes names touches `/proc`, on a root with no
/// `/proc` to walk into.
pub fn refuses(path: &[u8]) -> bool {
    lookup(path, false).is_some()
}

/// A program's directory as the base of a path: [`BASE`] and its process id.
const BASE: u32 = 0xC000_0000;

/// What a directory here is as a working directory or the start of a
/// relative path. `/proc` itself is the directory on the disk; without one,
/// nothing here can be walked from.
pub fn base(node: Node) -> Result<u32, u64> {
    let dir = crate::ext2_dir::proc_dir();
    match node {
        _ if dir == 0 => Err(ERR_NOT_SUPPORTED),
        Node::Dir => Ok(dir),
        Node::Process(pid) if pid < 1 << 30 => Ok(BASE | pid as u32),
        Node::Process(_) => Err(ERR_NOT_SUPPORTED),
        _ => Err(ERR_NOT_DIR),
    }
}

/// The program whose directory `base` is, if it is one.
pub fn base_pid(base: u32) -> Option<u64> {
    (base & BASE == BASE).then(|| (base & !BASE) as u64)
}

/// Whether the program whose directory `base` is is still there.
pub fn base_there(base: u32) -> bool {
    base_pid(base).is_some_and(|pid| program(pid).is_some())
}

/// `/proc/` and a process id: where a program's directory is.
pub fn path_of(pid: u64) -> &'static [u8] {
    static mut PATH: [u8; 32] = [0; 32];
    let out = unsafe { &mut *core::ptr::addr_of_mut!(PATH) };
    out[..6].copy_from_slice(b"/proc/");
    let mut digits = [0u8; 20];
    let d = decimal(pid, &mut digits);
    out[6..6 + d.len()].copy_from_slice(d);
    &out[..6 + d.len()]
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// OPEN of something here, found by a walk or by [`lookup`].
pub fn open(sender: usize, path: &[u8], node: Node, flags: u64) {
    let trailing = path.len() > 1 && path[path.len() - 1] == b'/';
    let is_dir = matches!(node, Node::Dir | Node::Process(_));
    let looks = asks(flags);
    if flags & OPEN_CREATE != 0 && flags & OPEN_EXCLUSIVE != 0 {
        return error_reply(sender, ERR_EXISTS);
    }
    if !is_dir && (trailing || flags & OPEN_DIRECTORY != 0) {
        return error_reply(sender, ERR_NOT_DIR);
    }
    // `self` not followed is a link, and is opened only to be asked about.
    if node == Node::Myself && !looks {
        return error_reply(sender, ERR_LOOP);
    }
    // What a file here says is the kernel's to say: nothing is written.
    if !looks && flags & (OPEN_WRITE | OPEN_TRUNCATE | OPEN_APPEND) != 0 {
        return error_reply(sender, if is_dir { ERR_IS_DIR } else { ERR_PERMISSION });
    }
    if let Node::Process(pid) | Node::Of(pid, _) = node {
        if program(pid).is_none() {
            return error_reply(sender, ERR_NOT_FOUND);
        }
    }
    let (mode, access) = match node {
        Node::Dir | Node::Process(_) => (DIR_MODE, 5),
        Node::Myself => (LINK_MODE, 7),
        _ => (FILE_MODE, 4),
    };
    let file = OpenFile {
        in_use: true,
        owner: space_of(sender),
        is_dir,
        writable: false,
        link: looks || node == Node::Myself,
        fs: FsFileData::Proc(node),
        ..OpenFile::empty()
    };
    crate::opened(sender, flags, file, [0, 0, is_dir as u64, mode, access, id_of(node)]);
}

/// OPEN of what [`lookup`] said: a refusal is one, and a name that is not
/// there cannot be made.
pub fn open_found(sender: usize, path: &[u8], found: Result<Node, u64>, flags: u64) {
    match found {
        Ok(node) => open(sender, path, node, flags),
        Err(ERR_NOT_FOUND) if flags & OPEN_CREATE != 0 => error_reply(sender, ERR_PERMISSION),
        Err(code) => error_reply(sender, code),
    }
}

/// READLINK of something here: `self` says the caller's process id, written
/// at `at` in what the caller lent, as much of it as `room` takes.
pub fn readlink(sender: usize, found: Result<Node, u64>, at: usize, room: usize) {
    match found {
        Ok(Node::Myself) => {}
        Ok(_) => return error_reply(sender, ERR_INVALID_PATH),
        Err(code) => return error_reply(sender, code),
    }
    let Some(pid) = syscall::sys_pid(sender) else {
        return error_reply(sender, ERR_NOT_FOUND);
    };
    let mut digits = [0u8; 20];
    let target = decimal(pid, &mut digits);
    let n = room.min(target.len());
    if n > 0 && syscall::sys_lent_write(sender, at, &target[..n]) != Ok(n) {
        return error_reply(sender, ERR_IO);
    }
    reply_opened(sender, [target.len() as u64, 0, 0, 0, 0, 0]);
}

/// Whether `msg`, a request naming a handle, is for one of these.
pub fn is_ours(sender: usize, msg: &Message) -> bool {
    matches!(crate::get_handle(msg.data[0] as usize, sender).map(|f| &f.fs), Some(FsFileData::Proc(_)))
}

/// READ, WRITE, STAT, READDIR_BULK and TRUNCATE on a handle [`is_ours`] said
/// is ours.
pub fn serve(sender: usize, msg: &Message) {
    let Some(file) = crate::get_handle(msg.data[0] as usize, sender) else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let FsFileData::Proc(node) = file.fs else {
        return error_reply(sender, ERR_INVALID_HANDLE);
    };
    let (is_dir, link) = (file.is_dir, file.link);
    match msg.tag {
        TAG_STAT => stat(sender, node),
        TAG_READ if is_dir => error_reply(sender, ERR_IS_DIR),
        TAG_READ if link => error_reply(sender, ERR_INVALID_HANDLE),
        TAG_READ => read(sender, node, msg.data[2], msg.data[3] as usize),
        TAG_READDIR_BULK if is_dir => list(sender, node, msg.data[1], msg.data[2] as usize),
        TAG_READDIR_BULK => error_reply(sender, ERR_NOT_DIR),
        TAG_WRITE | TAG_TRUNCATE if is_dir => error_reply(sender, ERR_IS_DIR),
        TAG_WRITE | TAG_TRUNCATE => error_reply(sender, ERR_PERMISSION),
        _ => error_reply(sender, ERR_NOT_SUPPORTED),
    }
}

fn read(sender: usize, node: Node, offset: u64, want: usize) {
    let text = match make(node) {
        Ok(text) => text,
        Err(code) => return error_reply(sender, code),
    };
    let start = (offset.min(text.len() as u64)) as usize;
    let n = want.min(PAGE_SIZE).min(text.len() - start);
    let client = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, n) };
    client.copy_from_slice(&text[start..start + n]);
    if lend_out(sender, n) {
        crate::reply_count(sender, n as u64);
    } else {
        error_reply(sender, ERR_IO);
    }
}

fn stat(sender: usize, node: Node) {
    let now = syscall::unix_time();
    let (mode, links, size, owner) = match node {
        Node::Dir => (DIR_MODE, 2, 0, None),
        Node::Myself => {
            let mut digits = [0u8; 20];
            let pid = syscall::sys_pid(sender).unwrap_or(0);
            (LINK_MODE, 1, decimal(pid, &mut digits).len() as u64, None)
        }
        Node::File(_) => (FILE_MODE, 1, 0, None),
        Node::Process(pid) | Node::Of(pid, _) => {
            let Some(p) = program(pid) else {
                return error_reply(sender, ERR_NOT_FOUND);
            };
            let mode = if is_process(node) { DIR_MODE } else { FILE_MODE };
            (mode, if is_process(node) { 2 } else { 1 }, 0, syscall::sys_get_tuid(p.tid).ok())
        }
    };
    let (uid, gid) = owner.unwrap_or((0, 0));
    let record = StatRecord {
        id: id_of(node),
        size,
        mode,
        links,
        uid: uid as u64,
        gid: gid as u64,
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

fn is_process(node: Node) -> bool {
    matches!(node, Node::Process(_))
}

/// `/proc`'s entries before the programs': `.`, `..`, `self` and the files.
/// A program's entry is at this plus its process id, so that a listing taken
/// up again goes on from the process ids it had not reached, whichever have
/// come and gone in between.
const FIXED: u64 = 3 + FILES.len() as u64;

fn list(sender: usize, node: Node, start: u64, room: usize) {
    if let Node::Process(pid) = node {
        if program(pid).is_none() {
            return error_reply(sender, ERR_NOT_FOUND);
        }
    }
    let room = room.min(PAGE_SIZE);
    let buf = unsafe { core::slice::from_raw_parts_mut(CLIENT_BUF as *mut u8, room) };
    let mut used = 0;
    let mut next = start;
    let mut end = true;
    // Each entry, at the place a listing would start from to have it: false
    // once one does not fit.
    let mut put = |at: u64, id: u64, kind: u8, name: &[u8]| -> bool {
        if at < start || !end {
            return end;
        }
        match put_dirent(buf, used, id, at + 1, 0, kind, name) {
            Some(len) => {
                used += len;
                next = at + 1;
            }
            None => end = false,
        }
        end
    };
    match node {
        Node::Dir => {
            put(0, id_of(Node::Dir), DT_DIR, b".");
            put(1, ROOT_ID, DT_DIR, b"..");
            put(2, id_of(Node::Myself), DT_LNK, b"self");
            for (i, (name, f)) in FILES.iter().enumerate() {
                put(3 + i as u64, id_of(Node::File(*f)), DT_REG, name);
            }
            let mut pids = [0u64; MAX_TASKS];
            let n = processes(start.saturating_sub(FIXED), &mut pids);
            for &pid in &pids[..n] {
                let mut digits = [0u8; 20];
                if !put(FIXED + pid, id_of(Node::Process(pid)), DT_DIR, decimal(pid, &mut digits)) {
                    break;
                }
            }
        }
        Node::Process(pid) => {
            put(0, id_of(node), DT_DIR, b".");
            put(1, id_of(Node::Dir), DT_DIR, b"..");
            for (i, (name, e)) in EACH.iter().enumerate() {
                put(2 + i as u64, id_of(Node::Of(pid, *e)), DT_REG, name);
            }
        }
        _ => return error_reply(sender, ERR_NOT_DIR),
    }
    crate::reply_dirents(sender, used, next, end);
}

// ---------------------------------------------------------------------------
// What the files say
// ---------------------------------------------------------------------------

/// Where a file is made: room for the longest, sixteen processors'
/// `cpuinfo`.
static mut TEXT: [u8; 32768] = [0; 32768];

struct Text {
    len: usize,
}

impl Text {
    fn bytes(&mut self, b: &[u8]) {
        let buf = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };
        let n = b.len().min(buf.len() - self.len);
        buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
    }

    fn done(&self) -> &'static [u8] {
        unsafe { &(&*core::ptr::addr_of!(TEXT))[..self.len] }
    }
}

impl Write for Text {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.bytes(s.as_bytes());
        Ok(())
    }
}

/// Nanoseconds in one of the ticks Linux counts a program's time in
/// (`USER_HZ`, a hundred a second).
const NS_PER_TICK: u64 = 10_000_000;

fn make(node: Node) -> Result<&'static [u8], u64> {
    let mut t = Text { len: 0 };
    match node {
        Node::File(File::Cpuinfo) => cpuinfo(&mut t),
        Node::File(File::Meminfo) => meminfo(&mut t),
        Node::File(File::Mounts) => mounts(&mut t),
        Node::File(File::Uptime) => uptime(&mut t),
        Node::File(File::Version) => version(&mut t),
        Node::Of(pid, each) => {
            let p = program(pid).ok_or(ERR_NOT_FOUND)?;
            let mut line = [0u8; syscall::PROGRAM_NAME_MAX];
            let len = syscall::sys_program_name(p.tid, &mut line).unwrap_or(0).min(line.len());
            let line = &line[..len];
            let mut digits = [0u8; 20];
            let name = match syscall::program_comm(line) {
                [] => decimal(pid, &mut digits),
                name => name,
            };
            match each {
                Each::Cmdline => t.bytes(line),
                Each::Comm => {
                    t.bytes(name);
                    t.bytes(b"\n");
                }
                Each::Mounts => mounts(&mut t),
                Each::Stat => process_stat(&mut t, pid, &p, name),
                Each::Status => status(&mut t, pid, &p, name),
            }
        }
        _ => return Err(ERR_IS_DIR),
    }
    Ok(t.done())
}

/// `/proc/PID/stat`: fifty-two fields on a line, in the order proc(5) gives
/// them, nought for what nothing here keeps.
fn process_stat(t: &mut Text, pid: u64, p: &Program, name: &[u8]) {
    let usage = syscall::sys_usage_of(p.tid).unwrap_or_default();
    let nice = syscall::sys_nice(pid, None).unwrap_or(0);
    let group = syscall::sys_getpgid(pid).unwrap_or(0);
    let session = syscall::sys_getsid(pid).unwrap_or(0);
    let _ = write!(t, "{} (", pid);
    t.bytes(name);
    let _ = write!(
        t,
        ") {} {} {} {} 0 -1 0 0 0 0 0 {} {} 0 0 {} {} {} 0 0 0 0 {}",
        p.state as char,
        p.parent,
        group,
        session,
        usage.user_ns / NS_PER_TICK,
        usage.system_ns / NS_PER_TICK,
        20 + nice,
        nice,
        p.threads.max(1),
        u64::MAX
    );
    // From the start of its code to its last task's wait channel: twelve.
    for _ in 0..12 {
        t.bytes(b" 0");
    }
    // What its parent is sent when it ends, and the fourteen after.
    t.bytes(b" 17");
    for _ in 0..14 {
        t.bytes(b" 0");
    }
    t.bytes(b"\n");
}

/// `/proc/PID/status`: what a person reads, a name and a value to a line.
fn status(t: &mut Text, pid: u64, p: &Program, name: &[u8]) {
    let state = match p.state {
        b'R' => "R (running)",
        b'S' => "S (sleeping)",
        b'T' => "T (stopped)",
        _ => "Z (zombie)",
    };
    let (uid, gid) = syscall::sys_get_tuid(p.tid).unwrap_or((0, 0));
    let usage = syscall::sys_usage_of(p.tid).unwrap_or_default();
    t.bytes(b"Name:\t");
    t.bytes(name);
    let _ = write!(
        t,
        "\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nTracerPid:\t0\nUid:\t{u}\t{u}\t{u}\t{u}\nGid:\t{g}\t{g}\t{g}\t{g}\nGroups:\t",
        state,
        pid,
        pid,
        p.parent,
        u = uid,
        g = gid
    );
    let mut groups = [0u32; 16];
    let n = syscall::sys_groups(p.tid, &mut groups).unwrap_or(0).min(groups.len());
    for g in &groups[..n] {
        let _ = write!(t, "{} ", g);
    }
    let _ = write!(
        t,
        "\nThreads:\t{}\nvoluntary_ctxt_switches:\t{}\nnonvoluntary_ctxt_switches:\t{}\n",
        p.threads.max(1),
        usage.voluntary,
        usage.involuntary
    );
}

fn meminfo(t: &mut Text) {
    let (total, _) = syscall::sys_mem_total();
    let (free, _) = syscall::sys_mem_info();
    let (room, used) = syscall::sys_swap_room();
    let kb = |pages: usize| pages as u64 * (PAGE_SIZE as u64 / 1024);
    for (label, value) in [
        ("MemTotal:", kb(total)),
        ("MemFree:", kb(free)),
        ("MemAvailable:", kb(free)),
        ("Buffers:", 0),
        ("Cached:", 0),
        ("SwapTotal:", kb(room)),
        ("SwapFree:", kb(room.saturating_sub(used))),
    ] {
        let _ = writeln!(t, "{:<15} {:>8} kB", label, value);
    }
}

/// Seconds since the machine started, to a hundredth; and the time its
/// processors have spent with nothing to do, which nothing here counts.
fn uptime(t: &mut Text) {
    let hundredths = syscall::sys_clock() / NS_PER_TICK;
    let _ = writeln!(t, "{}.{:02} 0.00", hundredths / 100, hundredths % 100);
}

/// What `uname` says: the system, the version of its system calls, the
/// machine.
fn version(t: &mut Text) {
    let (major, minor) = syscall::sys_abi_version();
    let _ = writeln!(t, "Quark version {}.{} x86_64", major, minor);
}

/// What is mounted, a line each: where it came from, where it is, its kind,
/// and Linux's three more fields.
fn mounts(t: &mut Text) {
    let mut page = [0u8; 512];
    for n in 0..64 {
        let Some((len, kind)) = crate::mounts::record(n, &mut page) else { break };
        let mut parts = page[..len].split(|&b| b == 0);
        let source = parts.next().unwrap_or(b"");
        let target = parts.next().unwrap_or(b"");
        escaped(t, source);
        t.bytes(b" ");
        escaped(t, target);
        let kind = match kind {
            KIND_EXT2 => "ext2",
            KIND_EXT4 => "ext4",
            KIND_FAT => "vfat",
            KIND_TMPFS => "tmpfs",
            _ => "unknown",
        };
        let _ = writeln!(t, " {} rw 0 0", kind);
    }
}

/// A path in a line of `mounts`, its spaces and the like written as octal
/// escapes, as Linux writes them.
fn escaped(t: &mut Text, path: &[u8]) {
    for &b in path {
        match b {
            b' ' | b'\t' | b'\n' | b'\\' => {
                let _ = write!(t, "\\{:03o}", b);
            }
            _ => t.bytes(&[b]),
        }
    }
}

/// Linux's names for what `cpuid` says, register by register, in the order
/// its `flags` line has them.
const LEAF1_EDX: [(u32, &str); 29] = [
    (0, "fpu"), (1, "vme"), (2, "de"), (3, "pse"), (4, "tsc"), (5, "msr"), (6, "pae"), (7, "mce"),
    (8, "cx8"), (9, "apic"), (11, "sep"), (12, "mtrr"), (13, "pge"), (14, "mca"), (15, "cmov"),
    (16, "pat"), (17, "pse36"), (18, "pn"), (19, "clflush"), (21, "dts"), (22, "acpi"), (23, "mmx"),
    (24, "fxsr"), (25, "sse"), (26, "sse2"), (27, "ss"), (28, "ht"), (29, "tm"), (31, "pbe"),
];
const EXT1_EDX: [(u32, &str); 10] = [
    (11, "syscall"), (19, "mp"), (20, "nx"), (22, "mmxext"), (25, "fxsr_opt"), (26, "pdpe1gb"),
    (27, "rdtscp"), (29, "lm"), (30, "3dnowext"), (31, "3dnow"),
];
const LEAF1_ECX: [(u32, &str); 30] = [
    (0, "pni"), (1, "pclmulqdq"), (2, "dtes64"), (3, "monitor"), (4, "ds_cpl"), (5, "vmx"), (6, "smx"),
    (7, "est"), (8, "tm2"), (9, "ssse3"), (10, "cid"), (11, "sdbg"), (12, "fma"), (13, "cx16"),
    (14, "xtpr"), (15, "pdcm"), (17, "pcid"), (18, "dca"), (19, "sse4_1"), (20, "sse4_2"),
    (21, "x2apic"), (22, "movbe"), (23, "popcnt"), (24, "tsc_deadline_timer"), (25, "aes"),
    (26, "xsave"), (28, "avx"), (29, "f16c"), (30, "rdrand"), (31, "hypervisor"),
];
const EXT1_ECX: [(u32, &str); 26] = [
    (0, "lahf_lm"), (1, "cmp_legacy"), (2, "svm"), (3, "extapic"), (4, "cr8_legacy"), (5, "abm"),
    (6, "sse4a"), (7, "misalignsse"), (8, "3dnowprefetch"), (9, "osvw"), (10, "ibs"), (11, "xop"),
    (12, "skinit"), (13, "wdt"), (15, "lwp"), (16, "fma4"), (17, "tce"), (19, "nodeid_msr"),
    (21, "tbm"), (22, "topoext"), (23, "perfctr_core"), (24, "perfctr_nb"), (26, "bpext"),
    (27, "ptsc"), (28, "perfctr_llc"), (29, "mwaitx"),
];
const LEAF7_EBX: [(u32, &str); 26] = [
    (0, "fsgsbase"), (1, "tsc_adjust"), (3, "bmi1"), (4, "hle"), (5, "avx2"), (7, "smep"), (8, "bmi2"),
    (9, "erms"), (10, "invpcid"), (11, "rtm"), (14, "mpx"), (16, "avx512f"), (17, "avx512dq"),
    (18, "rdseed"), (19, "adx"), (20, "smap"), (21, "avx512ifma"), (23, "clflushopt"), (24, "clwb"),
    (25, "intel_pt"), (26, "avx512pf"), (27, "avx512er"), (28, "avx512cd"), (29, "sha_ni"),
    (30, "avx512bw"), (31, "avx512vl"),
];
/// Protection keys (bits 3 and 4) are not here: the kernel leaves them off.
const LEAF7_ECX: [(u32, &str); 15] = [
    (1, "avx512vbmi"), (2, "umip"), (5, "waitpkg"), (6, "avx512_vbmi2"), (8, "gfni"), (9, "vaes"),
    (10, "vpclmulqdq"), (11, "avx512_vnni"), (12, "avx512_bitalg"), (14, "avx512_vpopcntdq"),
    (16, "la57"), (22, "rdpid"), (25, "cldemote"), (27, "movdiri"), (28, "movdir64b"),
];
const LEAF7_EDX: [(u32, &str); 17] = [
    (2, "avx512_4vnniw"), (3, "avx512_4fmaps"), (4, "fsrm"), (8, "avx512_vp2intersect"),
    (10, "md_clear"), (14, "serialize"), (16, "tsxldtrk"), (18, "pconfig"), (19, "arch_lbr"),
    (22, "amx_bf16"), (23, "avx512_fp16"), (24, "amx_tile"), (25, "amx_int8"), (27, "stibp"),
    (28, "flush_l1d"), (29, "arch_capabilities"), (31, "ssbd"),
];
const LEAFD_EAX: [(u32, &str); 4] = [(0, "xsaveopt"), (1, "xsavec"), (2, "xgetbv1"), (3, "xsaves")];

/// The processor's `flags` line, into `out`: how long it is. What the kernel
/// has not turned on for programs — the vector registers XCR0 does not save,
/// protection keys — is not said, since a program that read it would use it.
fn flags(out: &mut [u8; 1024]) -> usize {
    let top = __cpuid(0).eax;
    let ext_top = __cpuid(0x8000_0000).eax;
    let one = __cpuid(1);
    let (ext_ecx, ext_edx) = if ext_top >= 0x8000_0001 {
        let r = __cpuid(0x8000_0001);
        (r.ecx, r.edx)
    } else {
        (0, 0)
    };
    let seven = if top >= 7 { Some(__cpuid_count(7, 0)) } else { None };
    let xsave_more = if top >= 0xD { __cpuid_count(0xD, 1).eax } else { 0 };
    // What the kernel saves for programs: x87 and SSE always, AVX and
    // AVX-512 where it turned them on.
    let xcr0 = if one.ecx & (1 << 27) != 0 {
        let (lo, hi): (u32, u32);
        unsafe { core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
        (hi as u64) << 32 | lo as u64
    } else {
        0
    };
    let avx = xcr0 & 0b110 == 0b110;
    let avx512 = avx && xcr0 & 0xE0 == 0xE0;
    let amx = xcr0 & (3 << 17) == 3 << 17;
    let usable = |name: &str| {
        if name.starts_with("avx512") {
            avx512
        } else if name.starts_with("amx") {
            amx
        } else {
            avx || !matches!(name, "avx" | "avx2" | "fma" | "f16c" | "vaes" | "vpclmulqdq")
        }
    };
    let mut len = 0;
    let (b7, c7, d7) = seven.map_or((0, 0, 0), |r| (r.ebx, r.ecx, r.edx));
    let tables: [(&[(u32, &str)], u32); 8] = [
        (&LEAF1_EDX, one.edx),
        (&EXT1_EDX, ext_edx),
        (&LEAF1_ECX, one.ecx),
        (&EXT1_ECX, ext_ecx),
        (&LEAF7_EBX, b7),
        (&LEAF7_ECX, c7),
        (&LEAF7_EDX, d7),
        (&LEAFD_EAX, xsave_more),
    ];
    for (table, reg) in tables {
        for &(bit, name) in table {
            if reg & (1 << bit) == 0 || !usable(name) || len + 1 + name.len() > out.len() {
                continue;
            }
            out[len] = b' ';
            out[len + 1..len + 1 + name.len()].copy_from_slice(name.as_bytes());
            len += 1 + name.len();
        }
    }
    len
}

/// What the processor says it is — the same of every processor here, so
/// asked of whichever this server is on — once for each of them.
fn cpuinfo(t: &mut Text) {
    let (count, _) = syscall::sys_cpus();
    let top = __cpuid(0);
    let mut vendor = [0u8; 12];
    vendor[..4].copy_from_slice(&top.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&top.edx.to_le_bytes());
    vendor[8..].copy_from_slice(&top.ecx.to_le_bytes());
    let one = __cpuid(1);
    let base_family = (one.eax >> 8) & 0xF;
    let family = if base_family == 0xF { base_family + ((one.eax >> 20) & 0xFF) } else { base_family };
    let model = if base_family == 6 || base_family == 0xF {
        ((one.eax >> 4) & 0xF) | ((one.eax >> 12) & 0xF0)
    } else {
        (one.eax >> 4) & 0xF
    };
    let ext_top = __cpuid(0x8000_0000).eax;
    let mut brand = [0u8; 48];
    if ext_top >= 0x8000_0004 {
        for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
            let r = __cpuid(leaf);
            for (j, word) in [r.eax, r.ebx, r.ecx, r.edx].into_iter().enumerate() {
                brand[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
    }
    let brand = {
        let end = brand.iter().position(|&b| b == 0).unwrap_or(brand.len());
        let s = &brand[..end];
        let first = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
        let last = s.iter().rposition(|&b| b != b' ').map_or(first, |p| p + 1);
        &s[first..last.max(first)]
    };
    let mhz = if top.eax >= 0x16 { __cpuid(0x16).eax & 0xFFFF } else { 0 };
    let (physical, virtual_) = if ext_top >= 0x8000_0008 {
        let r = __cpuid(0x8000_0008).eax;
        (r & 0xFF, (r >> 8) & 0xFF)
    } else {
        (36, 48)
    };
    let line = ((one.ebx >> 8) & 0xFF) * 8;
    let mut said = [0u8; 1024];
    let said_len = flags(&mut said);
    for p in 0..count {
        let _ = writeln!(t, "processor\t: {}", p);
        t.bytes(b"vendor_id\t: ");
        t.bytes(&vendor);
        let _ = write!(t, "\ncpu family\t: {}\nmodel\t\t: {}\nmodel name\t: ", family, model);
        t.bytes(brand);
        let _ = writeln!(t, "\nstepping\t: {}", one.eax & 0xF);
        if mhz != 0 {
            let _ = writeln!(t, "cpu MHz\t\t: {}.000", mhz);
        }
        let _ = writeln!(
            t,
            "physical id\t: 0\nsiblings\t: {}\ncore id\t\t: {}\ncpu cores\t: {}\napicid\t\t: {}\nfpu\t\t: yes\nfpu_exception\t: yes\ncpuid level\t: {}\nwp\t\t: yes",
            count, p, count, p, top.eax
        );
        t.bytes(b"flags\t\t:");
        t.bytes(&said[..said_len]);
        let _ = writeln!(
            t,
            "\nclflush size\t: {}\ncache_alignment\t: {}\naddress sizes\t: {} bits physical, {} bits virtual\n",
            line, line, physical, virtual_
        );
    }
}
