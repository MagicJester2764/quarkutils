#![no_std]
#![no_main]

//! The RTL8139: the network card QEMU's machines have unless told otherwise.
//!
//! Started by the device manager for one, holding it: its ports are minted
//! from it and its interrupt line was given with it. What it does is move
//! frames — the stack above it (`net`) claims it, lends it a frame to send,
//! and is told when frames have come and asks for them (`quark_rt::nic`).
//! The card copies them by itself into a ring this program asked the kernel
//! for, below four gigabytes because the card is told where it is in a
//! register thirty-two bits wide; on a machine with an IOMMU that ring and
//! the four pages it sends from are all it reaches.

use quark_rt::manifest::CapReq;
use quark_rt::nic::{self, Card};
use quark_rt::{pci, println, syscall};

// A driver's band; the card; and frames for its ring and its four
// transmit buffers.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x10EC, 0x8139),
    CapReq::phys_alloc(8),
]);

// Registers, from the card's ports.
const REG_IDR: u16 = 0x00; // its address, six bytes
const REG_TSD0: u16 = 0x10; // transmit status, one for each of four buffers
const REG_TSAD0: u16 = 0x20; // transmit buffer, likewise
const REG_RBSTART: u16 = 0x30; // the receive ring
const REG_CR: u16 = 0x37;
const REG_CAPR: u16 = 0x38; // where the driver has read the ring to
const REG_IMR: u16 = 0x3C;
const REG_ISR: u16 = 0x3E;
const REG_TCR: u16 = 0x40;
const REG_RCR: u16 = 0x44;
const REG_CONFIG1: u16 = 0x52;

const CR_RST: u8 = 0x10;
const CR_RE: u8 = 0x08;
const CR_TE: u8 = 0x04;
const CR_BUFE: u8 = 0x01;

const ISR_ROK: u16 = 0x0001;
const ISR_TOK: u16 = 0x0004;

// Accept what is for this card, multicast and broadcast; a frame that runs
// past the ring's end is written on past it rather than round; an 8 KiB
// ring; the largest bursts.
const RCR_VALUE: u32 = 0x0000_E78E;
const TCR_VALUE: u32 = 0x0300_0700;

const RING: usize = 8192;
/// The ring and the room a frame written past its end takes.
const RING_PAGES: usize = 3;
const BUFFERS: usize = 4;
const MAX_FRAME: usize = 1536;

const RING_AT: usize = 0x89_0000_0000;
const BUFFERS_AT: usize = 0x89_0010_0000;
const PORTS_SLOT: usize = 10;

fn inb(port: u16) -> u8 {
    syscall::sys_ioport_read(port) as u8
}
fn inw(port: u16) -> u16 {
    syscall::sys_ioport_read16(port)
}
fn outb(port: u16, val: u8) {
    syscall::sys_ioport_write(port, val)
}
fn outw(port: u16, val: u16) {
    syscall::sys_ioport_write16(port, val)
}
fn outl(port: u16, val: u32) {
    syscall::sys_ioport_write32(port, val)
}

/// `count` pages in a row below four gigabytes, mapped at `at`: where they
/// are.
fn pages(count: usize, at: usize) -> Option<usize> {
    let first = syscall::sys_phys_alloc_low(count).ok()?;
    syscall::sys_map_phys(first, at, count).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, count * 4096) };
    Some(first)
}

struct Rtl {
    io: u16,
    irq: u8,
    mac: [u8; 6],
    /// Where in the ring the next frame is.
    offset: usize,
    /// The transmit buffer to use next, and where each is.
    next: usize,
    buffers: [usize; BUFFERS],
}

impl Card for Rtl {
    fn address(&self) -> [u8; 6] {
        self.mac
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() > MAX_FRAME {
            return false;
        }
        let n = self.next;
        unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), (BUFFERS_AT + n * 4096) as *mut u8, frame.len()) };
        outl(self.io + REG_TSAD0 + n as u16 * 4, self.buffers[n] as u32);
        outl(self.io + REG_TSD0 + n as u16 * 4, frame.len() as u32);
        self.next = (n + 1) % BUFFERS;
        true
    }

    fn receive(&mut self, into: &mut [u8]) -> usize {
        if inb(self.io + REG_CR) & CR_BUFE != 0 {
            return 0;
        }
        let header = unsafe { core::ptr::read_volatile((RING_AT + self.offset) as *const u32) };
        let (status, length) = (header as u16, (header >> 16) as usize);
        if status & 1 == 0 || length < 4 || length > MAX_FRAME + 4 {
            // Not a frame: start the ring again where the card is.
            return 0;
        }
        // Without the checksum at its end.
        let len = (length - 4).min(into.len());
        unsafe { core::ptr::copy_nonoverlapping((RING_AT + self.offset + 4) as *const u8, into.as_mut_ptr(), len) };
        self.offset = (self.offset + ((4 + length + 3) & !3)) % RING;
        outw(self.io + REG_CAPR, self.offset.wrapping_sub(16) as u16);
        len
    }

    fn interrupt(&mut self) -> bool {
        let isr = inw(self.io + REG_ISR);
        if isr != 0 {
            outw(self.io + REG_ISR, isr);
        }
        syscall::sys_irq_ack(self.irq);
        isr & ISR_ROK != 0
    }

    fn irq(&self) -> u8 {
        self.irq
    }
}

fn stop(why: &str) -> ! {
    println!("[rtl8139] {}", why);
    syscall::sys_exit_code(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some((device, info)) = pci::this_device().and_then(|d| Some((d, pci::info(d)?))) else {
        stop("started without a card: the device manager starts this, for an RTL8139.");
    };
    let Some((io, _)) = pci::ports(device, 0, PORTS_SLOT) else {
        stop("the card's ports were not given");
    };
    // The card is this program's, and copies memory itself only once it is.
    if pci::claim(device).is_err() {
        stop("the card is another program's");
    }
    if pci::enable(device, pci::COMMAND_PORTS | pci::COMMAND_MASTER).is_err() {
        stop("the card may not be turned on");
    }
    let irq = info.header.line;
    if irq == 0 || irq >= 16 || syscall::sys_irq_register(irq).is_err() {
        stop("the card's interrupt was not given");
    }

    // On, and reset.
    outb(io + REG_CONFIG1, 0);
    outb(io + REG_CR, CR_RST);
    for _ in 0..1000 {
        if inb(io + REG_CR) & CR_RST == 0 {
            break;
        }
        syscall::sleep_ns(10_000);
    }
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = inb(io + REG_IDR + i as u16);
    }
    let Some(ring) = pages(RING_PAGES, RING_AT) else {
        stop("no memory for the card's ring");
    };
    let mut buffers = [0usize; BUFFERS];
    for (n, b) in buffers.iter_mut().enumerate() {
        match pages(1, BUFFERS_AT + n * 4096) {
            Some(at) => *b = at,
            None => stop("no memory for the card's buffers"),
        }
    }
    outl(io + REG_RBSTART, ring as u32);
    outw(io + REG_IMR, ISR_ROK | ISR_TOK);
    outl(io + REG_RCR, RCR_VALUE);
    outl(io + REG_TCR, TCR_VALUE);
    outb(io + REG_CR, CR_RE | CR_TE);
    outw(io + REG_CAPR, 0xFFF0);

    let Some(name) = nic::register() else {
        stop("eth0 to eth7 are all taken");
    };
    println!(
        "[rtl8139] {} at ports {:#x}, interrupt {}, address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        core::str::from_utf8(&name).unwrap_or("a card"),
        io,
        irq,
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    let mut card = Rtl { io, irq, mac, offset: 0, next: 0, buffers };
    nic::serve(&mut card)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[rtl8139] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
