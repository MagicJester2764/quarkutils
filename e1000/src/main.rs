#![no_std]
#![no_main]

//! Intel's gigabit cards of the e1000 family: the 82540EM, which is QEMU's
//! `e1000` and the card a PC machine there is given, the 82545EM, and the
//! 82574L, which is `e1000e` and the q35 machine's own.
//!
//! Started by the device manager for one, holding it. Its registers are
//! memory, in its first BAR. Frames go each way through a ring of
//! descriptors the card reads and writes itself: thirty-two buffers wait for
//! what comes, eight carry what goes, and each descriptor names its buffer
//! by a whole sixty-four-bit address, so the memory may be anywhere — and on
//! a machine with an IOMMU it is all the card reaches. Only the oldest form
//! of descriptor is used, which every card of the family has, and nothing
//! is offloaded. Its interrupt is a message where it has MSI (the 82574L)
//! and its line where it has not. What it serves is `quark_rt::nic`, as
//! every card's driver does.

use core::ptr::{read_volatile, write_volatile};
use quark_rt::manifest::CapReq;
use quark_rt::nic::{self, Card};
use quark_rt::{pci, println, syscall};

// A driver's band; the cards; and frames for two rings and their buffers.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x8086, 0x100E),
    CapReq::drives(0x8086, 0x100F),
    CapReq::drives(0x8086, 0x10D3),
    CapReq::phys_alloc(24),
]);

/// Where the registers are mapped and the slot their range is minted in;
/// where an MSI-X table would be, for a card with that and not MSI.
const REGS_AT: usize = 0xA0_0000_0000;
const REGS_SLOT: usize = 10;
const TABLE_AT: usize = 0xA1_0000_0000;
const TABLE_SLOT: usize = 11;
/// The two rings, a page each; what goes, a page for two buffers; what
/// comes, likewise.
const RX_RING_AT: usize = 0x86_0000_0000;
const TX_RING_AT: usize = 0x86_0000_1000;
const TX_AT: usize = 0x86_0000_2000;
const RX_AT: usize = 0x86_0001_0000;

const RX_COUNT: usize = 32;
const TX_COUNT: usize = 8;
const BUFFER: usize = 2048;

// Registers (8254x and 82574 datasheets, §13 and §10).
const CTRL: usize = 0x0000;
const EERD: usize = 0x0014;
const ICR: usize = 0x00C0;
const IMS: usize = 0x00D0;
const IMC: usize = 0x00D8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;

const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const RAH_AV: u32 = 1 << 31;

/// Receive: on; broadcasts too; the checksum taken off. Buffers of 2048
/// bytes, which is what nought in the size field means.
const RCTL_VALUE: u32 = 1 << 1 | 1 << 15 | 1 << 26;
/// Transmit: on; short frames padded; the collision threshold and distance
/// the datasheets give for full duplex.
const TCTL_VALUE: u32 = 1 << 1 | 1 << 3 | 0x0F << 4 | 0x40 << 12;
/// The gaps between frames, as the datasheets give them for copper.
const TIPG_VALUE: u32 = 10 | 8 << 10 | 6 << 20;

// What an interrupt was for: frames came, came past what was free, or
// left too few free; the link changed.
const ICR_LSC: u32 = 1 << 2;
const ICR_RXDMT0: u32 = 1 << 4;
const ICR_RXO: u32 = 1 << 6;
const ICR_RXT0: u32 = 1 << 7;
const ICR_RECEIVED: u32 = ICR_RXDMT0 | ICR_RXO | ICR_RXT0;

// A descriptor's command and status.
const CMD_EOP: u8 = 1 << 0;
const CMD_IFCS: u8 = 1 << 1;
const CMD_RS: u8 = 1 << 3;
const STATUS_DD: u8 = 1 << 0;
const STATUS_EOP: u8 = 1 << 1;

/// A descriptor of either ring: where its buffer is, how long, and what
/// became of it. Received: `length` and `status`, with `errors` in the byte
/// after. Sent: `length`, the command in byte 11, the status in byte 12.
#[repr(C)]
struct Desc {
    addr: u64,
    length: u16,
    bytes: [u8; 6],
}

fn read(regs: usize, at: usize) -> u32 {
    unsafe { read_volatile((regs + at) as *const u32) }
}
fn write(regs: usize, at: usize, value: u32) {
    unsafe { write_volatile((regs + at) as *mut u32, value) }
}

/// `count` pages in a row of this program's own memory at `at`, cleared:
/// where they are.
fn pages(count: usize, at: usize) -> Option<u64> {
    let first = syscall::sys_phys_alloc(count).ok()?;
    syscall::sys_map_phys(first, at, count).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, count * 4096) };
    Some(first as u64)
}

fn rx(i: usize) -> *mut Desc {
    (RX_RING_AT + i * 16) as *mut Desc
}
fn tx(i: usize) -> *mut Desc {
    (TX_RING_AT + i * 16) as *mut Desc
}

struct E1000 {
    regs: usize,
    irq: u8,
    line: bool,
    mac: [u8; 6],
    /// The next descriptor of each ring to look at.
    rx_next: usize,
    tx_next: usize,
}

impl Card for E1000 {
    fn address(&self) -> [u8; 6] {
        self.mac
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.is_empty() || frame.len() > nic::FRAME {
            return false;
        }
        let i = self.tx_next;
        let d = tx(i);
        // The card says it has finished with a descriptor once it has sent
        // what it named (`RS`): a moment, if it has not yet.
        let mut tries = 0;
        while unsafe { read_volatile(&(*d).bytes[2]) } & STATUS_DD == 0 {
            tries += 1;
            if tries > 100 {
                return false;
            }
            syscall::sleep_ns(10_000);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), (TX_AT + i * BUFFER) as *mut u8, frame.len());
            write_volatile(&mut (*d).length, frame.len() as u16);
            write_volatile(&mut (*d).bytes[1], CMD_EOP | CMD_IFCS | CMD_RS);
            write_volatile(&mut (*d).bytes[2], 0);
        }
        self.tx_next = (i + 1) % TX_COUNT;
        write(self.regs, TDT, self.tx_next as u32);
        true
    }

    fn receive(&mut self, into: &mut [u8]) -> usize {
        loop {
            let i = self.rx_next;
            let d = rx(i);
            let (status, errors) = unsafe { (read_volatile(&(*d).bytes[2]), read_volatile(&(*d).bytes[3])) };
            if status & STATUS_DD == 0 {
                return 0;
            }
            let length = unsafe { read_volatile(&(*d).length) } as usize;
            // A frame in one buffer, whole and without errors, or nothing:
            // none longer than a buffer is let in.
            let good = status & STATUS_EOP != 0 && errors == 0 && length != 0 && length <= BUFFER;
            let len = length.min(into.len());
            if good {
                unsafe { core::ptr::copy_nonoverlapping((RX_AT + i * BUFFER) as *const u8, into.as_mut_ptr(), len) };
            }
            // Back to the card: the last descriptor it may fill is this one.
            unsafe { write_volatile(&mut (*d).bytes[2], 0) };
            self.rx_next = (i + 1) % RX_COUNT;
            write(self.regs, RDT, i as u32);
            if good {
                return len;
            }
        }
    }

    fn interrupt(&mut self) -> bool {
        // Read, it is cleared.
        let cause = read(self.regs, ICR);
        if self.line {
            syscall::sys_irq_ack(self.irq);
        }
        cause & ICR_RECEIVED != 0
    }

    fn irq(&self) -> u8 {
        self.irq
    }
}

fn stop(why: &str) -> ! {
    println!("[e1000] {}", why);
    syscall::sys_exit_code(1);
}

/// A word of the card's EEPROM, which is where its address is when the
/// card has not put it in its first receive address. The 82574L has the
/// register laid out another way.
fn eeprom(regs: usize, word: u32, newer: bool) -> Option<u16> {
    let (shift, done) = if newer { (2, 1 << 1) } else { (8, 1 << 4) };
    write(regs, EERD, word << shift | 1);
    for _ in 0..1000 {
        let value = read(regs, EERD);
        if value & done != 0 {
            return Some((value >> 16) as u16);
        }
        syscall::sleep_ns(10_000);
    }
    None
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some((device, info)) = pci::this_device().and_then(|d| Some((d, pci::info(d)?))) else {
        stop("started without a card: the device manager starts this, for an Intel gigabit card.");
    };
    let model = match info.header.device {
        0x100E => "82540EM",
        0x100F => "82545EM",
        0x10D3 => "82574L",
        _ => "e1000",
    };
    let newer = info.header.device == 0x10D3;
    let Some(regs) = pci::map_bar(device, 0, REGS_AT, REGS_SLOT) else {
        stop("the card's registers were not given");
    };
    // The card is this program's, and copies memory itself only once it is.
    if pci::claim(device).is_err() {
        stop("the card is another program's");
    }
    if pci::enable(device, pci::COMMAND_MEMORY | pci::COMMAND_MASTER).is_err() {
        stop("the card may not be turned on");
    }

    // Quiet, and reset: the reset takes a microsecond or so, and the
    // interrupts are masked again after it, which it may have undone.
    write(regs, IMC, u32::MAX);
    write(regs, CTRL, read(regs, CTRL) | CTRL_RST);
    syscall::sleep_ns(10_000);
    for _ in 0..1000 {
        if read(regs, CTRL) & CTRL_RST == 0 {
            break;
        }
        syscall::sleep_ns(10_000);
    }
    write(regs, IMC, u32::MAX);
    let _ = read(regs, ICR);
    write(regs, CTRL, read(regs, CTRL) | CTRL_SLU | CTRL_ASDE);

    let Some(interrupt) = pci::interrupt(device, |bar| pci::map_bar(device, bar, TABLE_AT, TABLE_SLOT)) else {
        stop("no interrupt to be had for the card");
    };

    // Its address: in the first receive address where the card put it,
    // else from its EEPROM, and then put there — that register is what
    // decides which frames are this card's.
    let (low, high) = (read(regs, RAL), read(regs, RAH));
    let mut mac = [0u8; 6];
    if high & RAH_AV != 0 {
        mac[..4].copy_from_slice(&low.to_le_bytes());
        mac[4..].copy_from_slice(&high.to_le_bytes()[..2]);
    } else {
        for word in 0..3 {
            let Some(value) = eeprom(regs, word, newer) else {
                stop("the card's address could not be read");
            };
            mac[word as usize * 2..word as usize * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        write(regs, RAL, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
        write(regs, RAH, u16::from_le_bytes([mac[4], mac[5]]) as u32 | RAH_AV);
    }
    for i in 0..128 {
        write(regs, MTA + i * 4, 0);
    }

    // What comes: every descriptor named its buffer, all of them but one
    // the card's to fill.
    let (Some(rx_ring), Some(rx_buffers)) = (pages(1, RX_RING_AT), pages(RX_COUNT * BUFFER / 4096, RX_AT)) else {
        stop("no memory for what the card receives");
    };
    for i in 0..RX_COUNT {
        unsafe { write_volatile(&mut (*rx(i)).addr, rx_buffers + (i * BUFFER) as u64) };
    }
    write(regs, RDBAL, rx_ring as u32);
    write(regs, RDBAH, (rx_ring >> 32) as u32);
    write(regs, RDLEN, (RX_COUNT * 16) as u32);
    write(regs, RDH, 0);
    write(regs, RDT, (RX_COUNT - 1) as u32);
    write(regs, RCTL, RCTL_VALUE);

    // What goes: every descriptor done with, to begin.
    let (Some(tx_ring), Some(tx_buffers)) = (pages(1, TX_RING_AT), pages(TX_COUNT * BUFFER / 4096, TX_AT)) else {
        stop("no memory for what the card sends");
    };
    for i in 0..TX_COUNT {
        unsafe {
            write_volatile(&mut (*tx(i)).addr, tx_buffers + (i * BUFFER) as u64);
            write_volatile(&mut (*tx(i)).bytes[2], STATUS_DD);
        }
    }
    write(regs, TDBAL, tx_ring as u32);
    write(regs, TDBAH, (tx_ring >> 32) as u32);
    write(regs, TDLEN, (TX_COUNT * 16) as u32);
    write(regs, TDH, 0);
    write(regs, TDT, 0);
    write(regs, TIPG, TIPG_VALUE);
    write(regs, TCTL, TCTL_VALUE);

    write(regs, IMS, ICR_RECEIVED | ICR_LSC);

    let Some(name) = nic::register() else {
        stop("eth0 to eth7 are all taken");
    };
    println!(
        "[e1000] {}: {}, interrupt {} ({}), address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        core::str::from_utf8(&name).unwrap_or("a card"),
        model,
        interrupt.number(),
        if interrupt.is_line() { "its line" } else { "a message of its own" },
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    let mut card = E1000 { regs, irq: interrupt.number(), line: interrupt.is_line(), mac, rx_next: 0, tx_next: 0 };
    nic::serve(&mut card)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[e1000] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
