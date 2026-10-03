//! Transfer request blocks, and the rings they go round (xHCI 4.9, 6.4).
//!
//! A ring is a page of sixteen-byte TRBs. The program writes at its end and
//! the controller reads, each telling a TRB it has not seen from one it has
//! by a cycle bit that turns over each time round; the last TRB of a page is
//! a link back to the first, which turns the controller's bit over too. The
//! event ring runs the other way: the controller writes, this reads, and
//! tells the controller how far it has got.

use crate::mem::Page;

/// TRBs in a page.
pub const TRBS: usize = 256;

// What a TRB is.
pub const NORMAL: u32 = 1;
pub const SETUP: u32 = 2;
pub const DATA: u32 = 3;
pub const STATUS: u32 = 4;
pub const LINK: u32 = 6;
pub const ENABLE_SLOT: u32 = 9;
pub const DISABLE_SLOT: u32 = 10;
pub const ADDRESS_DEVICE: u32 = 11;
pub const CONFIGURE_ENDPOINT: u32 = 12;
pub const EVALUATE_CONTEXT: u32 = 13;
pub const RESET_ENDPOINT: u32 = 14;
pub const STOP_ENDPOINT: u32 = 15;
pub const SET_DEQUEUE: u32 = 16;
pub const TRANSFER_EVENT: u32 = 32;
pub const COMMAND_COMPLETION: u32 = 33;
pub const PORT_STATUS_CHANGE: u32 = 34;

// Bits of a TRB's last word.
pub const CYCLE: u32 = 1 << 0;
pub const TOGGLE_CYCLE: u32 = 1 << 1;
pub const SHORT_IS_FINE: u32 = 1 << 2;
pub const ON_COMPLETION: u32 = 1 << 5;
pub const IMMEDIATE: u32 = 1 << 6;
/// A data or status stage's direction: towards this program.
pub const DIRECTION_IN: u32 = 1 << 16;

// How a TRB went (6.4.5).
pub const SUCCESS: u8 = 1;
pub const STALL: u8 = 6;
pub const SHORT_PACKET: u8 = 13;

#[derive(Clone, Copy, Default, Debug)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub fn new(kind: u32, param: u64, status: u32, flags: u32) -> Trb {
        Trb { param, status, control: kind << 10 | flags }
    }

    pub fn kind(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }

    /// How an event says its TRB went.
    pub fn code(&self) -> u8 {
        (self.status >> 24) as u8
    }

    /// What of a transfer was not done: bytes.
    pub fn residual(&self) -> u32 {
        self.status & 0xFF_FFFF
    }

    pub fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }

    /// Which endpoint an event is for (its context index).
    pub fn endpoint(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }

    pub fn went_well(&self) -> bool {
        self.code() == SUCCESS || self.code() == SHORT_PACKET
    }
}

/// A ring this program writes to: a command ring, or an endpoint's.
pub struct Ring {
    pub page: Page,
    index: usize,
    cycle: u32,
}

impl Ring {
    pub fn new(page: Page) -> Ring {
        page.zero();
        let link = (TRBS - 1) * 16;
        page.write64(link, page.phys);
        page.write32(link + 12, LINK << 10 | TOGGLE_CYCLE);
        Ring { page, index: 0, cycle: 1 }
    }

    /// Where the next TRB goes, with the cycle it will carry: what the
    /// controller is told to go on from after an endpoint stopped.
    pub fn dequeue(&self) -> u64 {
        self.page.phys + (self.index * 16) as u64 | self.cycle as u64
    }

    /// Put `trb` on the ring: where it is. The cycle bit is written last,
    /// which is what gives it to the controller.
    pub fn push(&mut self, trb: Trb) -> u64 {
        let at = self.index * 16;
        self.page.write64(at, trb.param);
        self.page.write32(at + 8, trb.status);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.page.write32(at + 12, (trb.control & !CYCLE) | self.cycle);
        let phys = self.page.phys + at as u64;
        self.index += 1;
        if self.index == TRBS - 1 {
            // Round the link, giving it this cycle, and turn over.
            let link = (TRBS - 1) * 16;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            let control = self.page.read32(link + 12);
            self.page.write32(link + 12, (control & !CYCLE) | self.cycle);
            self.cycle ^= 1;
            self.index = 0;
        }
        phys
    }
}

/// The ring the controller writes events to.
pub struct Events {
    pub page: Page,
    index: usize,
    cycle: u32,
}

impl Events {
    pub fn new(page: Page) -> Events {
        page.zero();
        Events { page, index: 0, cycle: 1 }
    }

    /// The next event, if the controller has written one.
    pub fn next(&mut self) -> Option<Trb> {
        let at = self.index * 16;
        let control = self.page.read32(at + 12);
        if control & CYCLE != self.cycle {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let trb = Trb {
            param: self.page.read32(at) as u64 | (self.page.read32(at + 4) as u64) << 32,
            status: self.page.read32(at + 8),
            control,
        };
        self.index += 1;
        if self.index == TRBS {
            self.index = 0;
            self.cycle ^= 1;
        }
        Some(trb)
    }

    /// How far this has read: what the controller is told.
    pub fn dequeue(&self) -> u64 {
        self.page.phys + (self.index * 16) as u64
    }
}
