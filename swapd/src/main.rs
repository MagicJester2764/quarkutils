#![no_std]
#![no_main]

//! Where memory goes when there is not enough of it.
//!
//! A machine's memory holds, at any moment, a great deal that is not being
//! used at that moment: a program that is waiting, the part of a program
//! that ran once at the start. When no frame is free the kernel takes
//! pages like that — a program's own, not used since it last looked — and
//! moves them into a memory object whose pager has said it will keep them
//! (`OBJECT_SWAP`). This is that pager, and the object is a file.
//!
//! It is the smallest pager there is, because the kernel does everything
//! that is not reading and writing: it chooses the pages, numbers them,
//! remembers which entry of which program names which number, and frees a
//! number when its page has gone back. Two things are asked of this
//! program:
//!
//! - **Write what is dirty** (`TAG_OBJECT_CLEAN`): each page the kernel has
//!   moved in and nobody has written yet is taken (`OBJECT_TAKE_OUT`),
//!   written to the file at its number, and said to have been written
//!   (`OBJECT_WRITTEN`) — or not to have been, if the disk is full, in
//!   which case the kernel keeps the page in memory rather than give up
//!   the only copy of it.
//! - **Give a page back** (`TAG_PAGE_IN`): read it from the file into the
//!   frame the kernel lends.
//!
//! The file is made when this starts, as long as it was asked to be and
//! with nothing in it: it takes room on the disk only as pages are written
//! to it. What it held when the machine was last on is nobody's — the
//! programs whose pages those were went with the machine.
//!
//! `swapd PATH MEGABYTES`, started by a `start` line in `/etc/init.conf`.
//! A system with no such line has nowhere to write memory out to, and a
//! program that wants more than there is ends, as it always did.
//!
//! **Whoever runs this is trusted with every program's memory**: it is
//! handed their pages and could answer with anything. The capability it
//! asks for (`CapReq::swap()`) is one `init` holds and gives to nothing
//! that does not ask in its manifest; an account has no right that
//! confers it.
//!
//! **It runs as a server**, and that matters: the kernel never takes pages
//! from a driver or a server, so this program, the file server it writes
//! through and the disk driver under that are always in memory — the three
//! things that have to be there to bring anything else back.

use quark_rt::ipc::{Message, PAGER_BIT, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{args, ipc, nameserver, println, syscall, vfs};

quark_rt::manifest!([CapReq::priority(quark_rt::syscall::PRIO_SERVER), CapReq::swap()]);

const PAGE: usize = 4096;
/// Where this program keeps the capability for its object.
const OBJECT_SLOT: usize = 40;
/// The pager's own name for its object, which it has one of.
const COOKIE: u64 = 0x5357_4150;
/// What a request is answered with when it is not done. The kernel asks
/// only whether a page-in was answered with 0.
const REFUSED: u64 = 1;
/// A file is read and written by 32-bit offsets, a megabyte short of four
/// gigabytes at the most.
const MOST_MEGABYTES: u64 = 4095;

/// The page being read or written, where the kernel and the file server
/// are each lent it in turn.
#[repr(align(4096))]
struct Page([u8; PAGE]);
static mut BUFFER: Page = Page([0; PAGE]);

fn buffer() -> &'static mut [u8; PAGE] {
    unsafe { &mut (*core::ptr::addr_of_mut!(BUFFER)).0 }
}

fn number(text: &[u8]) -> Option<u64> {
    if text.is_empty() || text.len() > 6 {
        return None;
    }
    text.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))
}

/// Write every page of the object that is waiting to be written, each to
/// its own place in the file, and say of each how it went.
fn write_out(vfs_tid: usize, file: usize, id: u64) {
    let page = buffer();
    let mut from = 0u64;
    loop {
        let n = syscall::sys_object_ctl(id, syscall::OBJECT_TAKE_OUT, page.as_mut_ptr() as u64, from);
        if n == u64::MAX {
            return;
        }
        let written = vfs::write(vfs_tid, file, &page[..], (n * PAGE as u64) as u32) == Ok(PAGE as u32);
        let _ = syscall::sys_object_ctl(id, syscall::OBJECT_WRITTEN, written as u64, n);
        from = n + 1;
    }
}

/// Read page `n` of the file into the frame `sender` was lent, and answer.
fn read_in(vfs_tid: usize, file: usize, sender: usize, n: u64) {
    let page = buffer();
    let got = vfs::read(vfs_tid, file, &mut page[..], (n * PAGE as u64) as u32);
    let ok = match got {
        // A page nothing was ever written to is not one the kernel asks
        // for; one that comes back short is a file that has been cut.
        Ok(len) if len as usize == PAGE => syscall::sys_lent_write(sender, 0, &page[..]) == Ok(PAGE),
        _ => false,
    };
    let reply = Message { sender: 0, tag: if ok { 0 } else { REFUSED }, data: [0; 6] };
    let _ = syscall::sys_reply(sender, &reply);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let (Some(path), Some(megabytes)) = (args::argv(1), args::argv(2).and_then(number)) else {
        println!("usage: swapd PATH MEGABYTES");
        syscall::sys_exit_code(2);
    };
    if megabytes == 0 || megabytes > MOST_MEGABYTES {
        println!("[swapd] between 1 and {} megabytes.", MOST_MEGABYTES);
        syscall::sys_exit_code(2);
    }
    let bytes = megabytes << 20;
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 50) else {
        println!("[swapd] no file server to keep a file with.");
        syscall::sys_exit_code(1);
    };
    // The right to do this, before anything is done: a file is about to be
    // made empty, and one that is somebody's is not this program's to
    // empty because it was named on a command line by somebody who may not
    // start it.
    let Ok(id) = syscall::sys_object_create(COOKIE, bytes, OBJECT_SLOT) else {
        println!("[swapd] the kernel has no room for another object.");
        syscall::sys_exit_code(1);
    };
    if syscall::sys_object_ctl(id, syscall::OBJECT_SWAP, 0, 0) != 0 {
        println!("[swapd] may not be where memory is written out to: not started with the right to, or something else already is.");
        syscall::sys_exit_code(1);
    }
    // With nothing in it, and then as long as it was asked to be: what it
    // held belonged to programs that are gone, and it takes room on the
    // disk only as pages are written.
    let file = match vfs::open_with(vfs_tid, path, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE) {
        Ok(opened) if !opened.is_dir => opened.handle,
        _ => {
            println!("[swapd] cannot make the file.");
            syscall::sys_exit_code(1);
        }
    };
    if vfs::truncate(vfs_tid, file, bytes).is_err() {
        println!("[swapd] cannot make the file {} megabytes long.", megabytes);
        syscall::sys_exit_code(1);
    }
    println!("[swapd] {} megabytes to write memory out to.", megabytes);

    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        let sender = msg.sender;
        match msg.tag {
            // From the kernel alone: nobody else can set the pager bit.
            ipc::TAG_PAGE_IN if sender & PAGER_BIT != 0 && msg.data[2] == id => {
                read_in(vfs_tid, file, sender, msg.data[1])
            }
            ipc::TAG_OBJECT_CLEAN if sender == 0 => write_out(vfs_tid, file, id),
            // Nothing maps it: no page of anybody's is written out just
            // now. It stays, for the next that is.
            ipc::TAG_OBJECT_IDLE if sender == 0 => {}
            _ if sender != 0 => {
                let refused = Message { sender: 0, tag: REFUSED, data: [0; 6] };
                let _ = syscall::sys_reply(sender, &refused);
            }
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[swapd] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
