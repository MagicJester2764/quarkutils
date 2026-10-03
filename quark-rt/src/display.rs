//! What a program that draws on the screen says about what it drew.
//!
//! A display that draws from memory — a virtio GPU — shows what its driver
//! copies to it, and nothing else: the program that has the display (`fb`
//! lends it) says which parts it drew on, and the driver copies those. It is
//! said as *tiles*: the screen cut into eight across and seven down, a bit
//! each of one word, which is a notification — sent to `fb` and passed on,
//! and never waited on, so drawing never waits for the screen. With the
//! bootloader's framebuffer it is said to nobody who needs it, and costs a
//! notification.
//!
//! Fifty-six tiles and not sixty-four, because bits 16 to 18 of a
//! notification word are the kernel's task signals, and `SYS_NOTIFY`
//! refuses a word with any of them in it, whole: the tiles skip them. With
//! eight rows, every repaint of the whole screen was refused, and every
//! change to the third row's left side, and what the screen showed was
//! whatever had been copied before.

use crate::syscall;

/// Tiles across and down.
pub const ACROSS: u64 = 8;
pub const DOWN: u64 = 7;

/// The bit of tile `t`, counted across and then down.
fn bit(t: u64) -> u64 {
    1 << if t < 16 { t } else { t + 3 }
}

/// The tile of bit `b`, if it is one.
fn tile(b: u64) -> Option<u64> {
    match b {
        0..=15 => Some(b),
        16..=18 => None,
        _ => Some(b - 3),
    }
}

/// The tiles a rectangle `[x0, x1) × [y0, y1)` touches, on a screen
/// `width` by `height`.
pub fn tiles(x0: u64, y0: u64, x1: u64, y1: u64, width: u64, height: u64) -> u64 {
    if width == 0 || height == 0 || x1 <= x0 || y1 <= y0 {
        return 0;
    }
    let col = |x: u64| (x.min(width - 1) * ACROSS / width).min(ACROSS - 1);
    let row = |y: u64| (y.min(height - 1) * DOWN / height).min(DOWN - 1);
    let (c0, c1, r0, r1) = (col(x0), col(x1 - 1), row(y0), row(y1 - 1));
    let mut bits = 0u64;
    for r in r0..=r1 {
        for c in c0..=c1 {
            bits |= bit(r * ACROSS + c);
        }
    }
    bits
}

/// The smallest rectangle holding every tile in `bits`, on a screen `width`
/// by `height`: `(x0, y0, x1, y1)`, or `None` for no tiles.
pub fn bounds(bits: u64, width: u64, height: u64) -> Option<(u64, u64, u64, u64)> {
    let (mut c0, mut c1, mut r0, mut r1) = (ACROSS, 0, DOWN, 0);
    for t in (0..64).filter(|&b| bits & (1 << b) != 0).filter_map(tile) {
        let (r, c) = (t / ACROSS, t % ACROSS);
        if r >= DOWN {
            continue;
        }
        c0 = c0.min(c);
        c1 = c1.max(c);
        r0 = r0.min(r);
        r1 = r1.max(r);
    }
    if c0 > c1 {
        return None;
    }
    let x = |c: u64| c * width / ACROSS;
    let y = |r: u64| r * height / DOWN;
    Some((x(c0), y(r0), x(c1 + 1).min(width), y(r1 + 1).min(height)))
}

/// Say that `[x0, x1) × [y0, y1)` was drawn on, to the framebuffer device
/// `fb`, on a screen `width` by `height`: whether it was said.
pub fn drew(fb: usize, x0: u64, y0: u64, x1: u64, y1: u64, width: u64, height: u64) -> bool {
    let bits = tiles(x0, y0, x1, y1, width, height);
    bits == 0 || syscall::sys_notify(fb, bits).is_ok()
}

// The framebuffer device's protocol, as far as a program that changes the
// display's mode needs it (`fb`'s own file says the rest).
const TAG_FB_INFO: u64 = 1;
const TAG_FB_CLAIM: u64 = 2;
const TAG_FB_RELEASE: u64 = 3;
const TAG_FB_SET_MODE: u64 = 7;
const TAG_FB_LOST: u64 = 0x100;
const TAG_FB_GAINED: u64 = 0x101;
/// Where a claimant is lent the display.
const LEASE_SLOT: usize = 2;

/// A mode, as `fb` says one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u64,
    /// Where the screen is; 0 for no display.
    pub phys: u64,
    /// A driver gives it, and it may be had in another size.
    pub driven: bool,
}

impl Mode {
    pub fn from_words(w: &[u64; 6]) -> Mode {
        Mode {
            width: w[0] >> 32,
            height: w[0] & 0xFFFF_FFFF,
            pitch: w[1] >> 32,
            bpp: w[1] & 0xFF,
            phys: w[3],
            driven: w[4] == 1,
        }
    }
}

/// The display's mode, as `fb` says it to anybody.
pub fn mode() -> Option<Mode> {
    let fb = crate::nameserver::lookup(b"fb")?;
    let msg = crate::ipc::Message { sender: 0, tag: TAG_FB_INFO, data: [0; 6] };
    let mut reply = crate::ipc::Message::empty();
    syscall::sys_call(fb, &msg, &mut reply).ok()?;
    (reply.tag == 0).then(|| Mode::from_words(&reply.data))
}

/// Have the display `width` by `height`: claimed, asked for, and let go
/// once `fb` has handed it back in that mode — the mode it was handed back
/// in. Only a display a driver gives has other modes. Answering `fb`'s two
/// calls before letting go is the point: a program in a call to `fb` when
/// `fb` calls it can answer nothing, and the two wait out each other's
/// patience.
pub fn set_mode(width: u64, height: u64) -> Result<Mode, &'static str> {
    use crate::ipc::{Message, TID_ANY};
    let fb = crate::nameserver::lookup(b"fb").ok_or("no framebuffer device")?;
    let me = syscall::sys_getpid() as usize;
    if syscall::sys_cap_read(me, LEASE_SLOT).is_ok_and(|c| c.cap_type != 0) {
        return Err("the slot the display is lent in is not empty");
    }
    let mut reply = Message::empty();
    let claim = Message { sender: 0, tag: TAG_FB_CLAIM, data: [0; 6] };
    if syscall::sys_call_offer_self(fb, &claim, &mut reply).is_err() || reply.tag != 0 {
        return Err("the display could not be had");
    }
    let ask = Message { sender: 0, tag: TAG_FB_SET_MODE, data: [width << 32 | height, 0, 0, 0, 0, 0] };
    let asked = syscall::sys_call(fb, &ask, &mut reply).is_ok() && reply.tag == 0;
    let mut mode = Err("the display has no such mode");
    if asked {
        // The display goes and comes back with the mode: five seconds.
        mode = Err("the display did not come back");
        let started = syscall::sys_clock();
        while syscall::sys_clock().wrapping_sub(started) < 5_000_000_000 {
            let mut msg = Message::empty();
            if syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(100_000_000)).is_err() || msg.sender != fb {
                continue;
            }
            let _ = syscall::sys_reply(fb, &Message::empty());
            if msg.tag == TAG_FB_LOST {
                let _ = syscall::sys_cap_delete(LEASE_SLOT);
            } else if msg.tag == TAG_FB_GAINED {
                mode = Ok(Mode::from_words(&msg.data));
                break;
            }
        }
    }
    let release = Message { sender: 0, tag: TAG_FB_RELEASE, data: [0; 6] };
    let _ = syscall::sys_call(fb, &release, &mut reply);
    let _ = syscall::sys_cap_delete(LEASE_SLOT);
    mode
}
