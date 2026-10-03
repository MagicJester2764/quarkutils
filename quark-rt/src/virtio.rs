//! virtio over PCI: what a driver for one of its devices needs of the
//! transport (virtio 1.x, §4.1).
//!
//! A virtio device is a PCI device whose registers are described by
//! capabilities of the vendor's kind: where its common configuration is
//! (features, status, the queues), where a queue is told it has something
//! (notify), where an interrupt says why (ISR), and where its own
//! configuration is. Each names a BAR and an offset into it, and [`Device`]
//! maps them — from the device's own capability, as any BAR is. Work is
//! handed over in *virtqueues* ([`Queue`]): a table of buffers by physical
//! address, a ring the driver puts chains of them on, and a ring the device
//! says it has finished one in. The memory for those is the driver's own
//! (`sys_phys_alloc`), which is what a claimed device reaches.
//!
//! Only "modern" devices are driven, and every one QEMU makes is one —
//! a transitional device (ids 0x1000 to 0x103F) has the same capabilities
//! beside its old I/O BAR. Interrupts are one message (MSI-X entry 0) for
//! every queue where the device has MSI-X, else its line.

use core::sync::atomic::{fence, Ordering};

use crate::ipc::Message;
use crate::pci::{self, Address};
use crate::syscall;

/// Every virtio device's vendor.
pub const VENDOR: u16 = 0x1AF4;

const CAPABILITY_VENDOR: u8 = 0x09;
const COMMON: u8 = 1;
const NOTIFY: u8 = 2;
const ISR: u8 = 3;
const CONFIG: u8 = 4;

// The common configuration (§4.1.4.3).
const DEVICE_FEATURE_SELECT: usize = 0x00;
const DEVICE_FEATURE: usize = 0x04;
const DRIVER_FEATURE_SELECT: usize = 0x08;
const DRIVER_FEATURE: usize = 0x0C;
const CONFIG_MSIX_VECTOR: usize = 0x10;
const DEVICE_STATUS: usize = 0x14;
const CONFIG_GENERATION: usize = 0x15;
const QUEUE_SELECT: usize = 0x16;
const QUEUE_SIZE: usize = 0x18;
const QUEUE_MSIX_VECTOR: usize = 0x1A;
const QUEUE_ENABLE: usize = 0x1C;
const QUEUE_NOTIFY_OFF: usize = 0x1E;
const QUEUE_DESC: usize = 0x20;
const QUEUE_DRIVER: usize = 0x28;
const QUEUE_DEVICE: usize = 0x30;

const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const FAILED: u8 = 128;

/// That the device is a virtio 1 device, whose structures are the ones
/// above: every device driven here must offer it.
pub const VERSION_1: u64 = 1 << 32;

const NO_VECTOR: u16 = 0xFFFF;

/// The most buffers a queue here holds: a page of them.
pub const MAX_QUEUE: u16 = 128;

/// How far apart the BARs are mapped from the `at` a driver gives.
const BAR_SPAN: usize = 1 << 28;

fn read8(at: usize) -> u8 {
    unsafe { core::ptr::read_volatile(at as *const u8) }
}
fn read16(at: usize) -> u16 {
    unsafe { core::ptr::read_volatile(at as *const u16) }
}
fn read32(at: usize) -> u32 {
    unsafe { core::ptr::read_volatile(at as *const u32) }
}
fn write8(at: usize, v: u8) {
    unsafe { core::ptr::write_volatile(at as *mut u8, v) }
}
fn write16(at: usize, v: u16) {
    unsafe { core::ptr::write_volatile(at as *mut u16, v) }
}
fn write32(at: usize, v: u32) {
    unsafe { core::ptr::write_volatile(at as *mut u32, v) }
}
fn write64(at: usize, v: u64) {
    write32(at, v as u32);
    write32(at + 4, (v >> 32) as u32);
}

/// One of the device's structures: which BAR, how far in, how long.
#[derive(Clone, Copy, Default)]
struct Place {
    bar: u8,
    offset: u32,
    length: u32,
    found: bool,
}

/// A virtio device, its structures mapped and its interrupt had.
pub struct Device {
    pub address: Address,
    common: usize,
    notify: usize,
    multiplier: u32,
    isr: usize,
    config: usize,
    config_len: usize,
    /// The interrupt every queue's completions arrive as.
    pub irq: u8,
    /// Whether that is a message of the device's own, or its line.
    pub by_message: bool,
    /// Whether it is entry 0 of the device's MSI-X table, which each queue
    /// is told to send.
    table: bool,
    /// Where each BAR is mapped, once it is.
    bars: [usize; 6],
    at: usize,
    slots: usize,
}

impl Device {
    /// Find the device's structures at `address` — which this program
    /// holds — map them at `at` onwards (a BAR every 256 MiB), minting in
    /// this program's slots from `slots` on; claim it; reset it; and have
    /// an interrupt for it. Then the driver negotiates ([`Device::accept`]),
    /// sets up its queues ([`Device::queue`]) and says it is ready
    /// ([`Device::ready`]).
    pub fn open(address: Address, at: usize, slots: usize) -> Result<Device, &'static str> {
        let info = pci::info(address).ok_or("this program does not hold the device")?;
        let mut places = [Place::default(); 5];
        let mut multiplier = 0;
        if pci::read16(address, 0x06).unwrap_or(0) & (1 << 4) != 0 {
            let mut cap = pci::read8(address, 0x34).unwrap_or(0) & 0xFC;
            for _ in 0..48 {
                if cap < 0x40 {
                    break;
                }
                let id = pci::read8(address, cap as u16).unwrap_or(0);
                if id == CAPABILITY_VENDOR {
                    let kind = pci::read8(address, cap as u16 + 3).unwrap_or(0);
                    let place = Place {
                        bar: pci::read8(address, cap as u16 + 4).unwrap_or(0xFF),
                        offset: pci::read32(address, cap as u16 + 8).unwrap_or(0),
                        length: pci::read32(address, cap as u16 + 12).unwrap_or(0),
                        found: true,
                    };
                    // The first of each kind the driver can use, as the
                    // specification asks.
                    if (1..=4).contains(&kind) && !places[kind as usize].found && place.bar < 6 {
                        places[kind as usize] = place;
                        if kind == NOTIFY {
                            multiplier = pci::read32(address, cap as u16 + 16).unwrap_or(0);
                        }
                    }
                }
                cap = pci::read8(address, cap as u16 + 1).unwrap_or(0) & 0xFC;
            }
        }
        if !places[COMMON as usize].found || !places[NOTIFY as usize].found {
            return Err("the device has no virtio 1 structures");
        }
        let mut device = Device {
            address,
            common: 0,
            notify: 0,
            multiplier,
            isr: 0,
            config: 0,
            config_len: places[CONFIG as usize].length as usize,
            irq: 0,
            by_message: false,
            table: false,
            bars: [0; 6],
            at,
            slots,
        };
        let mut window = |place: &Place| -> Result<usize, &'static str> {
            if !place.found {
                return Ok(0);
            }
            let bar = place.bar as usize;
            let bar_size = info.bars[bar].size;
            if place.offset as u64 + place.length as u64 > bar_size {
                return Err("a structure runs past its BAR");
            }
            Ok(device.map(bar)? + place.offset as usize)
        };
        let common = window(&places[COMMON as usize])?;
        let notify = window(&places[NOTIFY as usize])?;
        let isr = window(&places[ISR as usize])?;
        let config = window(&places[CONFIG as usize])?;
        device.common = common;
        device.notify = notify;
        device.isr = isr;
        device.config = config;

        // The device is this program's before it may copy anything.
        pci::claim(address).map_err(|_| "the device is another program's")?;
        pci::enable(address, pci::COMMAND_MEMORY | pci::COMMAND_MASTER)
            .map_err(|_| "the device may not be turned on")?;

        // Start again from nothing: written 0, it says 0 when it has.
        write8(device.common + DEVICE_STATUS, 0);
        for _ in 0..1000 {
            if read8(device.common + DEVICE_STATUS) == 0 {
                break;
            }
            syscall::sleep_ns(10_000);
        }
        write8(device.common + DEVICE_STATUS, ACKNOWLEDGE);
        write8(device.common + DEVICE_STATUS, ACKNOWLEDGE | DRIVER);

        // Its interrupt: a message, where it has MSI-X — entry 0 of its
        // table, in its own registers — else its line.
        let interrupt = pci::interrupt(address, |bar| device.map(bar).ok())
            .ok_or("no interrupt to be had for the device")?;
        device.irq = interrupt.number();
        device.by_message = !interrupt.is_line();
        device.table = matches!(interrupt, pci::Interrupt::Table(_));
        if device.table {
            write16(device.common + CONFIG_MSIX_VECTOR, NO_VECTOR);
        }
        Ok(device)
    }

    /// Map BAR `n`, if it is not yet: where it begins.
    fn map(&mut self, n: usize) -> Result<usize, &'static str> {
        if self.bars[n] == 0 {
            let at = self.at + n * BAR_SPAN;
            self.bars[n] = pci::map_bar(self.address, n, at, self.slots + n).ok_or("may not map the device's registers")?;
        }
        Ok(self.bars[n])
    }

    /// What the device offers, and the driver wants: agree on what both
    /// do, and say so. What was agreed; `Err` if the device is not a
    /// virtio 1 device, or will not have it.
    pub fn accept(&mut self, wanted: u64) -> Result<u64, &'static str> {
        let c = self.common;
        write32(c + DEVICE_FEATURE_SELECT, 0);
        let low = read32(c + DEVICE_FEATURE) as u64;
        write32(c + DEVICE_FEATURE_SELECT, 1);
        let offered = low | (read32(c + DEVICE_FEATURE) as u64) << 32;
        if offered & VERSION_1 == 0 {
            self.fail();
            return Err("the device is not a virtio 1 device");
        }
        let agreed = offered & (wanted | VERSION_1);
        write32(c + DRIVER_FEATURE_SELECT, 0);
        write32(c + DRIVER_FEATURE, agreed as u32);
        write32(c + DRIVER_FEATURE_SELECT, 1);
        write32(c + DRIVER_FEATURE, (agreed >> 32) as u32);
        let status = read8(c + DEVICE_STATUS);
        write8(c + DEVICE_STATUS, status | FEATURES_OK);
        if read8(c + DEVICE_STATUS) & FEATURES_OK == 0 {
            self.fail();
            return Err("the device would not have what was agreed");
        }
        Ok(agreed)
    }

    /// Set queue `index` up in a page of this program's own memory, mapped
    /// at `at`, with at most `most` buffers.
    pub fn queue(&mut self, index: u16, at: usize, most: u16) -> Result<Queue, &'static str> {
        let c = self.common;
        write16(c + QUEUE_SELECT, index);
        let offered = read16(c + QUEUE_SIZE);
        if offered == 0 {
            return Err("the device has no such queue");
        }
        // A power of two, as a split queue's size is.
        let mut size = offered.min(most).min(MAX_QUEUE);
        while size & (size - 1) != 0 {
            size &= size - 1;
        }
        let frame = syscall::sys_phys_alloc(1).map_err(|_| "no memory for a queue")?;
        syscall::sys_map_phys(frame, at, 1).map_err(|_| "cannot map a queue")?;
        unsafe { core::ptr::write_bytes(at as *mut u8, 0, 4096) };
        let used = (Queue::AVAIL + 6 + 2 * size as usize + 3) & !3;
        write16(c + QUEUE_SIZE, size);
        write64(c + QUEUE_DESC, frame as u64);
        write64(c + QUEUE_DRIVER, (frame + Queue::AVAIL) as u64);
        write64(c + QUEUE_DEVICE, (frame + used) as u64);
        if self.table {
            write16(c + QUEUE_MSIX_VECTOR, 0);
            if read16(c + QUEUE_MSIX_VECTOR) != 0 {
                return Err("the device would not send that queue's message");
            }
        }
        let notify_at = self.notify + read16(c + QUEUE_NOTIFY_OFF) as usize * self.multiplier as usize;
        write16(c + QUEUE_ENABLE, 1);
        let mut queue = Queue { index, size, at, frame: frame as u64, used, notify_at, free: 0, free_count: size, next: 0, seen: 0 };
        for i in 0..size {
            queue.desc(i).next = i + 1;
        }
        Ok(queue)
    }

    /// Everything is set up: the device may begin.
    pub fn ready(&mut self) {
        let status = read8(self.common + DEVICE_STATUS);
        write8(self.common + DEVICE_STATUS, status | DRIVER_OK);
    }

    fn fail(&mut self) {
        let status = read8(self.common + DEVICE_STATUS);
        write8(self.common + DEVICE_STATUS, status | FAILED);
    }

    /// Have the device say when its own configuration changes — a display
    /// whose size the host changed — with the message its queues send:
    /// whether it will. A device on its line raises it for that anyway.
    /// The driver asks its configuration what changed, whatever woke it.
    pub fn hear_changes(&self) -> bool {
        if !self.table {
            return true;
        }
        write16(self.common + CONFIG_MSIX_VECTOR, 0);
        read16(self.common + CONFIG_MSIX_VECTOR) == 0
    }

    /// Write a 32-bit field of the device's own configuration, at `offset`.
    pub fn set_config32(&self, offset: usize, value: u32) {
        if self.config != 0 && offset + 4 <= self.config_len {
            write32(self.config + offset, value);
        }
    }

    /// A byte of the device's own configuration, at `offset`.
    pub fn config8(&self, offset: usize) -> u8 {
        if self.config == 0 || offset >= self.config_len {
            return 0;
        }
        read8(self.config + offset)
    }

    /// A 32-bit field of the device's own configuration, at `offset`.
    pub fn config32(&self, offset: usize) -> u32 {
        if self.config == 0 || offset + 4 > self.config_len {
            return 0;
        }
        read32(self.config + offset)
    }

    /// A 64-bit one: two readings, again if the device changed it between
    /// them.
    pub fn config64(&self, offset: usize) -> u64 {
        if self.config == 0 || offset + 8 > self.config_len {
            return 0;
        }
        loop {
            let before = read8(self.common + CONFIG_GENERATION);
            let value = read32(self.config + offset) as u64 | (read32(self.config + offset + 4) as u64) << 32;
            if read8(self.common + CONFIG_GENERATION) == before {
                return value;
            }
        }
    }

    /// Say that whatever the device's line was raised for has been dealt
    /// with: where it interrupts by its line, a reading of the ISR is what
    /// lowers it, and the controller is told. A message needs nothing.
    pub fn settle(&self) {
        if !self.by_message {
            if self.isr != 0 {
                let _ = read8(self.isr);
            }
            syscall::sys_irq_ack(self.irq);
        }
    }

    /// Wait for the device's interrupt, `span` at most (see
    /// `syscall::sys_recv_timeout`): whether it came. A line is said to be
    /// dealt with before this returns; a message needs nothing said.
    /// Anything else the kernel says meanwhile is kept for the driver's
    /// loop (`ipc::keep`).
    pub fn wait(&self, span: u64) -> bool {
        let mut msg = Message::empty();
        loop {
            if syscall::sys_recv_timeout(0, &mut msg, span).is_err() {
                return false;
            }
            if msg.sender == 0 && msg.tag == self.irq as u64 {
                self.settle();
                return true;
            }
            crate::ipc::keep(&msg);
        }
    }
}

/// One of a queue's buffer descriptors (§2.7.5).
#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

/// A split virtqueue in one page: the descriptors, then the ring the driver
/// puts chains on, then the ring the device gives them back in.
pub struct Queue {
    pub index: u16,
    size: u16,
    at: usize,
    frame: u64,
    used: usize,
    notify_at: usize,
    /// The first free descriptor, and how many there are.
    free: u16,
    free_count: u16,
    /// The driver's ring's next index, and how far the device's has been
    /// read.
    next: u16,
    seen: u16,
}

impl Queue {
    /// Where the driver's ring is in the page: after 128 descriptors.
    const AVAIL: usize = 16 * MAX_QUEUE as usize;

    fn desc(&mut self, i: u16) -> &mut Desc {
        unsafe { &mut *((self.at + 16 * i as usize) as *mut Desc) }
    }

    /// How many buffers it holds.
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Put a chain of buffers on the queue for the device: each a physical
    /// address, a length, and whether the device writes it rather than
    /// reads it. The chain's first descriptor, which is what [`take`] gives
    /// back; `None` when there is not room.
    ///
    /// [`take`]: Queue::take
    pub fn add(&mut self, parts: &[(u64, u32, bool)]) -> Option<u16> {
        if parts.is_empty() || parts.len() > self.free_count as usize {
            return None;
        }
        let head = self.free;
        let mut i = head;
        for (n, &(addr, len, written)) in parts.iter().enumerate() {
            let last = n + 1 == parts.len();
            let next = self.desc(i).next;
            let d = self.desc(i);
            d.addr = addr;
            d.len = len;
            d.flags = if last { 0 } else { DESC_NEXT } | if written { DESC_WRITE } else { 0 };
            if last {
                self.free = next;
            } else {
                i = next;
            }
        }
        self.free_count -= parts.len() as u16;
        let ring = self.at + Self::AVAIL;
        let slot = ring + 4 + 2 * (self.next % self.size) as usize;
        write16(slot, head);
        // The descriptors and the entry before the index that hands them
        // over.
        fence(Ordering::SeqCst);
        self.next = self.next.wrapping_add(1);
        write16(ring + 2, self.next);
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// Tell the device the queue has something.
    pub fn notify(&self) {
        write16(self.notify_at, self.index);
    }

    /// A chain the device has finished with: its first descriptor, and how
    /// many bytes it wrote. Its descriptors are free again.
    pub fn take(&mut self) -> Option<(u16, u32)> {
        let ring = self.at + self.used;
        if read16(ring + 2) == self.seen {
            return None;
        }
        fence(Ordering::SeqCst);
        let entry = ring + 4 + 8 * (self.seen % self.size) as usize;
        let (head, written) = (read32(entry) as u16, read32(entry + 4));
        self.seen = self.seen.wrapping_add(1);
        // Give the chain back to the free list.
        let mut i = head;
        let mut count = 1;
        while self.desc(i).flags & DESC_NEXT != 0 && count < self.size {
            i = self.desc(i).next;
            count += 1;
        }
        let free = self.free;
        self.desc(i).next = free;
        self.free = head;
        self.free_count += count;
        Some((head, written))
    }

    /// Where the queue's page is in physical memory, for a driver that
    /// puts its own buffers in what is left of it.
    pub fn frame(&self) -> u64 {
        self.frame
    }
}
