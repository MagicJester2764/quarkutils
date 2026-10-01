#![no_std]
#![no_main]

//! Load a font into the console.
//!
//!     setfont /usr/share/consolefonts/unifont.hex
//!
//! The console draws ASCII out of a font it was built with, and everything
//! else out of one it is given. This gives it one: a file in GNU Unifont's
//! `.hex` format — a code point, a colon and the glyph's rows in hexadecimal,
//! one character a line — read here and lent to the console a piece at a
//! time, whole lines to a piece.
//!
//! Read here and not by the console, because a console that called the file
//! server would be waiting on something that may be waiting to print on it.
//!
//! `init` runs it at boot when `/etc/init.conf` says to (`run /usr/bin/setfont
//! <file>`), and it can be run again by hand to change the font. The console
//! takes a font from root only.

use quark_rt::ipc::Message;
use quark_rt::{args, nameserver, println, syscall, vfs};

// No manifest: reading a file takes a buffer to lend the file server, and
// handing it on takes the same buffer lent to the console.

const TAG_FONT: u64 = 0x120;
const FONT_BEGIN: u64 = 1;
const FONT_GLYPHS: u64 = 2;
const FONT_END: u64 = 3;

/// As much as the console takes in one call.
const CHUNK: usize = 32768;
static mut TEXT: [u8; CHUNK] = [0; CHUNK];

fn say(console: usize, what: u64, lent: &[u8]) -> Option<u64> {
    let msg = Message { sender: 0, tag: TAG_FONT, data: [what, lent.len() as u64, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    let sent = if lent.is_empty() {
        syscall::sys_call(console, &msg, &mut reply)
    } else {
        syscall::sys_call_lend(console, &msg, &mut reply, lent)
    };
    (sent.is_ok() && reply.tag == 0).then_some(reply.data[0])
}

fn fail(what: &str) -> ! {
    println!("setfont: {}", what);
    syscall::sys_exit_code(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(path) = args::argv(1) else {
        println!("usage: setfont FILE");
        syscall::sys_exit_code(2);
    };
    let name = core::str::from_utf8(path).unwrap_or("the file");
    let Some(files) = nameserver::lookup_retry(b"vfs", 50) else {
        fail("no file server");
    };
    let Some(console) = nameserver::lookup_retry(b"console", 50) else {
        fail("no console");
    };
    let Ok((handle, size, is_dir)) = vfs::open(files, path) else {
        println!("setfont: {}: not found", name);
        syscall::sys_exit_code(1);
    };
    if is_dir {
        let _ = vfs::close(files, handle);
        println!("setfont: {}: is a directory", name);
        syscall::sys_exit_code(1);
    }

    if say(console, FONT_BEGIN, &[]).is_none() {
        let _ = vfs::close(files, handle);
        fail("the console takes a font from root, and this is not root");
    }

    let text = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };
    let mut offset = 0u32;
    // What is in `text` that has not gone yet: the start of a line whose end
    // the last read did not reach.
    let mut have = 0usize;
    let mut ok = true;
    while ok {
        // Fill what room there is.
        let mut ended = offset >= size;
        while have < CHUNK && !ended {
            let want = (CHUNK - have).min((size - offset) as usize);
            match vfs::read(files, handle, &mut text[have..have + want], offset) {
                Ok(n) if n > 0 => {
                    have += n as usize;
                    offset += n;
                    ended = offset >= size;
                }
                Ok(_) => ended = true,
                Err(_) => {
                    ok = false;
                    ended = true;
                }
            }
        }
        if !ok || have == 0 {
            break;
        }
        // Whole lines only; at the end of the file, whatever is left is one.
        let whole = match text[..have].iter().rposition(|&b| b == b'\n') {
            Some(at) => at + 1,
            None if ended => have,
            // A line longer than a piece is not a glyph.
            None => {
                ok = false;
                break;
            }
        };
        if say(console, FONT_GLYPHS, &text[..whole]).is_none() {
            ok = false;
            break;
        }
        text.copy_within(whole..have, 0);
        have -= whole;
        if ended && have == 0 {
            break;
        }
    }
    let _ = vfs::close(files, handle);

    // The end is said either way: the screen is drawn again in what was
    // loaded, which after a failure is what there was before it went wrong.
    let count = say(console, FONT_END, &[]);
    match (ok, count) {
        (true, Some(n)) if n > 0 => {
            println!("setfont: {} characters from {}", n, name);
            syscall::sys_exit_code(0);
        }
        _ => {
            println!("setfont: {} is not a font the console can use", name);
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("setfont: {}", info);
    syscall::sys_exit_code(255);
}
