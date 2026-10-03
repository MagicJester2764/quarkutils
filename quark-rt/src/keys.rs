//! Keys as every keyboard's driver says them: a key's code is Linux's
//! (evdev), which is what a compositor hands its clients, and what it types
//! is ASCII, by the table of a US keyboard, with Shift, Caps Lock and Ctrl.
//!
//! Where a key came from — a scancode at an i8042's port, a usage in a USB
//! keyboard's report — is its driver's business. From the driver on it is the
//! same key, said the same way (`TAG_GET_KEY_NB`: pressed or let go, the
//! character, the code, the modifiers down), so `input` and everything above
//! it neither know nor care which keyboard it was typed on.

/// The modifiers a key event carries.
pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CTRL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;
pub const MOD_CAPSLOCK: u8 = 1 << 3;

// The codes of the keys that are modifiers.
pub const LEFT_CTRL: u8 = 29;
pub const LEFT_SHIFT: u8 = 42;
pub const RIGHT_SHIFT: u8 = 54;
pub const LEFT_ALT: u8 = 56;
pub const CAPS_LOCK: u8 = 58;
pub const RIGHT_CTRL: u8 = 97;
pub const RIGHT_ALT: u8 = 100;

/// What each of the first 128 codes types, unshifted and shifted. Below 0x60
/// a code is the key's scancode in set 1 as well, which is where the table
/// came from.
#[rustfmt::skip]
static UNSHIFTED: [u8; 128] = [
    0,  27, b'1',b'2',b'3',b'4',b'5',b'6',b'7',b'8',b'9',b'0',b'-',b'=', 8,  9,   // 0x00-0x0F
    b'q',b'w',b'e',b'r',b't',b'y',b'u',b'i',b'o',b'p',b'[',b']', 10,  0, b'a',b's', // 0x10-0x1F
    b'd',b'f',b'g',b'h',b'j',b'k',b'l',b';',b'\'',b'`', 0, b'\\',b'z',b'x',b'c',b'v', // 0x20-0x2F
    b'b',b'n',b'm',b',',b'.',b'/', 0, b'*', 0, b' ', 0,  0,  0,  0,  0,  0,   // 0x30-0x3F
    0,   0,  0,  0,  0,  0,  0,  b'7',b'8',b'9',b'-',b'4',b'5',b'6',b'+',b'1', // 0x40-0x4F
    b'2',b'3',b'0',b'.', 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,   // 0x50-0x5F
    0,   0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,       // 0x60-0x6F
    0,   0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,       // 0x70-0x7F
];

#[rustfmt::skip]
static SHIFTED: [u8; 128] = [
    0,  27, b'!',b'@',b'#',b'$',b'%',b'^',b'&',b'*',b'(',b')',b'_',b'+', 8,  9,   // 0x00-0x0F
    b'Q',b'W',b'E',b'R',b'T',b'Y',b'U',b'I',b'O',b'P',b'{',b'}', 10,  0, b'A',b'S', // 0x10-0x1F
    b'D',b'F',b'G',b'H',b'J',b'K',b'L',b':',b'"',b'~', 0, b'|',b'Z',b'X',b'C',b'V', // 0x20-0x2F
    b'B',b'N',b'M',b'<',b'>',b'?', 0, b'*', 0, b' ', 0,  0,  0,  0,  0,  0,   // 0x30-0x3F
    0,   0,  0,  0,  0,  0,  0,  b'7',b'8',b'9',b'-',b'4',b'5',b'6',b'+',b'1', // 0x40-0x4F
    b'2',b'3',b'0',b'.', 0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,   // 0x50-0x5F
    0,   0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,       // 0x60-0x6F
    0,   0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,       // 0x70-0x7F
];

/// The modifiers down, kept as keys go down and come up.
#[derive(Clone, Copy, Default)]
pub struct Modifiers(pub u8);

impl Modifiers {
    /// Key `code` went down (`press`) or came up: the modifiers now. Either
    /// Shift, Ctrl or Alt is the one modifier, and Caps Lock turns over each
    /// time it goes down.
    pub fn key(&mut self, code: u8, press: bool) -> u8 {
        let bit = match code {
            LEFT_SHIFT | RIGHT_SHIFT => MOD_SHIFT,
            LEFT_CTRL | RIGHT_CTRL => MOD_CTRL,
            LEFT_ALT | RIGHT_ALT => MOD_ALT,
            CAPS_LOCK => {
                if press {
                    self.0 ^= MOD_CAPSLOCK;
                }
                return self.0;
            }
            _ => return self.0,
        };
        if press {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
        self.0
    }
}

/// What key `code` types with `modifiers` down: an ASCII byte, or 0 for a key
/// that types nothing. Shift and Caps Lock each turn the table over, every
/// key's and not only a letter's; Ctrl and a letter is that letter's control
/// character.
pub fn ascii(code: u8, modifiers: u8) -> u8 {
    let typed = match code {
        // The keypad's Enter and /, whose codes are past the table's.
        96 => b'\n',
        98 => b'/',
        c if (c as usize) < UNSHIFTED.len() => {
            let shifted = (modifiers & MOD_SHIFT != 0) ^ (modifiers & MOD_CAPSLOCK != 0);
            if shifted { SHIFTED[c as usize] } else { UNSHIFTED[c as usize] }
        }
        _ => 0,
    };
    if modifiers & MOD_CTRL != 0 && typed.is_ascii_lowercase() { typed & 0x1F } else { typed }
}

/// The code of the key a USB keyboard's report names by usage (the HID
/// usage tables' keyboard page), or 0 for one with no code here. Linux's
/// table, as far as a full-size keyboard goes.
#[rustfmt::skip]
static FROM_USAGE: [u8; 0x66] = [
      0,   0,   0,   0,  30,  48,  46,  32,  18,  33,  34,  35,  23,  36,  37,  38, // 0x00
     50,  49,  24,  25,  16,  19,  31,  20,  22,  47,  17,  45,  21,  44,   2,   3, // 0x10
      4,   5,   6,   7,   8,   9,  10,  11,  28,   1,  14,  15,  57,  12,  13,  26, // 0x20
     27,  43,  43,  39,  40,  41,  51,  52,  53,  58,  59,  60,  61,  62,  63,  64, // 0x30
     65,  66,  67,  68,  87,  88,  99,  70, 119, 110, 102, 104, 111, 107, 109, 106, // 0x40
    105, 108, 103,  69,  98,  55,  74,  78,  96,  79,  80,  81,  75,  76,  77,  71, // 0x50
     72,  73,  82,  83,  86, 127,                                                   // 0x60
];

/// The modifier keys, by their bit in a USB keyboard's report: left Ctrl,
/// Shift, Alt and GUI, then the right ones.
pub static FROM_MODIFIER_BIT: [u8; 8] = [LEFT_CTRL, LEFT_SHIFT, LEFT_ALT, 125, RIGHT_CTRL, RIGHT_SHIFT, RIGHT_ALT, 126];

pub fn from_usage(usage: u8) -> u8 {
    match usage {
        0xE0..=0xE7 => FROM_MODIFIER_BIT[(usage - 0xE0) as usize],
        u if (u as usize) < FROM_USAGE.len() => FROM_USAGE[u as usize],
        _ => 0,
    }
}
