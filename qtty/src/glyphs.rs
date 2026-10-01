//! What a character looks like.
//!
//! Three places to look, in this order.
//!
//! A font somebody loaded (`setfont`, at boot): as many characters as the
//! file had, eight pixels wide or sixteen. This is where anything past ASCII
//! comes from, and with one loaded the whole screen is drawn in it — a line
//! of text in two typefaces is worse than either.
//!
//! The font built into the runtime: ASCII, and what the console has drawn in
//! since before there were files to load a font from.
//!
//! And for a character neither has, something that is not nothing: the
//! nearest thing in ASCII where there is one — a straight quote for a curly
//! one, a letter without its accent — and a box where there is not. A box
//! says a character was there. A blank says the program printed a space.
//!
//! The file format is GNU Unifont's `.hex`, one glyph a line:
//!
//! ```text
//! 0041:0000000018242442427E424242420000
//! 4E2D:0000010001003FF8210821083FF8210821083FF82108010001000100010000000000
//! ```
//!
//! a code point, a colon, and sixteen rows of one byte each or of two. It is
//! the format the font is published in, so a distribution installs the file
//! as it came.

use quark_rt::font::FONT;
use quark_rt::syscall;

/// Where the loaded font lives: an index of what is there, and the rows. Both
/// are reserved whole and given memory as they fill, so a small font costs a
/// small font.
const INDEX_AT: usize = 0x82_0000_0000;
const DATA_AT: usize = 0x82_4000_0000;
const MAX_GLYPHS: usize = 1 << 18;
const MAX_DATA: usize = 16 << 20;
const PAGE: usize = 4096;

/// In an entry's offset: the glyph is sixteen pixels wide.
const WIDE: u32 = 1 << 31;

#[derive(Clone, Copy)]
#[repr(C)]
struct Entry {
    cp: u32,
    at: u32,
}

static mut MAPPED: bool = false;
static mut COUNT: usize = 0;
static mut USED: usize = 0;

/// The rows of a glyph: sixteen bytes, or sixteen pairs.
pub enum Glyph {
    Narrow(&'static [u8]),
    Wide(&'static [u8]),
}

/// A character there is no picture of.
static BOX: [u8; 16] = [
    0x00, 0x00, 0x00, 0x7E, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x7E, 0x00, 0x00, 0x00,
];
static BLANK: [u8; 16] = [0; 16];

fn index() -> &'static mut [Entry] {
    unsafe { core::slice::from_raw_parts_mut(INDEX_AT as *mut Entry, COUNT) }
}

/// Forget the loaded font.
pub fn clear() {
    unsafe {
        COUNT = 0;
        USED = 0;
    }
}

/// How many characters the loaded font has.
pub fn count() -> usize {
    unsafe { COUNT }
}

fn hex(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'A'..=b'F' => Some((b - b'A' + 10) as u32),
        b'a'..=b'f' => Some((b - b'a' + 10) as u32),
        _ => None,
    }
}

/// Add the glyphs in `text`, which is whole lines of a `.hex` file. Returns
/// how many it added.
///
/// The file is in order of code point, and so the index is: a line that goes
/// back is refused with everything after it, since finding a glyph is a
/// search that depends on the order. A line that is not a glyph — blank, a
/// comment, a size this does not draw — is passed over.
pub fn load_hex(text: &[u8]) -> Result<usize, ()> {
    unsafe {
        if !MAPPED {
            let index_pages = MAX_GLYPHS * core::mem::size_of::<Entry>() / PAGE;
            syscall::sys_map_anon(INDEX_AT, index_pages, false)?;
            syscall::sys_map_anon(DATA_AT, MAX_DATA / PAGE, false)?;
            MAPPED = true;
        }
    }
    let mut added = 0;
    for line in text.split(|&b| b == b'\n') {
        let line = match line.last() {
            Some(b'\r') => &line[..line.len() - 1],
            _ => line,
        };
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let (name, rows) = (&line[..colon], &line[colon + 1..]);
        let bytes = match rows.len() {
            32 => 16,
            64 => 32,
            _ => continue,
        };
        if name.is_empty() || name.len() > 6 {
            continue;
        }
        let mut cp = 0u32;
        for &b in name {
            cp = cp << 4 | hex(b).ok_or(())?;
        }
        unsafe {
            if COUNT > 0 && index()[COUNT - 1].cp >= cp {
                return Err(());
            }
            if COUNT >= MAX_GLYPHS || USED + bytes > MAX_DATA {
                return Err(());
            }
            let out = core::slice::from_raw_parts_mut((DATA_AT + USED) as *mut u8, bytes);
            for (i, byte) in out.iter_mut().enumerate() {
                let hi = hex(rows[2 * i]).ok_or(())?;
                let lo = hex(rows[2 * i + 1]).ok_or(())?;
                *byte = (hi << 4 | lo) as u8;
            }
            let at = USED as u32 | if bytes == 32 { WIDE } else { 0 };
            COUNT += 1;
            let last = COUNT - 1;
            index()[last] = Entry { cp, at };
            USED += bytes;
        }
        added += 1;
    }
    Ok(added)
}

fn loaded(cp: u32) -> Option<Glyph> {
    let entries = index();
    let i = entries.binary_search_by(|e| e.cp.cmp(&cp)).ok()?;
    let at = entries[i].at;
    let start = DATA_AT + (at & !WIDE) as usize;
    unsafe {
        Some(if at & WIDE != 0 {
            Glyph::Wide(core::slice::from_raw_parts(start as *const u8, 32))
        } else {
            Glyph::Narrow(core::slice::from_raw_parts(start as *const u8, 16))
        })
    }
}

/// What to draw for a character nothing has a picture of, if ASCII has
/// something near enough.
fn nearest(cp: u32) -> Option<u8> {
    Some(match cp {
        0x00A0 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 => b' ',
        0x2018..=0x201B | 0x2032 | 0x00B4 => b'\'',
        0x201C..=0x201F | 0x2033 => b'"',
        0x2010..=0x2015 | 0x2212 | 0x2500 | 0x2501 | 0x2550 => b'-',
        0x2502 | 0x2503 | 0x2551 | 0x00A6 => b'|',
        0x250C..=0x254B | 0x2552..=0x256C => b'+',
        0x2022 | 0x2219 | 0x25CF | 0x00D7 => b'*',
        0x00B7 | 0x2026 => b'.',
        0x00AB | 0x2039 | 0x2190 => b'<',
        0x00BB | 0x203A | 0x2192 => b'>',
        0x2191 => b'^',
        0x2193 => b'v',
        0x00F7 | 0x2044 => b'/',
        0x00A9 => b'c',
        0x00AE => b'r',
        0x00B0 => b'o',
        0x00C0..=0x00C6 => b'A',
        0x00C7 => b'C',
        0x00C8..=0x00CB => b'E',
        0x00CC..=0x00CF => b'I',
        0x00D1 => b'N',
        0x00D2..=0x00D6 | 0x00D8 => b'O',
        0x00D9..=0x00DC => b'U',
        0x00DD => b'Y',
        0x00DF => b's',
        0x00E0..=0x00E6 => b'a',
        0x00E7 => b'c',
        0x00E8..=0x00EB => b'e',
        0x00EC..=0x00EF => b'i',
        0x00F1 => b'n',
        0x00F2..=0x00F6 | 0x00F8 => b'o',
        0x00F9..=0x00FC => b'u',
        0x00FD | 0x00FF => b'y',
        _ => return None,
    })
}

/// The glyph to draw for `cp`. Always something.
pub fn of(cp: u32) -> Glyph {
    if cp == 0 {
        return Glyph::Narrow(&BLANK);
    }
    if unsafe { COUNT } > 0 {
        if let Some(glyph) = loaded(cp) {
            return glyph;
        }
    }
    if cp < 0x80 {
        return Glyph::Narrow(&FONT[cp as usize]);
    }
    match nearest(cp) {
        Some(ascii) => of(ascii as u32),
        None => Glyph::Narrow(&BOX),
    }
}
