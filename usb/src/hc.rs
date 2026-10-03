//! The host controller: its registers, its command ring and its event ring,
//! and waiting for what it says (xHCI 4.2, 5).
//!
//! Everything here is the controller's thread's. A command or a transfer
//! made from it waits for its own event and keeps the rest — reports from
//! interrupt endpoints, ports that changed, notifications from this
//! program's other threads — for the loop to see to after.

use crate::mem::{self, Page};
use crate::trb::{self, Events, Ring, Trb};
use quark_rt::ipc::{Message, TAG_NOTIFICATION, TAG_PING, TID_ANY};
use quark_rt::{pci, syscall};

/// The most devices driven at once, and so the slots asked for.
pub const MAX_SLOTS: usize = 32;

// Capability registers.
const CAPLENGTH: usize = 0x00;
const HCSPARAMS1: usize = 0x04;
const HCSPARAMS2: usize = 0x08;
const HCCPARAMS1: usize = 0x10;
const DBOFF: usize = 0x14;
const RTSOFF: usize = 0x18;
// Operational registers.
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const PAGESIZE: usize = 0x08;
const CRCR: usize = 0x18;
const DCBAAP: usize = 0x30;
const CONFIG: usize = 0x38;
const PORTSC: usize = 0x400;
const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_INTERRUPTS: u32 = 1 << 2;
const STS_HALTED: u32 = 1 << 0;
const STS_EVENT: u32 = 1 << 3;
const STS_NOT_READY: u32 = 1 << 11;
// Interrupter 0, in the runtime registers.
const IMAN: usize = 0x20;
const ERSTSZ: usize = 0x28;
const ERSTBA: usize = 0x30;
const ERDP: usize = 0x38;
const IMAN_PENDING: u32 = 1 << 0;
const IMAN_ENABLE: u32 = 1 << 1;
const ERDP_BUSY: u64 = 1 << 3;

// A port's status and control (5.4.8).
pub const PORT_CONNECTED: u32 = 1 << 0;
pub const PORT_ENABLED: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
pub const PORT_CONNECT_CHANGE: u32 = 1 << 17;
const PORT_RESET_CHANGE: u32 = 1 << 21;
const PORT_POWER: u32 = 1 << 9;
/// The controller switches its ports' power, and after a reset they are off.
const HCC_PORT_POWER: u32 = 1 << 3;
/// What a write leaves as it was: the link state, power, the indicator and
/// what wakes it. Not what is cleared by writing one — the changes, and
/// enabled, which a one turns off — and not reset.
const PORT_KEEP: u32 = (0xF << 5) | (1 << 9) | (3 << 14) | (7 << 25);
const PORT_CHANGES: u32 = 0x7F << 17;

// Extended capabilities (7).
const XCAP_LEGACY: u32 = 1;
const XCAP_PROTOCOL: u32 = 2;
const LEGACY_FIRMWARE: u32 = 1 << 16;
const LEGACY_OS: u32 = 1 << 24;

fn read32(at: usize) -> u32 {
    unsafe { core::ptr::read_volatile(at as *const u32) }
}
fn write32(at: usize, v: u32) {
    unsafe { core::ptr::write_volatile(at as *mut u32, v) }
}
fn write64(at: usize, v: u64) {
    write32(at, v as u32);
    write32(at + 4, (v >> 32) as u32);
}

/// Wait up to `ms` milliseconds for `done`, one at a time.
pub fn until(ms: u64, done: impl Fn() -> bool) -> bool {
    for _ in 0..ms {
        if done() {
            return true;
        }
        syscall::sleep_ms(1);
    }
    done()
}

pub struct Hc {
    op: usize,
    rt: usize,
    db: usize,
    pub ports: u8,
    /// Bytes in a context: 32, or 64.
    pub context: usize,
    dcbaa: Page,
    commands: Ring,
    events: Events,
    pub interrupt: pci::Interrupt,
    /// The ports that are USB 3's, a bit each.
    usb3: [u64; 4],
    /// The endpoint a transfer is waiting on, and its events as they come.
    watching: (u8, u8),
    seen: [Trb; 8],
    nseen: usize,
    /// The last command's completion.
    done: Option<Trb>,
    /// Events for endpoints nobody is waiting on: reports, mostly.
    pub unclaimed: [Trb; 64],
    pub nunclaimed: usize,
    /// Root ports that said they changed, a bit each.
    pub changed: [u64; 4],
    /// Notifications from this program's other threads.
    pub told: u64,
    /// Who asked whether this is up (the device manager, settling its
    /// drivers before a session starts): answered once what was plugged in
    /// when the machine started has been seen to.
    pub pingers: [usize; 4],
    pub npingers: usize,
    /// The interrupt came, and is to be said to be over.
    pending: bool,
}

impl Hc {
    /// Take the controller from the firmware, reset it, and start it with a
    /// command ring and an event ring, its interrupt the best it has (`map`
    /// maps a BAR its MSI-X table may be in). The device is claimed and
    /// copies memory already.
    pub fn start(device: pci::Address, base: usize, map: impl FnOnce(usize) -> Option<usize>) -> Result<Hc, &'static str> {
        let op = base + (read32(base + CAPLENGTH) & 0xFF) as usize;
        let hcs1 = read32(base + HCSPARAMS1);
        let hcs2 = read32(base + HCSPARAMS2);
        let hcc1 = read32(base + HCCPARAMS1);
        let db = base + (read32(base + DBOFF) & !3) as usize;
        let rt = base + (read32(base + RTSOFF) & !0x1F) as usize;
        let ports = (hcs1 >> 24) as u8;
        let context = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };
        mem::set_low(hcc1 & 1 == 0);
        if read32(op + PAGESIZE) & 1 == 0 {
            return Err("the controller has no pages of 4 KiB");
        }

        // The extended capabilities: the firmware's claim on it, given up;
        // and which ports speak USB 3.
        let mut usb3 = [0u64; 4];
        let mut at = ((hcc1 >> 16) as usize) * 4;
        for _ in 0..64 {
            if at == 0 {
                break;
            }
            let word = read32(base + at);
            match word & 0xFF {
                XCAP_LEGACY => {
                    write32(base + at, word | LEGACY_OS);
                    until(1000, || read32(base + at) & LEGACY_FIRMWARE == 0);
                    // And none of its system-management interrupts.
                    let control = read32(base + at + 4);
                    write32(base + at + 4, (control & ((7 << 1) | (0xFF << 5) | (7 << 17))) | (7 << 29));
                }
                XCAP_PROTOCOL if word >> 24 == 3 => {
                    let ports = read32(base + at + 8);
                    let (first, count) = ((ports & 0xFF) as usize, ((ports >> 8) & 0xFF) as usize);
                    for p in first..(first + count).min(256) {
                        usb3[p / 64] |= 1 << (p % 64);
                    }
                }
                _ => {}
            }
            let next = ((word >> 8) & 0xFF) as usize * 4;
            at = if next == 0 { 0 } else { at + next };
        }

        // Stopped, reset, and ready.
        write32(op + USBCMD, read32(op + USBCMD) & !CMD_RUN);
        if !until(100, || read32(op + USBSTS) & STS_HALTED != 0) {
            return Err("the controller does not stop");
        }
        write32(op + USBCMD, CMD_RESET);
        if !until(1000, || read32(op + USBCMD) & CMD_RESET == 0 && read32(op + USBSTS) & STS_NOT_READY == 0) {
            return Err("the controller does not come out of its reset");
        }

        // Its slots; where each device's context is, the first entry naming
        // the pages it may keep its own state in; the command ring; the
        // event ring, one segment of a page.
        let slots = ((hcs1 & 0xFF) as usize).min(MAX_SLOTS);
        write32(op + CONFIG, slots as u32);
        let no_memory = "no memory for the controller";
        let dcbaa = mem::page().ok_or(no_memory)?;
        let scratch = (((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27)) as usize;
        if scratch > 0 {
            if scratch > 128 {
                return Err("the controller wants more pages of its own than are kept for it");
            }
            let array = mem::page().ok_or(no_memory)?;
            for i in 0..scratch {
                array.write64(i * 8, mem::page().ok_or(no_memory)?.phys);
            }
            dcbaa.write64(0, array.phys);
        }
        write64(op + DCBAAP, dcbaa.phys);
        let commands = Ring::new(mem::page().ok_or(no_memory)?);
        write64(op + CRCR, commands.page.phys | 1);
        let events = Events::new(mem::page().ok_or(no_memory)?);
        let segments = mem::page().ok_or(no_memory)?;
        segments.write64(0, events.page.phys);
        segments.write32(8, trb::TRBS as u32);
        write32(rt + ERSTSZ, 1);
        write64(rt + ERDP, events.page.phys);
        write64(rt + ERSTBA, segments.phys);
        write32(rt + IMAN, IMAN_ENABLE | IMAN_PENDING);

        let interrupt = pci::interrupt(device, map).ok_or("no interrupt to be had for the controller")?;
        write32(op + USBCMD, CMD_RUN | CMD_INTERRUPTS);
        if !until(100, || read32(op + USBSTS) & STS_HALTED == 0) {
            return Err("the controller does not start");
        }
        // Power to every port, where the controller switches it: a device
        // says nothing on a port with none (USB 2.0 7.1.7.1 gives it twenty
        // milliseconds to come up).
        if hcc1 & HCC_PORT_POWER != 0 {
            for port in 1..=ports as usize {
                let at = op + PORTSC + 0x10 * (port - 1);
                let status = read32(at);
                write32(at, (status & PORT_KEEP) | PORT_POWER);
            }
            syscall::sleep_ms(20);
        }
        Ok(Hc {
            op,
            rt,
            db,
            ports,
            context,
            dcbaa,
            commands,
            events,
            interrupt,
            usb3,
            watching: (0, 0),
            seen: [Trb::default(); 8],
            nseen: 0,
            done: None,
            unclaimed: [Trb::default(); 64],
            nunclaimed: 0,
            changed: [0; 4],
            told: 0,
            pingers: [0; 4],
            npingers: 0,
            pending: false,
        })
    }

    pub fn is_usb3(&self, port: u8) -> bool {
        self.usb3[port as usize / 64] & (1 << (port % 64)) != 0
    }

    pub fn portsc(&self, port: u8) -> u32 {
        read32(self.op + PORTSC + 0x10 * (port as usize - 1))
    }

    fn set_portsc(&self, port: u8, value: u32) {
        write32(self.op + PORTSC + 0x10 * (port as usize - 1), value);
    }

    /// Clear what port `port` says changed: its status, as it was.
    pub fn port_seen(&self, port: u8) -> u32 {
        let status = self.portsc(port);
        self.set_portsc(port, (status & PORT_KEEP) | (status & PORT_CHANGES));
        status
    }

    /// Reset a USB 2 port, as a device on one is before it is spoken to:
    /// whether it is enabled after. A USB 3 port is enabled by its link.
    pub fn port_reset(&self, port: u8) -> bool {
        if self.is_usb3(port) {
            return until(500, || self.portsc(port) & PORT_ENABLED != 0);
        }
        let status = self.portsc(port);
        self.set_portsc(port, (status & PORT_KEEP) | PORT_RESET);
        let reset = until(500, || self.portsc(port) & PORT_RESET_CHANGE != 0);
        let status = self.portsc(port);
        self.set_portsc(port, (status & PORT_KEEP) | PORT_RESET_CHANGE);
        reset && status & PORT_ENABLED != 0
    }

    /// The speed a port's device runs at (the protocol's numbering, which
    /// the slot's context takes as it is).
    pub fn port_speed(&self, port: u8) -> u8 {
        ((self.portsc(port) >> 10) & 0xF) as u8
    }

    pub fn ring(&self, slot: u8, target: u8) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write32(self.db + 4 * slot as usize, target as u32);
    }

    pub fn set_context(&self, slot: u8, phys: u64) {
        self.dcbaa.write64(8 * slot as usize, phys);
    }

    /// See to everything the controller has written: whether there was
    /// anything. Then tell it how far this has read, and say the interrupt
    /// is over.
    pub fn pump(&mut self) -> bool {
        let mut any = false;
        while let Some(event) = self.events.next() {
            any = true;
            match event.kind() {
                trb::COMMAND_COMPLETION => self.done = Some(event),
                trb::TRANSFER_EVENT if (event.slot(), event.endpoint()) == self.watching => {
                    if self.nseen < self.seen.len() {
                        self.seen[self.nseen] = event;
                        self.nseen += 1;
                    }
                }
                trb::TRANSFER_EVENT => {
                    if self.nunclaimed < self.unclaimed.len() {
                        self.unclaimed[self.nunclaimed] = event;
                        self.nunclaimed += 1;
                    }
                }
                trb::PORT_STATUS_CHANGE => {
                    let port = (event.param >> 24) as u8 as usize;
                    self.changed[port / 64] |= 1 << (port % 64);
                }
                _ => {}
            }
        }
        if any || self.pending {
            write64(self.rt + ERDP, self.events.dequeue() | ERDP_BUSY);
            write32(self.rt + IMAN, IMAN_ENABLE | IMAN_PENDING);
            write32(self.op + USBSTS, STS_EVENT);
            if self.pending && self.interrupt.is_line() {
                syscall::sys_irq_ack(self.interrupt.number());
            }
            self.pending = false;
        }
        any
    }

    /// Wait for the controller's interrupt or a notification, `ns` at most.
    /// A ping is kept to be answered when the loop says (`answer_pings`);
    /// nothing else is asked of this thread, and anything else is refused.
    pub fn wait(&mut self, ns: u64) {
        let mut msg = Message::empty();
        if syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(ns.max(10_000))).is_err() {
            return;
        }
        if msg.sender == 0 {
            if msg.tag == self.interrupt.number() as u64 {
                self.pending = true;
            } else if msg.tag == TAG_NOTIFICATION {
                self.told |= msg.data[0];
            }
        } else if msg.tag == TAG_PING && self.npingers < self.pingers.len() {
            self.pingers[self.npingers] = msg.sender;
            self.npingers += 1;
        } else {
            let _ = syscall::sys_reply(msg.sender, &Message { sender: 0, tag: u64::MAX, data: [0; 6] });
        }
    }

    /// Answer whoever asked whether this is up.
    pub fn answer_pings(&mut self) {
        for &tid in &self.pingers[..self.npingers] {
            let _ = syscall::sys_reply(tid, &Message { sender: 0, tag: TAG_PING, data: [0; 6] });
        }
        self.npingers = 0;
    }

    /// Run a command and wait for it, five seconds at most: its completion,
    /// or how it failed (0 for not at all).
    pub fn command(&mut self, command: Trb) -> Result<Trb, u8> {
        self.done = None;
        let at = self.commands.push(command);
        self.ring(0, 0);
        let start = syscall::sys_clock();
        loop {
            self.pump();
            if let Some(done) = self.done.filter(|d| d.param == at) {
                self.done = None;
                return if done.code() == trb::SUCCESS { Ok(done) } else { Err(done.code()) };
            }
            if syscall::sys_clock().wrapping_sub(start) > 5_000_000_000 {
                return Err(0);
            }
            self.wait(10_000_000);
        }
    }

    /// Put `trbs` on `ring` for endpoint `ep` of `slot`, ring its doorbell,
    /// and wait for the last of them, `seconds` at most: how much of it was
    /// not done, or how it failed (0 for not at all).
    pub fn transfer(&mut self, slot: u8, ep: u8, ring: &mut Ring, trbs: &[Trb], seconds: u64) -> Result<u32, u8> {
        self.watching = (slot, ep);
        self.nseen = 0;
        let mut last = 0;
        for t in trbs {
            last = ring.push(*t);
        }
        self.ring(slot, ep);
        let start = syscall::sys_clock();
        let mut residual = 0;
        let result = loop {
            self.pump();
            let mut finished = None;
            for event in &self.seen[..self.nseen] {
                if event.code() == trb::SHORT_PACKET {
                    residual += event.residual();
                }
                if !event.went_well() {
                    finished = Some(Err(event.code()));
                    break;
                }
                if event.param == last {
                    finished = Some(Ok(residual));
                    break;
                }
            }
            self.nseen = 0;
            if let Some(result) = finished {
                break result;
            }
            if syscall::sys_clock().wrapping_sub(start) > seconds * 1_000_000_000 {
                break Err(0);
            }
            self.wait(10_000_000);
        };
        self.watching = (0, 0);
        result
    }

    /// Start endpoint `ep` of `slot` again after it stopped on an error
    /// (`halted`), or was given up on, from where `ring` will next be
    /// written: whether it took.
    pub fn recover(&mut self, slot: u8, ep: u8, ring: &Ring, halted: bool) -> bool {
        let which = (slot as u32) << 24 | (ep as u32) << 16;
        let first = if halted { trb::RESET_ENDPOINT } else { trb::STOP_ENDPOINT };
        let _ = self.command(Trb::new(first, 0, 0, which));
        self.command(Trb::new(trb::SET_DEQUEUE, ring.dequeue(), 0, which)).is_ok()
    }
}
