#![no_std]
#![no_main]

//! A disk on virtio: QEMU's `virtio-blk`, and any other device that speaks
//! virtio's block protocol (virtio 1.x, §5.2).
//!
//! Started by the device manager for one, holding it. A request is three
//! buffers on the device's one queue: a header saying which sectors and
//! which way, the sectors themselves, and a byte the device answers in. The
//! sectors go straight between the device and the page `block::serve` reads
//! and writes them in, which is this program's own memory — so on a machine
//! with an IOMMU the device reaches that page and the request's, and
//! nothing else. Everything above the sectors — volumes, claims, the
//! partition table — is `quark_rt::block`, as it is for every disk.
//!
//! No cache to flush is agreed to: a device that may not keep writes in a
//! cache writes each through before it says it has, which is what the
//! journal above needs of a disk.

use quark_rt::block::{self, Device as Disk, MAX_SECTORS, SECTOR};
use quark_rt::manifest::CapReq;
use quark_rt::{pci, println, syscall, virtio};

// A driver's band; a virtio block device, as a transitional device names
// itself and as a modern one does; and frames for the queue and two pages.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x1AF4, 0x1001),
    CapReq::drives(0x1AF4, 0x1042),
    CapReq::phys_alloc(4),
]);

/// The page `block::serve` keeps sectors in, the page a request's header
/// and answer are in, and the queue's page.
const DATA_AT: usize = 0x86_0000_0000;
const REQUEST_AT: usize = 0x86_0000_1000;
const QUEUE_AT: usize = 0x86_0000_2000;
/// Where the device's registers are mapped, and the slots their ranges are
/// minted in.
const DEVICE_AT: usize = 0xA0_0000_0000;
const DEVICE_SLOTS: usize = 2;

const IN: u32 = 0;
const OUT: u32 = 1;
const FLUSH: u32 = 4;
const STATUS_OK: u8 = 0;
/// The device will not be written to.
const READ_ONLY: u64 = 1 << 5;
/// The device keeps what is written in a cache, and writes it out when it
/// is asked to; without this it says nothing of a cache, and is taken to
/// have written each write before it answered it.
const CAN_FLUSH: u64 = 1 << 9;

/// How long a request may take before the device is given up on.
const PATIENCE: u64 = 30;

struct Blk {
    device: virtio::Device,
    queue: virtio::Queue,
    sectors: u64,
    read_only: bool,
    can_flush: bool,
    data: u64,
    request: u64,
}

impl Blk {
    /// Move `count` sectors at `lba` between the device and `buf`, which is
    /// in the page sectors are kept in.
    fn transfer(&mut self, write: bool, lba: u64, count: u32, buf: usize) -> bool {
        let bytes = count as usize * SECTOR;
        if count == 0 || count > MAX_SECTORS || lba.checked_add(count as u64).is_none_or(|end| end > self.sectors) {
            return false;
        }
        let offset = buf.wrapping_sub(DATA_AT);
        if offset + bytes > 4096 {
            return false;
        }
        self.request(if write { OUT } else { IN }, lba, Some((self.data + offset as u64, bytes as u32, !write)))
    }

    /// Send a request of `kind` about `lba`, with data or without, and wait
    /// for its answer: whether it went well.
    fn request(&mut self, kind: u32, lba: u64, data: Option<(u64, u32, bool)>) -> bool {
        unsafe {
            core::ptr::write_volatile(REQUEST_AT as *mut u32, kind);
            core::ptr::write_volatile((REQUEST_AT + 4) as *mut u32, 0);
            core::ptr::write_volatile((REQUEST_AT + 8) as *mut u64, lba);
            core::ptr::write_volatile((REQUEST_AT + 16) as *mut u8, 0xFF);
        }
        let head = (self.request, 16, false);
        let status = (self.request + 16, 1, true);
        let added = match data {
            Some(d) => self.queue.add(&[head, d, status]),
            None => self.queue.add(&[head, status]),
        };
        if added.is_none() {
            return false;
        }
        self.queue.notify();
        let mut waited = 0;
        loop {
            if self.queue.take().is_some() {
                break;
            }
            if !self.device.wait(10) {
                waited += 1;
                if waited > PATIENCE {
                    println!("[virtblk] the device has not answered a request in {} seconds", PATIENCE / 10);
                    return false;
                }
            }
        }
        self.device.settle();
        unsafe { core::ptr::read_volatile((REQUEST_AT + 16) as *const u8) == STATUS_OK }
    }
}

impl Disk for Blk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool {
        into.len() >= count as usize * SECTOR && self.transfer(false, lba, count, into.as_ptr() as usize)
    }

    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool {
        !self.read_only && from.len() >= count as usize * SECTOR && self.transfer(true, lba, count, from.as_ptr() as usize)
    }

    fn flush(&mut self) -> bool {
        !self.can_flush || self.request(FLUSH, 0, None)
    }
}

/// A page of this program's own memory at `at`: where it is.
fn page(at: usize) -> Option<u64> {
    let frame = syscall::sys_phys_alloc(1).ok()?;
    syscall::sys_map_phys(frame, at, 1).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, 4096) };
    Some(frame as u64)
}

fn stop(why: &str) -> ! {
    println!("[virtblk] {}", why);
    syscall::sys_exit_code(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(address) = pci::this_device() else {
        stop("started without a device: the device manager starts this, for a virtio block device.");
    };
    let mut device = match virtio::Device::open(address, DEVICE_AT, DEVICE_SLOTS) {
        Ok(device) => device,
        Err(why) => stop(why),
    };
    let agreed = match device.accept(READ_ONLY | CAN_FLUSH) {
        Ok(agreed) => agreed,
        Err(why) => stop(why),
    };
    let queue = match device.queue(0, QUEUE_AT, 16) {
        Ok(queue) => queue,
        Err(why) => stop(why),
    };
    let (Some(data), Some(request)) = (page(DATA_AT), page(REQUEST_AT)) else {
        stop("no memory for the device to copy sectors through");
    };
    device.ready();
    // The device's size, in sectors of 512 bytes whatever its own are.
    let sectors = device.config64(0);
    let read_only = agreed & READ_ONLY != 0;
    let by = if device.by_message { "a message of its own" } else { "its line" };
    println!(
        "[virtblk] {} sectors ({} MiB){}, interrupt {} ({})",
        sectors,
        sectors / 2048,
        if read_only { ", read only" } else { "" },
        device.irq,
        by
    );
    let can_flush = agreed & CAN_FLUSH != 0;
    let mut blk = Blk { device, queue, sectors, read_only, can_flush, data, request };
    match block::register_disk() {
        Some(name) => println!("[virtblk] Registered as {}.", core::str::from_utf8(&name).unwrap_or("a disk")),
        None => stop("disk0 to disk3 are all taken"),
    }
    block::serve(&mut blk, DATA_AT)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[virtblk] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
