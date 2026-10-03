#![no_std]
#![no_main]
#![allow(static_mut_refs)]

//! The text console: what the machine boots into.
//!
//! It draws characters on the whole of the display, and it is two things to
//! the programs that print on it.
//!
//! To the services started before there are files or users, it is a pipe:
//! what they write down it is drawn. That is all it was.
//!
//! To a session it is a *terminal*. Asked for one (`TAG_TTY_OPEN`), it makes
//! a pseudo-terminal, keeps the master, and says which. Whatever opens the
//! slave has a real tty on its standard descriptors: `isatty` is true,
//! `tcsetattr` works, the kernel's line discipline echoes and edits, and a
//! program that wants raw keys — a shell with a line editor — turns the
//! editing off and gets them. What comes out of the master is drawn; what is
//! typed goes into it. Before this a program's standard input was a message
//! to the input server and its output this pipe, and neither was a terminal:
//! bash would not have shown a prompt.
//!
//! With a terminal to feed, the console holds the keyboard the way a
//! compositor does — whoever owns the screen owns the keys — and a compositor
//! that takes the display claims it above the console's, as it always has.
//!
//! What is written to it is UTF-8. A cell holds a character, not a byte; a
//! character is as wide as the program printing it believes it is (`width`),
//! which for most of East Asia is two cells; and what it looks like comes
//! from the font that was loaded, where one has been (`glyphs`). A byte that
//! is not part of any character is drawn as U+FFFD rather than guessed at.

use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::nameserver;
use quark_rt::{println, syscall};

mod glyphs;
mod width;

// A server: programs are usually blocked waiting on this, so it runs
// ahead of them and behind the drivers. No capabilities — everything
// this needs it is given directly or asks another server for.
quark_rt::manifest!([
    quark_rt::manifest::CapReq::priority(quark_rt::syscall::PRIO_SERVER),
]);


const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;

static mut FB: usize = 0;
static mut PITCH: usize = 0;
static mut WIDTH: usize = 0;
static mut HEIGHT: usize = 0;
static mut BPP: usize = 0;
static mut COLS: usize = 0;
static mut ROWS: usize = 0;
static mut COL: usize = 0;
static mut ROW: usize = 0;
static mut R_POS: u8 = 16;
static mut G_POS: u8 = 8;
static mut B_POS: u8 = 0;
static mut INITIALIZED: bool = false;

/// The framebuffer device server, and whether the display is ours right now.
///
/// A compositor can take the screen while this keeps running: the text carries
/// on accumulating in the cell buffer, and is redrawn from it when the display
/// comes back.
static mut FB_TID: usize = 0;
static mut HAVE_DISPLAY: bool = false;

/// Where the framebuffer is mapped.
const FB_VADDR: usize = 0x81_0000_0000;
/// The slot the framebuffer device grants the display into. Fixed by that
/// device, and emptied again when the display goes back.
const FB_LEASE_SLOT: usize = 2;

const TAG_FB_CLAIM: u64 = 2;
const TAG_FB_LOST: u64 = 0x100;
const TAG_FB_GAINED: u64 = 0x101;
const TAG_FB_ERROR: u64 = u64::MAX;

/// Give the caller the console's terminal: the reply's first word is the
/// pty's number, whose slave it then opens.
const TAG_TTY_OPEN: u64 = 0x110;

// The input server, as a claimant sees it.
const TAG_INPUT_CLAIM: u64 = 0x200;
const TAG_INPUT_POLL: u64 = 0x202;
const TAG_INPUT_KEY: u64 = 0x203;

/// The master of the console's terminal, once somebody has asked for one.
static mut TTY_MASTER: usize = usize::MAX;
static mut TTY_NUMBER: usize = 0;
/// The program the terminal was given to. Nobody else is given it while that
/// one lives: whoever holds a terminal's slave reads what is typed.
static mut TTY_OWNER: u64 = 0;
static mut INPUT_TID: usize = 0;

// Escape sequences: ECMA-48's shape. After ESC [ come parameter bytes
// (0x30-0x3F: digits, `;`, and the private markers `<=>?`), intermediate
// bytes (0x20-0x2F), and one final byte (0x40-0x7E) that says what it was.
const NORMAL: u8 = 0;
const ESCAPE: u8 = 1;
const CSI: u8 = 2;
/// ESC and one intermediate: the next byte finishes it (a character set).
const ESC_ONE_MORE: u8 = 3;
/// An operating system command: a title. Everything to BEL or ESC \.
const OSC: u8 = 4;
const OSC_ESC: u8 = 5;
static mut ESC_STATE: u8 = NORMAL;
const MAX_PARAMS: usize = 16;
static mut ESC_PARAMS: [u16; MAX_PARAMS] = [0; MAX_PARAMS];
static mut ESC_PARAM_COUNT: usize = 0;
/// The sequence began `ESC [ ?`: a private mode, most of which are not ours.
static mut ESC_PRIVATE: bool = false;

/// A colour a program asked for: one of the sixteen, or one it spelled out.
/// The sixteen are kept as numbers so that bold can brighten whichever is
/// current, in whichever order the two arrive.
#[derive(Clone, Copy)]
enum Colour {
    Index(u8),
    Rgb(u8, u8, u8),
}

const DEFAULT_FG: Colour = Colour::Index(7);
const DEFAULT_BG: Colour = Colour::Index(0);
static mut FG: Colour = DEFAULT_FG;
static mut BG: Colour = DEFAULT_BG;
static mut BOLD: bool = false;
static mut REVERSE: bool = false;
static mut FG_COLOR: u32 = 0;
static mut BG_COLOR: u32 = 0;
/// Where `ESC [ s` left the cursor.
static mut SAVED: (usize, usize) = (0, 0);
/// A program asked for the cursor not to be drawn (`ESC [ ? 25 l`).
static mut CURSOR_HIDDEN: bool = false;
/// The last column has been written and the cursor has not moved on.
///
/// A terminal does not wrap when a character lands in its last column; it
/// wraps when the *next* one arrives. The difference is a line exactly as
/// wide as the screen followed by a newline: wrapped at once, that is a blank
/// line after every full one, and `ls` fills lines to the edge.
static mut WRAP_PENDING: bool = false;
/// What is being drawn came out of the terminal rather than down the pipe.
///
/// The two differ in what a line feed is. A service printing down the pipe
/// means a new line by it. A terminal's output has been through the line
/// discipline, which puts a carriage return in front where the program wants
/// one — so here a line feed alone is what it says: down, same column.
static mut FROM_TTY: bool = false;

/// The sixteen colours of a PC console: eight, and their bright halves.
const PALETTE: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00), (0xCC, 0x00, 0x00), (0x00, 0xCC, 0x00), (0xCC, 0xCC, 0x00),
    (0x00, 0x00, 0xCC), (0xCC, 0x00, 0xCC), (0x00, 0xCC, 0xCC), (0xCC, 0xCC, 0xCC),
    (0x66, 0x66, 0x66), (0xFF, 0x55, 0x55), (0x55, 0xFF, 0x55), (0xFF, 0xFF, 0x55),
    (0x5C, 0x5C, 0xFF), (0xFF, 0x55, 0xFF), (0x55, 0xFF, 0xFF), (0xFF, 0xFF, 0xFF),
];

// Cursor blink state
static mut CURSOR_VISIBLE: bool = true;
static mut CURSOR_LAST_TOGGLE: u64 = 0;
const CURSOR_BLINK_TICKS: u64 = 50; // 500ms at 100 Hz

// Text cell buffer — avoids expensive framebuffer reads during scroll
const MAX_CELL_COLS: usize = 320;
const MAX_CELL_ROWS: usize = 200;

/// The character in each cell: a code point, 0 for none, or `TAIL` for the
/// right half of a character two cells wide, which is drawn with its left.
static mut CELL_CH: [u32; MAX_CELL_COLS * MAX_CELL_ROWS] = [0; MAX_CELL_COLS * MAX_CELL_ROWS];
const TAIL: u32 = u32::MAX;

/// A character being put together from its bytes: how many more it needs,
/// what there is of it, and the least it may come to — a character spelt in
/// more bytes than it needs is not that character, it is a way round whoever
/// was checking for it.
static mut UTF8_LEFT: u8 = 0;
static mut UTF8_CP: u32 = 0;
static mut UTF8_MIN: u32 = 0;
/// What is drawn for bytes that are not UTF-8.
const REPLACEMENT: u32 = 0xFFFD;

/// A font, loaded by a program that read it from a file: `setfont`.
const TAG_FONT: u64 = 0x120;
const FONT_BEGIN: u64 = 1;
const FONT_GLYPHS: u64 = 2;
const FONT_END: u64 = 3;
/// As much of a font file as arrives in one call.
const FONT_CHUNK: usize = 32768;
static mut FONT_TEXT: [u8; FONT_CHUNK] = [0; FONT_CHUNK];
static mut CELL_FG: [u32; MAX_CELL_COLS * MAX_CELL_ROWS] = [0; MAX_CELL_COLS * MAX_CELL_ROWS];
static mut CELL_BG: [u32; MAX_CELL_COLS * MAX_CELL_ROWS] = [0; MAX_CELL_COLS * MAX_CELL_ROWS];
static mut DIRTY_MIN: usize = usize::MAX;
static mut DIRTY_MAX: usize = 0;

fn cell_idx(col: usize, row: usize) -> usize {
    row * MAX_CELL_COLS + col
}

unsafe fn mark_dirty(row: usize) {
    if row < DIRTY_MIN { DIRTY_MIN = row; }
    if row > DIRTY_MAX { DIRTY_MAX = row; }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("[console] Started.");

    if !claim_display() {
        println!("[console] No display; nothing to draw on.");
        syscall::sys_exit_code(1);
    }

    // Register with nameserver
    if nameserver::register(b"console").is_ok() {
        println!("[console] Registered with nameserver.");
    }

    println!("[console] Ready.");

    unsafe { CURSOR_LAST_TOGGLE = syscall::sys_ticks(); }

    // Main loop: draw what has been written, type what has been typed, and
    // when there is neither, wait a tick and blink.
    //
    // Waiting a tick rather than yielding in a loop. There is no way to block
    // on a pipe, a terminal and IPC at once, so idling here means asking each
    // again shortly — but a yield loop asks as fast as the machine will go,
    // forever. That is a whole core spent on an idle terminal, and it was
    // enough to keep the keyboard driver from being scheduled while somebody
    // typed.
    let mut pipe_open = true;
    let mut passes = 0u32;
    loop {
        let mut buf = [0u8; 256];
        let mut busy = false;

        // What the pipe has is drawn to the end of a line before anything
        // the terminal has: a line from each is two lines. Read a piece at a
        // time and drawn in between, a line of the boot was cut where a
        // piece ended and the login prompt that came after it was drawn
        // inside it — on four processors, where the two arrive together.
        for _ in 0..64 {
            if !pipe_open {
                break;
            }
            match syscall::sys_fd_read_nb(0, &mut buf) {
                // Every writer has gone. That is the end of the pipe and not
                // of this program: it is a server with a name, and somebody
                // may yet ask it for its terminal.
                0 => pipe_open = false,
                n if n == syscall::WOULD_BLOCK || n == u64::MAX => break,
                n => {
                    let n = n as usize;
                    draw(&buf[..n], false);
                    busy = true;
                    if buf[n - 1] == b'\n' {
                        break;
                    }
                }
            }
        }

        let master = unsafe { TTY_MASTER };
        if master != usize::MAX {
            match syscall::sys_fd_read_nb(master, &mut buf) {
                // 0 is every slave closed: a session between two logins.
                0 => {}
                n if n == syscall::WOULD_BLOCK || n == u64::MAX => {}
                n => {
                    draw(&buf[..n as usize], true);
                    busy = true;
                }
            }
            if unsafe { HAVE_DISPLAY } && pump_keys(master) {
                busy = true;
            }
        }

        if busy {
            // Still look for word from the framebuffer device, and for
            // somebody asking for the terminal: a program printing without
            // pause must not be able to hold the display against a
            // compositor that has been given it.
            passes += 1;
            if passes >= 16 {
                passes = 0;
                serve(0);
            }
            continue;
        }
        passes = 0;
        serve(1);
        let now = syscall::sys_ticks();
        unsafe {
            if now.wrapping_sub(CURSOR_LAST_TOGGLE) >= CURSOR_BLINK_TICKS {
                CURSOR_VISIBLE = !CURSOR_VISIBLE;
                CURSOR_LAST_TOGGLE = now;
                draw_cursor();
            }
        }
    }
}

/// Draw what a program wrote, with the cursor out of the way while it is.
fn draw(bytes: &[u8], from_tty: bool) {
    unsafe {
        FROM_TTY = from_tty;
        hide_cursor();
    }
    write_bytes(bytes);
    unsafe {
        CURSOR_VISIBLE = true;
        CURSOR_LAST_TOGGLE = syscall::sys_ticks();
        draw_cursor();
    }
}

/// The console's terminal, made the first time somebody asks for it.
fn tty_number() -> Option<usize> {
    unsafe {
        if TTY_MASTER != usize::MAX {
            return Some(TTY_NUMBER);
        }
        let master = syscall::sys_pty_create().ok()?;
        let Ok(number) = syscall::sys_pty_number(master) else {
            let _ = syscall::sys_fd_close(master);
            return None;
        };
        // How big it is, for the program that lays itself out to fit: `ls`
        // asks, and so does a shell's line editor.
        let _ = syscall::sys_pty_set_size(master, ROWS as u16, COLS as u16);
        TTY_MASTER = master;
        TTY_NUMBER = number;
        Some(number)
    }
}

/// What a key is to a terminal: the bytes a program reads when it is pressed.
///
/// A character is itself — the driver has already applied Shift and Ctrl —
/// except that Return is a carriage return, which the line discipline turns
/// into the newline a program expects, and Backspace is DEL, which is what
/// terminals send. A key with no character is the escape sequence a Linux
/// console sends for it. Alt sends an escape first, which is how a line
/// editor is told "meta".
fn key_bytes(ascii: u8, code: u8, modifiers: u8, out: &mut [u8; 8]) -> usize {
    const ALT: u8 = 1 << 2;
    let seq: &[u8] = match (ascii, code) {
        (b'\n', _) => b"\r",
        (8, _) => b"\x7f",
        (0, 103) => b"\x1b[A",  // Up
        (0, 108) => b"\x1b[B",  // Down
        (0, 106) => b"\x1b[C",  // Right
        (0, 105) => b"\x1b[D",  // Left
        (0, 102) => b"\x1b[1~", // Home
        (0, 110) => b"\x1b[2~", // Insert
        (0, 111) => b"\x1b[3~", // Delete
        (0, 107) => b"\x1b[4~", // End
        (0, 104) => b"\x1b[5~", // Page Up
        (0, 109) => b"\x1b[6~", // Page Down
        (0, _) => b"",
        _ => {
            let mut n = 0;
            if modifiers & ALT != 0 {
                out[n] = 0x1b;
                n += 1;
            }
            out[n] = ascii;
            return n + 1;
        }
    };
    out[..seq.len()].copy_from_slice(seq);
    seq.len()
}

/// Take what has been typed and write it into the terminal. True if anything
/// was.
///
/// Bounded, so that a key held down cannot keep this from getting back to
/// drawing. What is left stays with the driver and arrives next time round.
fn pump_keys(master: usize) -> bool {
    let input = unsafe {
        if INPUT_TID == 0 {
            // The input server is started after this one. Ask once a pass
            // until it is there; then say the keys are ours while the display
            // is.
            let Some(tid) = nameserver::lookup(b"input") else {
                return false;
            };
            let claim = Message { sender: 0, tag: TAG_INPUT_CLAIM, data: [0; 6] };
            let mut reply = Message::empty();
            if syscall::sys_call(tid, &claim, &mut reply).is_err() || reply.tag == u64::MAX {
                return false;
            }
            INPUT_TID = tid;
        }
        INPUT_TID
    };
    let mut typed = false;
    for _ in 0..32 {
        let poll = Message { sender: 0, tag: TAG_INPUT_POLL, data: [0; 6] };
        let mut reply = Message::empty();
        // Timed: a keyboard that has stopped answering costs the keys, not
        // the screen.
        match syscall::sys_call_timeout(input, &poll, &mut reply, 20) {
            syscall::CallOutcome::Replied if reply.tag == TAG_INPUT_KEY => {}
            // Nothing typed — or the keys are somebody else's for now, which
            // is the same answer to this.
            _ => break,
        }
        if reply.data[0] == 0 {
            continue; // a release
        }
        let mut bytes = [0u8; 8];
        let n = key_bytes(reply.data[1] as u8, reply.data[2] as u8, reply.data[3] as u8, &mut bytes);
        if n > 0 {
            // A terminal whose program is not reading drops what does not
            // fit, as a full keyboard buffer does.
            let _ = syscall::sys_fd_write_nb(master, &bytes[..n]);
            typed = true;
        }
    }
    typed
}

/// Take the display and start drawing on it.
///
/// The console is an ordinary client of the framebuffer device: it asks for
/// the screen, is given the right to map it, and draws text across the whole
/// of it. No frame, no title bar — this is the text console the machine boots
/// into, and it is the only thing on the screen until something else asks for
/// it.
fn claim_display() -> bool {
    let Some(fb) = nameserver::lookup_retry(b"fb", 30) else {
        return false;
    };
    unsafe { FB_TID = fb };

    // With the right to call this console on offer: the device has to be able
    // to say when somebody else takes the display, and when it comes back.
    let msg = Message { sender: 0, tag: TAG_FB_CLAIM, data: [0; 6] };
    let mut reply = Message::empty();
    if syscall::sys_call_offer_self(fb, &msg, &mut reply).is_err() || reply.tag == TAG_FB_ERROR {
        println!("[console] the framebuffer would not give up the display");
        return false;
    }
    adopt_mode(&reply)
}

/// Map the framebuffer and lay the text grid out on it.
fn adopt_mode(reply: &Message) -> bool {
    let w = (reply.data[0] >> 32) as usize;
    let h = (reply.data[0] & 0xFFFF_FFFF) as usize;
    let pitch = (reply.data[1] >> 32) as usize;
    let bpp = (reply.data[1] & 0xFF) as usize;
    let phys = reply.data[3] as usize;

    let pages = (pitch * h + 4095) / 4096;
    if syscall::sys_map_phys(phys, FB_VADDR, pages).is_err() {
        println!("[console] could not map the framebuffer");
        return false;
    }

    unsafe {
        FB = FB_VADDR;
        PITCH = pitch;
        WIDTH = w;
        HEIGHT = h;
        BPP = bpp;
        R_POS = ((reply.data[2] >> 16) & 0xFF) as u8;
        G_POS = ((reply.data[2] >> 8) & 0xFF) as u8;
        B_POS = (reply.data[2] & 0xFF) as u8;
        COLS = (w / GLYPH_W).min(MAX_CELL_COLS);
        ROWS = (h / GLYPH_H).min(MAX_CELL_ROWS);
        // The pixel layout may have changed with the mode; the attributes
        // have not.
        recolor();
        HAVE_DISPLAY = true;
        INITIALIZED = true;
    }
    true
}

/// Unmap the framebuffer, however many pages that is.
///
/// `sys_munmap` takes at most 256 pages a call and a screenful is four times
/// that, so this loops. Leaving it mapped would mean holding a window onto the
/// screen after the right to do so had been revoked — revocation governs the
/// right to map, not mappings that already exist.
unsafe fn unmap_framebuffer(bytes: usize) {
    const MUNMAP_MAX: usize = 256;
    let pages = (bytes + 4095) / 4096;
    let mut done = 0;
    while done < pages {
        let chunk = (pages - done).min(MUNMAP_MAX);
        let _ = syscall::sys_munmap(FB_VADDR + done * 4096, chunk);
        done += chunk;
    }
}

/// Redraw every cell. Used when the display comes back from a compositor:
/// what is on the screen is whatever that left there.
fn redraw_all() {
    if !unsafe { HAVE_DISPLAY } {
        return;
    }
    unsafe {
        core::ptr::write_bytes(FB as *mut u8, 0, PITCH * HEIGHT);
        DIRTY_MIN = 0;
        DIRTY_MAX = ROWS.saturating_sub(1);
    }
    flush_dirty();
}

/// One step of loading a font: begin, a piece of the file, or end. Returns
/// how many characters the font has so far.
///
/// From root and nobody else. What the console's characters look like is
/// what everything on it is read by, and a program that could change them
/// could make a prompt say anything.
///
/// The console does not read the file. A server that called the file server
/// would be waiting on something that may be waiting to print on it; so the
/// file is read by whoever is loading the font, and lent here a piece at a
/// time, whole lines to a piece.
fn load_font(msg: &Message) -> Option<usize> {
    let (uid, _) = syscall::sys_get_tuid(msg.sender).ok()?;
    if uid != 0 {
        return None;
    }
    match msg.data[0] {
        FONT_BEGIN => glyphs::clear(),
        FONT_GLYPHS => {
            let len = msg.data[1] as usize;
            if len > FONT_CHUNK {
                return None;
            }
            let text = unsafe { &mut FONT_TEXT[..len] };
            let mut have = 0;
            while have < len {
                match syscall::sys_lent_read(msg.sender, have, &mut text[have..]) {
                    Ok(n) if n > 0 => have += n,
                    _ => return None,
                }
            }
            glyphs::load_hex(text).ok()?;
        }
        // Everything on the screen is drawn again, in the font it has now.
        FONT_END => redraw_all(),
        _ => return None,
    }
    Some(glyphs::count())
}

/// Answer whoever is calling, waiting up to `ticks` for somebody to.
///
/// Two callers matter. The framebuffer device says when the display is taken
/// and when it comes back; and whatever is going to run a session asks for
/// the console's terminal. Zero polls and returns; anything else is how this
/// server idles.
fn serve(ticks: u64) {
    let mut msg = Message::empty();
    if syscall::sys_recv_timeout(TID_ANY, &mut msg, ticks).is_err() {
        return;
    }
    // Only the device can take the display away or give it back. Anybody
    // else saying so would blank the console.
    let from_fb = msg.sender == unsafe { FB_TID };
    match msg.tag {
        TAG_FB_LOST if from_fb => {
            // Stop drawing before answering: the reply is what lets the new
            // owner start, and two programs writing the same pixels is the
            // thing this protocol exists to prevent.
            unsafe {
                HAVE_DISPLAY = false;
                unmap_framebuffer(PITCH * HEIGHT);
            }
            // Drop the capability along with the mapping. A granted slot must
            // be empty to be granted into again, so keeping a revoked one
            // means the display can never be handed back.
            let _ = syscall::sys_cap_delete(FB_LEASE_SLOT);
            let ack = Message { sender: 0, tag: 0, data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &ack);
        }
        TAG_FB_GAINED if from_fb => {
            let ok = adopt_mode(&msg);
            let ack = Message { sender: 0, tag: 0, data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &ack);
            if ok {
                redraw_all();
            }
        }
        TAG_TTY_OPEN => {
            // To the first program that asks, and to nobody else while that
            // one lives: the terminal's slave is where what is typed goes.
            let asker = syscall::sys_task_space(msg.sender).unwrap_or(0);
            let free = unsafe {
                TTY_OWNER == 0 || TTY_OWNER == asker || syscall::sys_space_watch(TTY_OWNER).is_err()
            };
            let reply = match (free && asker != 0).then(tty_number).flatten() {
                Some(number) => {
                    unsafe { TTY_OWNER = asker };
                    Message { sender: 0, tag: 0, data: [number as u64, 0, 0, 0, 0, 0] }
                }
                None => Message { sender: 0, tag: u64::MAX, data: [0; 6] },
            };
            let _ = syscall::sys_reply(msg.sender, &reply);
        }
        TAG_FONT => {
            let reply = match load_font(&msg) {
                Some(count) => Message { sender: 0, tag: 0, data: [count as u64, 0, 0, 0, 0, 0] },
                None => Message { sender: 0, tag: u64::MAX, data: [0; 6] },
            };
            let _ = syscall::sys_reply(msg.sender, &reply);
        }
        // From the kernel, about a program this watched to see whether the
        // terminal was free: nothing to answer.
        _ if msg.sender == 0 => {}
        _ => {
            let ack = Message { sender: 0, tag: u64::MAX, data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &ack);
        }
    }
}

fn write_bytes(s: &[u8]) {
    unsafe {
        if !INITIALIZED {
            return;
        }
    }
    for &b in s {
        putc(b);
    }
    flush_dirty();
}

fn putc(c: u8) {
    unsafe {
        match ESC_STATE {
            // A control in the middle of a character ends it there: what
            // there was of it was not one.
            NORMAL if UTF8_LEFT != 0 && !(0x80..=0xbf).contains(&c) => {
                UTF8_LEFT = 0;
                put_char(REPLACEMENT);
                return putc(c);
            }
            NORMAL => match c {
                0x1b => ESC_STATE = ESCAPE,
                b'\n' => {
                    if !FROM_TTY {
                        COL = 0;
                    }
                    ROW += 1;
                    WRAP_PENDING = false;
                }
                b'\r' => {
                    COL = 0;
                    WRAP_PENDING = false;
                }
                b'\t' => {
                    let next = (COL + 8) & !7;
                    COL = if next < COLS { next } else { COLS - 1 };
                }
                0x08 => {
                    COL = COL.saturating_sub(1);
                    WRAP_PENDING = false;
                }
                // The bell, and the rest of the controls nothing here acts
                // on: not characters, so not drawn.
                0..=0x1f | 0x7f => {}
                0x20..=0x7e => put_char(c as u32),
                // The rest of a character.
                0x80..=0xbf => {
                    if UTF8_LEFT == 0 {
                        put_char(REPLACEMENT);
                    } else {
                        UTF8_CP = UTF8_CP << 6 | (c & 0x3f) as u32;
                        UTF8_LEFT -= 1;
                        if UTF8_LEFT == 0 {
                            let cp = UTF8_CP;
                            let real = cp >= UTF8_MIN
                                && cp <= 0x10FFFF
                                && !(0xD800..=0xDFFF).contains(&cp);
                            put_char(if real { cp } else { REPLACEMENT });
                        }
                    }
                }
                // The first byte of one, which says how many follow.
                0xc0..=0xdf => (UTF8_LEFT, UTF8_CP, UTF8_MIN) = (1, (c & 0x1f) as u32, 0x80),
                0xe0..=0xef => (UTF8_LEFT, UTF8_CP, UTF8_MIN) = (2, (c & 0x0f) as u32, 0x800),
                0xf0..=0xf7 => (UTF8_LEFT, UTF8_CP, UTF8_MIN) = (3, (c & 0x07) as u32, 0x10000),
                _ => put_char(REPLACEMENT),
            },
            ESCAPE => match c {
                b'[' => {
                    ESC_STATE = CSI;
                    ESC_PARAMS = [0; MAX_PARAMS];
                    ESC_PARAM_COUNT = 0;
                    ESC_PRIVATE = false;
                }
                b']' => ESC_STATE = OSC,
                // A character set, or a line attribute: one more byte.
                b'(' | b')' | b'*' | b'+' | b'#' | b'%' => ESC_STATE = ESC_ONE_MORE,
                b'7' => {
                    SAVED = (COL, ROW);
                    ESC_STATE = NORMAL;
                }
                b'8' => {
                    (COL, ROW) = SAVED;
                    WRAP_PENDING = false;
                    ESC_STATE = NORMAL;
                }
                // Reset: as it was when it started.
                b'c' => {
                    reset_attributes();
                    for row in 0..ROWS {
                        erase(row, 0, COLS);
                    }
                    (COL, ROW) = (0, 0);
                    WRAP_PENDING = false;
                    CURSOR_HIDDEN = false;
                    ESC_STATE = NORMAL;
                }
                // Anything else is a two-byte sequence this does nothing
                // with, and it is over.
                _ => ESC_STATE = NORMAL,
            },
            ESC_ONE_MORE => ESC_STATE = NORMAL,
            // A title, for a window this has not got. Swallowed whole: it
            // ends at a bell, or at ESC \.
            OSC => match c {
                0x07 => ESC_STATE = NORMAL,
                0x1b => ESC_STATE = OSC_ESC,
                _ => {}
            },
            OSC_ESC => ESC_STATE = if c == b'\\' { NORMAL } else { OSC },
            CSI => match c {
                b'0'..=b'9' => {
                    if ESC_PARAM_COUNT < MAX_PARAMS {
                        let p = &mut ESC_PARAMS[ESC_PARAM_COUNT];
                        *p = p.saturating_mul(10).saturating_add((c - b'0') as u16);
                    }
                }
                // A colon separates the parts of one parameter where a
                // semicolon separates parameters; the only sequences that use
                // one are colours, which are read the same either way.
                b';' | b':' => {
                    if ESC_PARAM_COUNT < MAX_PARAMS {
                        ESC_PARAM_COUNT += 1;
                    }
                }
                // A private marker: the sequence is somebody's extension.
                // Read to its end like any other, and acted on only where
                // this knows what it means. It used to be taken for the end
                // of the sequence, and the rest of it drawn as text.
                b'<' | b'=' | b'>' | b'?' => ESC_PRIVATE = true,
                // Intermediates: part of the sequence, saying nothing here.
                0x20..=0x2f => {}
                0x40..=0x7e => {
                    if ESC_PARAM_COUNT < MAX_PARAMS {
                        ESC_PARAM_COUNT += 1;
                    }
                    dispatch_csi(c);
                    ESC_STATE = NORMAL;
                }
                // Not part of any sequence: give up on this one.
                _ => ESC_STATE = NORMAL,
            },
            _ => ESC_STATE = NORMAL,
        }
        if ROW >= ROWS {
            scroll();
        }
    }
}

/// Put a character where the cursor is and move the cursor past it.
///
/// A character that combines with the one before it takes no cell, and there
/// is nowhere here to draw two characters in one, so it is dropped. One that
/// is two cells wide and has only one left on the line goes on the next, as
/// it would on any terminal: half a character is not a character.
unsafe fn put_char(cp: u32) {
    unsafe {
        // The C1 controls, as characters: nothing to draw and nothing to do.
        if (0x80..0xa0).contains(&cp) {
            return;
        }
        let wide = match width::of(cp) {
            0 => return,
            w => w == 2 && COLS >= 2,
        };
        if WRAP_PENDING || (wide && COL + 1 >= COLS) {
            WRAP_PENDING = false;
            COL = 0;
            ROW += 1;
            if ROW >= ROWS {
                scroll();
            }
        }
        set_cell(COL, ROW, cp);
        let cells = if wide {
            set_cell(COL + 1, ROW, TAIL);
            2
        } else {
            1
        };
        if COL + cells >= COLS {
            COL = COLS - 1;
            WRAP_PENDING = true;
        } else {
            COL += cells;
        }
    }
}

/// Is the cell at `col` the left half of a character two cells wide?
unsafe fn is_head(col: usize, row: usize) -> bool {
    unsafe { col + 1 < COLS && CELL_CH[cell_idx(col + 1, row)] == TAIL }
}

/// Empty one cell, and with it the other half of a wide character it was
/// half of: neither half means anything alone.
unsafe fn clear_cell(col: usize, row: usize) {
    unsafe {
        let was_tail = CELL_CH[cell_idx(col, row)] == TAIL;
        let was_head = is_head(col, row);
        for c in [Some(col), (was_tail && col > 0).then(|| col - 1), was_head.then(|| col + 1)]
            .into_iter()
            .flatten()
        {
            let idx = cell_idx(c, row);
            CELL_CH[idx] = 0;
            CELL_FG[idx] = 0;
            CELL_BG[idx] = 0;
        }
    }
}

/// Put `ch` in a cell, in the colours characters are being drawn in.
unsafe fn set_cell(col: usize, row: usize, ch: u32) {
    unsafe {
        clear_cell(col, row);
        let idx = cell_idx(col, row);
        CELL_CH[idx] = ch;
        CELL_FG[idx] = FG_COLOR;
        CELL_BG[idx] = BG_COLOR;
        mark_dirty(row);
    }
}

/// Draw the character in a cell. One that is two cells wide is drawn from its
/// left cell across both; its right cell draws nothing of its own.
fn draw_glyph(col: usize, row: usize, ch: u32, fg: u32, bg: u32) {
    if !unsafe { HAVE_DISPLAY } || ch == TAIL {
        return;
    }
    let wide_cell = unsafe { is_head(col, row) };
    let (rows, glyph_wide): (&[u8], bool) = match glyphs::of(ch) {
        glyphs::Glyph::Narrow(r) => (r, false),
        glyphs::Glyph::Wide(r) => (r, true),
    };

    let pixel_x = col * GLYPH_W;
    let pixel_y = row * GLYPH_H;
    // As many pixels across as the character has cells. A wide glyph in one
    // cell shows its left half, and a narrow one in two is followed by a
    // blank: the cells are what the program was told, and the font may
    // disagree with them.
    let across = if wide_cell { 2 * GLYPH_W } else { GLYPH_W };

    unsafe {
        let bytes_per_pixel = BPP / 8;

        for gy in 0..GLYPH_H {
            let bits: u16 = if glyph_wide {
                (rows[2 * gy] as u16) << 8 | rows[2 * gy + 1] as u16
            } else {
                (rows[gy] as u16) << 8
            };
            let y = pixel_y + gy;
            let row_base = FB + y * PITCH + pixel_x * bytes_per_pixel;

            for gx in 0..across {
                let on = (bits >> (15 - gx)) & 1 != 0;
                let color = if on { fg } else { bg };
                let px = row_base + gx * bytes_per_pixel;

                if bytes_per_pixel == 4 {
                    (px as *mut u32).write_volatile(color);
                } else if bytes_per_pixel == 3 {
                    let ptr = px as *mut u8;
                    ptr.write_volatile(color as u8);
                    ptr.add(1).write_volatile((color >> 8) as u8);
                    ptr.add(2).write_volatile((color >> 16) as u8);
                }
            }
        }
    }
}

unsafe fn encode_color(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << R_POS | (g as u32) << G_POS | (b as u32) << B_POS
}

/// Empty the cells of `row` from `from` up to `to`.
unsafe fn erase(row: usize, from: usize, to: usize) {
    unsafe {
        for c in from..to.min(COLS) {
            clear_cell(c, row);
        }
        mark_dirty(row);
    }
}

/// The colour a program means by a number: the sixteen, then a cube of six
/// levels of each of red, green and blue, then twenty-four greys.
fn indexed(n: u8) -> (u8, u8, u8) {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..=15 => PALETTE[n as usize],
        16..=231 => {
            let n = n - 16;
            (LEVELS[(n / 36) as usize], LEVELS[(n / 6 % 6) as usize], LEVELS[(n % 6) as usize])
        }
        _ => {
            let grey = 8 + 10 * (n - 232);
            (grey, grey, grey)
        }
    }
}

/// What the next character is drawn in, worked out from the attributes.
unsafe fn recolor() {
    unsafe {
        let rgb = |c: Colour| match c {
            Colour::Index(i) => indexed(i),
            Colour::Rgb(r, g, b) => (r, g, b),
        };
        // Bold is bright, as on every PC console: the font has one weight.
        let fg = match FG {
            Colour::Index(i) if BOLD && i < 8 => Colour::Index(i + 8),
            other => other,
        };
        let (fg, bg) = if REVERSE { (BG, fg) } else { (fg, BG) };
        let (r, g, b) = rgb(fg);
        FG_COLOR = encode_color(r, g, b);
        let (r, g, b) = rgb(bg);
        BG_COLOR = encode_color(r, g, b);
    }
}

unsafe fn reset_attributes() {
    unsafe {
        FG = DEFAULT_FG;
        BG = DEFAULT_BG;
        BOLD = false;
        REVERSE = false;
        recolor();
    }
}

/// Answer a program that asked the terminal something: the answer is typed
/// at it, which is how a terminal says anything.
fn answer(bytes: &[u8]) {
    unsafe {
        if FROM_TTY && TTY_MASTER != usize::MAX {
            let _ = syscall::sys_fd_write_nb(TTY_MASTER, bytes);
        }
    }
}

/// `n` in decimal at `out[at..]`; returns where the digits end.
fn put_number(out: &mut [u8], mut at: usize, n: usize) -> usize {
    let mut digits = [0u8; 20];
    let mut len = 0;
    let mut n = n;
    loop {
        digits[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    while len > 0 && at < out.len() {
        len -= 1;
        out[at] = digits[len];
        at += 1;
    }
    at
}

fn dispatch_csi(cmd: u8) {
    unsafe {
        let p0 = ESC_PARAMS[0] as usize;
        let p1 = ESC_PARAMS[1] as usize;
        if ESC_PRIVATE {
            // The one private mode that is this console's to honour: whether
            // the cursor is drawn. The rest — bracketed paste, alternate
            // screens, mouse reporting — are accepted and mean nothing here.
            if p0 == 25 && (cmd == b'h' || cmd == b'l') {
                CURSOR_HIDDEN = cmd == b'l';
            }
            return;
        }
        // Everything that moves the cursor leaves the right-hand edge.
        if matches!(cmd, b'A'..=b'H' | b'a' | b'd' | b'e' | b'f' | b'`' | b'u') {
            WRAP_PENDING = false;
        }
        match cmd {
            b'A' => ROW = ROW.saturating_sub(p0.max(1)),
            b'B' | b'e' => ROW = (ROW + p0.max(1)).min(ROWS - 1),
            b'C' | b'a' => COL = (COL + p0.max(1)).min(COLS - 1),
            b'D' => COL = COL.saturating_sub(p0.max(1)),
            b'E' => {
                ROW = (ROW + p0.max(1)).min(ROWS - 1);
                COL = 0;
            }
            b'F' => {
                ROW = ROW.saturating_sub(p0.max(1));
                COL = 0;
            }
            b'G' | b'`' => COL = p0.max(1).min(COLS) - 1,
            b'd' => ROW = p0.max(1).min(ROWS) - 1,
            b'H' | b'f' => {
                ROW = p0.max(1).min(ROWS) - 1;
                COL = p1.max(1).min(COLS) - 1;
            }
            b'J' => match p0 {
                // From the cursor to the end of the screen.
                0 => {
                    erase(ROW, COL, COLS);
                    for row in ROW + 1..ROWS {
                        erase(row, 0, COLS);
                    }
                }
                // From the start of the screen to the cursor.
                1 => {
                    for row in 0..ROW {
                        erase(row, 0, COLS);
                    }
                    erase(ROW, 0, COL + 1);
                }
                // All of it. The cursor stays where it is: a program that
                // wants it at the top says so, and every one of them does.
                _ => {
                    for row in 0..ROWS {
                        erase(row, 0, COLS);
                    }
                }
            },
            b'K' => match p0 {
                0 => erase(ROW, COL, COLS),
                1 => erase(ROW, 0, COL + 1),
                _ => erase(ROW, 0, COLS),
            },
            b'm' => {
                let params = &ESC_PARAMS[..ESC_PARAM_COUNT];
                let mut i = 0;
                while i < params.len() {
                    // 38 and 48 are a colour spelled out, and what follows
                    // them is the colour, not more attributes: 5 and a
                    // number, or 2 and three. Read as attributes, `38;5;1`
                    // is bold.
                    let spelled = match (params[i], params.get(i + 1)) {
                        (38 | 48, Some(5)) if i + 2 < params.len() => {
                            Some((Colour::Index(params[i + 2] as u8), 3))
                        }
                        (38 | 48, Some(2)) if i + 4 < params.len() => Some((
                            Colour::Rgb(params[i + 2] as u8, params[i + 3] as u8, params[i + 4] as u8),
                            5,
                        )),
                        // Cut short: nothing after it can be trusted.
                        (38 | 48, _) => break,
                        _ => None,
                    };
                    match spelled {
                        Some((colour, used)) => {
                            if params[i] == 38 {
                                FG = colour;
                            } else {
                                BG = colour;
                            }
                            i += used;
                        }
                        None => {
                            apply_sgr(params[i]);
                            i += 1;
                        }
                    }
                }
                recolor();
            }
            // "Where is the cursor?", "are you there?" and "what are you?"
            // are asked by programs that then wait for an answer.
            b'n' if p0 == 6 => {
                let mut out = [0u8; 16];
                out[..2].copy_from_slice(b"\x1b[");
                let mut at = put_number(&mut out, 2, ROW + 1);
                out[at] = b';';
                at = put_number(&mut out, at + 1, COL + 1);
                out[at] = b'R';
                answer(&out[..at + 1]);
            }
            b'n' if p0 == 5 => answer(b"\x1b[0n"),
            b'c' if p0 == 0 => answer(b"\x1b[?6c"),
            b's' => SAVED = (COL, ROW),
            b'u' => (COL, ROW) = SAVED,
            _ => {}
        }
    }
}

fn apply_sgr(code: u16) {
    unsafe {
        match code {
            0 => {
                FG = DEFAULT_FG;
                BG = DEFAULT_BG;
                BOLD = false;
                REVERSE = false;
            }
            1 => BOLD = true,
            7 => REVERSE = true,
            22 => BOLD = false,
            27 => REVERSE = false,
            30..=37 => FG = Colour::Index((code - 30) as u8),
            39 => FG = DEFAULT_FG,
            40..=47 => BG = Colour::Index((code - 40) as u8),
            49 => BG = DEFAULT_BG,
            90..=97 => FG = Colour::Index((code - 90) as u8 + 8),
            100..=107 => BG = Colour::Index((code - 100) as u8 + 8),
            // Underline, blink, italics: nothing this font can show.
            _ => {}
        }
    }
}

/// Move everything up one line.
///
/// The pixel half is skipped when the display belongs to somebody else, but
/// the cell buffer is not: that is the text, and it is what the screen is
/// redrawn from when the display comes back.
fn scroll() {
    unsafe {
        // Flush any pending dirty rows to the framebuffer BEFORE scrolling pixels,
        // so the FB is in sync when we copy pixels upward.
        flush_dirty();

        let stride = MAX_CELL_COLS;
        let used = ROWS * stride;

        // Shift cell arrays up by one row (fast — normal cached RAM)
        core::ptr::copy(
            CELL_CH.as_ptr().add(stride),
            CELL_CH.as_mut_ptr(),
            used - stride,
        );
        core::ptr::copy(
            CELL_FG.as_ptr().add(stride),
            CELL_FG.as_mut_ptr(),
            used - stride,
        );
        core::ptr::copy(
            CELL_BG.as_ptr().add(stride),
            CELL_BG.as_mut_ptr(),
            used - stride,
        );

        // Clear last cell row
        let last = (ROWS - 1) * stride;
        for i in last..last + COLS {
            CELL_CH[i] = 0;
            CELL_FG[i] = 0;
            CELL_BG[i] = 0;
        }

        // Scroll framebuffer pixels up by one text row instead of a full
        // redraw, so pre-existing content (e.g. kernel boot text) is preserved.
        if HAVE_DISPLAY {
            let shift = GLYPH_H * PITCH;
            let total = ROWS * GLYPH_H * PITCH;
            core::ptr::copy(
                (FB + shift) as *const u8,
                FB as *mut u8,
                total - shift,
            );

            // Clear the last text row in the framebuffer
            core::ptr::write_bytes(
                (FB + (ROWS - 1) * GLYPH_H * PITCH) as *mut u8,
                0,
                GLYPH_H * PITCH,
            );
        }

        ROW = ROWS - 1;
    }
}

fn flush_dirty() {
    unsafe {
        if !INITIALIZED || !HAVE_DISPLAY || DIRTY_MIN > DIRTY_MAX {
            return;
        }
        let min = DIRTY_MIN;
        let max = if DIRTY_MAX >= ROWS { ROWS - 1 } else { DIRTY_MAX };
        for row in min..=max {
            for col in 0..COLS {
                let idx = cell_idx(col, row);
                draw_glyph(col, row, CELL_CH[idx], CELL_FG[idx], CELL_BG[idx]);
            }
        }
        DIRTY_MIN = usize::MAX;
        DIRTY_MAX = 0;
    }
}

/// Draw cursor block at current position.
unsafe fn draw_cursor() {
    if !INITIALIZED || !HAVE_DISPLAY { return; }
    if CURSOR_VISIBLE && !CURSOR_HIDDEN {
        // Draw a solid block at (COL, ROW) using FG_COLOR
        draw_cursor_block(FG_COLOR);
    } else {
        // Restore the cell content at cursor position
        hide_cursor();
    }
}

/// Erase cursor by redrawing the cell content at cursor position.
unsafe fn hide_cursor() {
    if !HAVE_DISPLAY {
        return;
    }
    if !INITIALIZED { return; }
    if COL < COLS && ROW < ROWS {
        // On the right half of a wide character, what is under the cursor is
        // that character's, and it is drawn from its left.
        let col = if CELL_CH[cell_idx(COL, ROW)] == TAIL && COL > 0 { COL - 1 } else { COL };
        let idx = cell_idx(col, ROW);
        draw_glyph(col, ROW, CELL_CH[idx], CELL_FG[idx], CELL_BG[idx]);
    }
}

/// Draw a solid underline cursor (bottom 2 rows of the glyph cell).
unsafe fn draw_cursor_block(color: u32) {
    if !HAVE_DISPLAY {
        return;
    }
    if COL >= COLS || ROW >= ROWS { return; }
    let pixel_x = COL * GLYPH_W;
    let pixel_y = ROW * GLYPH_H;
    let bytes_per_pixel = BPP / 8;

    // Draw bottom 2 pixel rows as a solid underline
    for gy in (GLYPH_H - 2)..GLYPH_H {
        let y = pixel_y + gy;
        let row_base = FB + y * PITCH + pixel_x * bytes_per_pixel;
        for gx in 0..GLYPH_W {
            let px = row_base + gx * bytes_per_pixel;
            if bytes_per_pixel == 4 {
                (px as *mut u32).write_volatile(color);
            } else if bytes_per_pixel == 3 {
                let ptr = px as *mut u8;
                ptr.write_volatile(color as u8);
                ptr.add(1).write_volatile((color >> 8) as u8);
                ptr.add(2).write_volatile((color >> 16) as u8);
            }
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[console] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
