#![no_std]
#![no_main]

//! The log: what every service prints, passed on to the console and kept.
//!
//! A service's descriptors 1 and 2 are IPC descriptors to this program, one
//! stream each (`quark_rt::logd`), so a write there is a call here with the
//! stream in its tag. What comes is passed on to the console as it came —
//! the screen shows what it always did, until the session has it — and cut
//! into lines, each stamped
//! with the date and who said it: the last of them are kept for each stream,
//! for `svc log`, and once init says the root is up every one is written to
//! `/var/log/messages`, which is moved to `messages.0` when it is a megabyte
//! long.
//!
//! Three threads, so that nothing here waits on anything a service could be
//! waiting on: this one only receives, and answers at once; one writes to the
//! console, which may be full; one writes the file, through the file server,
//! which may itself be writing a line here. A writer the console or the file
//! keeps waiting holds up the bytes behind it, and nobody else.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt::Write;
use quark_rt::calendar::Date;
use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::logd;
use quark_rt::manifest::CapReq;
use quark_rt::sync::{Condvar, Mutex};
use quark_rt::{nameserver, syscall, thread, vfs};

// A server: everything init starts after it writes here.
quark_rt::manifest!([CapReq::priority(syscall::PRIO_SERVER)]);

/// The lines kept for each stream.
const TAIL_LINES: usize = 64;
/// The longest line; a longer one is cut there.
const LINE_MAX: usize = 1024;
/// The most bytes waiting for the console, and lines waiting for the file:
/// the oldest go when there are more.
const CONSOLE_MAX: usize = 64 * 1024;
const FILE_MAX: usize = 256 * 1024;
/// How long the file may be before it is moved aside.
const ROTATE_AT: u64 = 1024 * 1024;

struct Stream {
    name: Vec<u8>,
    pid: u64,
    partial: Vec<u8>,
    tail: VecDeque<Vec<u8>>,
}

/// What the three threads share.
struct Shared {
    console: VecDeque<u8>,
    file: VecDeque<u8>,
    /// Whether the root is there to write on, and whether it would not be.
    files: bool,
    no_file: bool,
    /// Writes asked for and written, counted.
    sync_asked: u64,
    sync_done: u64,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    console: VecDeque::new(),
    file: VecDeque::new(),
    files: false,
    no_file: false,
    sync_asked: 0,
    sync_done: 0,
});
static FOR_CONSOLE: Condvar = Condvar::new();
static FOR_FILE: Condvar = Condvar::new();

/// The console, written as fast as it takes it.
extern "C" fn console() -> ! {
    let mut chunk = Vec::new();
    loop {
        {
            let mut s = SHARED.lock();
            while s.console.is_empty() {
                s = FOR_CONSOLE.wait(s);
            }
            let n = s.console.len().min(4096);
            chunk.clear();
            chunk.extend(s.console.drain(..n));
        }
        let mut at = 0;
        while at < chunk.len() {
            match syscall::sys_fd_write(1, &chunk[at..]) {
                0 | u64::MAX => break,
                n => at += n as usize,
            }
        }
    }
}

/// The file, written a batch at a time once the root is up.
extern "C" fn file() -> ! {
    let mut fd: Option<usize> = None;
    let mut size: u64 = 0;
    let mut batch = Vec::new();
    loop {
        let sync;
        {
            let mut s = SHARED.lock();
            while !(s.files && (!s.file.is_empty() || s.sync_asked != s.sync_done)) {
                s = FOR_FILE.wait(s);
            }
            sync = s.sync_asked;
            batch.clear();
            batch.extend(s.file.drain(..));
        }
        let vfs_tid = nameserver::lookup(b"vfs").unwrap_or(0);
        if fd.is_none() && vfs_tid != 0 {
            let _ = vfs::mkdir(vfs_tid, b"/var/log");
            fd = vfs::open_fd(vfs_tid, logd::PATH, vfs::OPEN_WRITE | vfs::OPEN_CREATE | vfs::OPEN_APPEND, 0o640).ok();
            size = vfs::open(vfs_tid, logd::PATH).ok().map_or(0, |(h, n, _)| {
                let _ = vfs::close(vfs_tid, h);
                n as u64
            });
        }
        let Some(mut f) = fd else {
            // Nowhere to write: kept in memory only, and said once.
            let mut s = SHARED.lock();
            s.files = false;
            s.no_file = true;
            s.file.clear();
            s.sync_done = sync;
            drop(s);
            say(b"[logd] /var/log/messages cannot be written: what is printed is kept in memory only\n");
            continue;
        };
        if size + batch.len() as u64 > ROTATE_AT {
            let _ = syscall::sys_fd_close(f);
            let _ = vfs::unlink(vfs_tid, logd::OLD_PATH);
            let _ = vfs::rename(vfs_tid, logd::PATH, logd::OLD_PATH);
            match vfs::open_fd(vfs_tid, logd::PATH, vfs::OPEN_WRITE | vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE, 0o640) {
                Ok(new) => {
                    f = new;
                    fd = Some(new);
                    size = 0;
                }
                Err(_) => {
                    fd = None;
                    continue;
                }
            }
        }
        let mut at = 0;
        while at < batch.len() {
            match syscall::sys_fd_write(f, &batch[at..]) {
                0 | u64::MAX => break,
                n => at += n as usize,
            }
        }
        size += at as u64;
        if sync != SHARED.lock().sync_done {
            let _ = vfs::sync(vfs_tid);
        }
        SHARED.lock().sync_done = sync;
        // A moment, for more to come and go in one write.
        syscall::sleep_ms(100);
    }
}

/// A line of this program's own, on the console.
fn say(text: &[u8]) {
    let mut s = SHARED.lock();
    s.console.extend(text);
    drop(s);
    FOR_CONSOLE.notify_one();
}

/// `write!` into a `Vec<u8>`.
struct Bytes<'a>(&'a mut Vec<u8>);

impl Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// A finished line, stamped: the date, who said it, what.
fn stamped(stream: &Stream, line: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len() + 48);
    let d = Date::from_unix(syscall::sys_clock_wall() / 1_000_000_000);
    let _ = write!(
        Bytes(&mut out),
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}[{}]: ",
        d.year,
        d.month,
        d.day,
        d.hour,
        d.minute,
        d.second,
        core::str::from_utf8(&stream.name).unwrap_or("?"),
        stream.pid
    );
    // What a terminal would act on is not kept as itself.
    out.extend(line.iter().map(|&b| if b == b'\t' || (b' '..0x7F).contains(&b) || b >= 0x80 { b } else { b'?' }));
    out.push(b'\n');
    out
}

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let me = syscall::sys_getpid() as usize;
    let parent = syscall::sys_task_info(me).map_or(0, |(_, parent, _)| parent);
    if thread::spawn_with_stack(console, 4).is_err() || thread::spawn_with_stack(file, 16).is_err() {
        let _ = syscall::sys_fd_write(1, b"[logd] could not start its threads\n");
        syscall::sys_exit_code(1);
    }
    let mut streams: Vec<Option<Stream>> = Vec::new();
    // The session has the console: what comes is only kept.
    let mut quiet = false;
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() || msg.sender == 0 {
            continue;
        }
        let tag = msg.tag;
        if (logd::STREAM..logd::STREAM + logd::MAX_STREAMS).contains(&tag) {
            // A write: answered first, so the writer goes on.
            reply(msg.sender, logd::TAG_OK, [0; 6]);
            let id = (tag - logd::STREAM) as usize;
            let n = (msg.data[0] as usize).min(40);
            let mut bytes = [0u8; 40];
            for (i, w) in msg.data[1..6].iter().enumerate() {
                bytes[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
            }
            let bytes = &bytes[..n];
            if id >= logd::KEPT_STREAMS as usize {
                // Not kept: passed on, while anything is.
                if !quiet {
                    SHARED.lock().console.extend(bytes);
                    FOR_CONSOLE.notify_one();
                }
                continue;
            }
            if streams.len() <= id {
                streams.resize_with(id + 1, || None);
            }
            let stream = streams[id].get_or_insert_with(|| Stream {
                name: alloc::format!("stream-{}", id).into_bytes(),
                pid: 0,
                partial: Vec::new(),
                tail: VecDeque::new(),
            });
            let mut lines = Vec::new();
            for &b in bytes {
                if b == b'\n' || stream.partial.len() >= LINE_MAX {
                    let line = core::mem::take(&mut stream.partial);
                    let line: Vec<u8> = line.into_iter().filter(|&c| c != b'\r').collect();
                    lines.push(stamped(stream, &line));
                    if b != b'\n' {
                        stream.partial.push(b);
                    }
                } else {
                    stream.partial.push(b);
                }
            }
            for line in &lines {
                if stream.tail.len() == TAIL_LINES {
                    stream.tail.pop_front();
                }
                stream.tail.push_back(line.clone());
            }
            let mut s = SHARED.lock();
            if !quiet {
                s.console.extend(bytes);
                while s.console.len() > CONSOLE_MAX {
                    s.console.pop_front();
                }
            }
            if !s.no_file {
                for line in lines.iter() {
                    s.file.extend(line);
                }
                // The oldest lines go: from the next line's start.
                if s.file.len() > FILE_MAX {
                    let over = s.file.len() - FILE_MAX;
                    let cut = s.file.iter().skip(over).position(|&b| b == b'\n').map_or(s.file.len(), |p| over + p + 1);
                    s.file.drain(..cut);
                }
            }
            let any = !lines.is_empty();
            drop(s);
            FOR_CONSOLE.notify_one();
            if any {
                FOR_FILE.notify_one();
            }
            continue;
        }
        if msg.sender != parent {
            reply(msg.sender, logd::TAG_ERROR, [1, 0, 0, 0, 0, 0]);
            continue;
        }
        match tag {
            logd::TAG_NAME => {
                let id = msg.data[0] as usize;
                if id >= logd::KEPT_STREAMS as usize {
                    reply(msg.sender, logd::TAG_ERROR, [22, 0, 0, 0, 0, 0]);
                    continue;
                }
                let mut name = [0u8; 24];
                for i in 0..3 {
                    name[i * 8..i * 8 + 8].copy_from_slice(&msg.data[1 + i].to_le_bytes());
                }
                let len = (msg.data[4] as usize).min(24);
                if streams.len() <= id {
                    streams.resize_with(id + 1, || None);
                }
                let stream = streams[id].get_or_insert_with(|| Stream { name: Vec::new(), pid: 0, partial: Vec::new(), tail: VecDeque::new() });
                stream.name = name[..len].to_vec();
                stream.pid = msg.data[5];
                reply(msg.sender, logd::TAG_OK, [0; 6]);
            }
            logd::TAG_FILES => {
                SHARED.lock().files = true;
                FOR_FILE.notify_one();
                reply(msg.sender, logd::TAG_OK, [0; 6]);
            }
            logd::TAG_TAIL => {
                let id = msg.data[0] as usize;
                let room = msg.data[1] as usize;
                let mut text = Vec::new();
                if let Some(Some(stream)) = streams.get(id) {
                    for line in &stream.tail {
                        text.extend_from_slice(line);
                    }
                }
                let n = text.len().min(room);
                if n > 0 && syscall::sys_lent_write(msg.sender, 0, &text[..n]).is_err() {
                    reply(msg.sender, logd::TAG_ERROR, [22, 0, 0, 0, 0, 0]);
                    continue;
                }
                reply(msg.sender, logd::TAG_OK, [n as u64, text.len() as u64, 0, 0, 0, 0]);
            }
            logd::TAG_SYNC => {
                let mut s = SHARED.lock();
                s.sync_asked += 1;
                let g = s.sync_asked;
                drop(s);
                FOR_FILE.notify_one();
                reply(msg.sender, logd::TAG_OK, [g, 0, 0, 0, 0, 0]);
            }
            logd::TAG_QUIET => {
                quiet = true;
                // What was passed on before is on the console first: two
                // seconds at most, for a console that takes nothing.
                for _ in 0..400 {
                    if SHARED.lock().console.is_empty() {
                        break;
                    }
                    syscall::sleep_ms(5);
                }
                reply(msg.sender, logd::TAG_OK, [0; 6]);
            }
            logd::TAG_SYNCED => {
                let s = SHARED.lock();
                let done = s.sync_done >= msg.data[0] || !s.files;
                drop(s);
                reply(msg.sender, logd::TAG_OK, [done as u64, 0, 0, 0, 0, 0]);
            }
            _ => reply(msg.sender, logd::TAG_ERROR, [22, 0, 0, 0, 0, 0]),
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall::sys_exit_code(255);
}
