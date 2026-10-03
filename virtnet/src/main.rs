#![no_std]
#![no_main]

//! A network card on virtio: QEMU's `virtio-net`, and any other device that
//! speaks virtio's network protocol (virtio 1.x, §5.1).
//!
//! Started by the device manager for one, holding it. A frame goes each way
//! in a queue of its own, with a header in front saying nothing more than
//! "a frame" here: no offloads are asked for. Sixteen buffers wait in the
//! receive queue for what comes; each frame is copied out of one when the
//! stack asks for it, and the buffer goes back. What it serves is
//! `quark_rt::nic`, as every card's driver does.

use quark_rt::manifest::CapReq;
use quark_rt::nic::{self, Card};
use quark_rt::{pci, println, syscall, virtio};

// A driver's band; a virtio network card, as a transitional device names
// itself and as a modern one does; and frames for two queues, the buffers
// frames come into and the one they go out of.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x1AF4, 0x1000),
    CapReq::drives(0x1AF4, 0x1041),
    CapReq::phys_alloc(16),
]);

/// The device says its address in its configuration.
const F_MAC: u64 = 1 << 5;
/// The header before every frame, with `VERSION_1`.
const HEADER: usize = 12;
/// Buffers frames come into, each this long.
const BUFFERS: usize = 16;
const BUFFER: usize = 2048;

const RX_QUEUE_AT: usize = 0x86_0000_0000;
const TX_QUEUE_AT: usize = 0x86_0000_1000;
const TX_AT: usize = 0x86_0000_2000;
const RX_AT: usize = 0x86_0001_0000;
const DEVICE_AT: usize = 0xA0_0000_0000;
const DEVICE_SLOTS: usize = 2;

struct Net {
    device: virtio::Device,
    rx: virtio::Queue,
    tx: virtio::Queue,
    mac: [u8; 6],
    /// Where the receive buffers are, and which one each descriptor at the
    /// head of a chain is.
    rx_frame: u64,
    posted: [u8; virtio::MAX_QUEUE as usize],
    tx_frame: u64,
}

impl Net {
    /// Give buffer `n` to the device to receive into.
    fn post(&mut self, n: usize) -> bool {
        match self.rx.add(&[(self.rx_frame + (n * BUFFER) as u64, BUFFER as u32, true)]) {
            Some(head) => {
                self.posted[head as usize] = n as u8;
                true
            }
            None => false,
        }
    }
}

impl Card for Net {
    fn address(&self) -> [u8; 6] {
        self.mac
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() > nic::FRAME {
            return false;
        }
        unsafe {
            core::ptr::write_bytes(TX_AT as *mut u8, 0, HEADER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), (TX_AT + HEADER) as *mut u8, frame.len());
        }
        if self.tx.add(&[(self.tx_frame, (HEADER + frame.len()) as u32, false)]).is_none() {
            return false;
        }
        self.tx.notify();
        // One buffer to send from: it is free when the device says so.
        for _ in 0..200 {
            if self.tx.take().is_some() {
                return true;
            }
            syscall::sleep_ns(50_000);
        }
        println!("[virtnet] the device did not send a frame");
        false
    }

    fn receive(&mut self, into: &mut [u8]) -> usize {
        let Some((head, written)) = self.rx.take() else {
            return 0;
        };
        let n = self.posted[head as usize] as usize;
        let len = (written as usize).saturating_sub(HEADER).min(into.len()).min(nic::FRAME);
        unsafe {
            core::ptr::copy_nonoverlapping((RX_AT + n * BUFFER + HEADER) as *const u8, into.as_mut_ptr(), len)
        };
        self.post(n);
        self.rx.notify();
        len
    }

    fn interrupt(&mut self) -> bool {
        self.device.settle();
        true
    }

    fn irq(&self) -> u8 {
        self.device.irq
    }
}

fn stop(why: &str) -> ! {
    println!("[virtnet] {}", why);
    syscall::sys_exit_code(1);
}

/// `count` pages of this program's memory in a row, mapped at `at`: where
/// they are.
fn pages(count: usize, at: usize) -> Option<u64> {
    let first = syscall::sys_phys_alloc(count).ok()?;
    syscall::sys_map_phys(first, at, count).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, count * 4096) };
    Some(first as u64)
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(address) = pci::this_device() else {
        stop("started without a card: the device manager starts this, for a virtio network card.");
    };
    let mut device = match virtio::Device::open(address, DEVICE_AT, DEVICE_SLOTS) {
        Ok(device) => device,
        Err(why) => stop(why),
    };
    let agreed = match device.accept(F_MAC) {
        Ok(agreed) => agreed,
        Err(why) => stop(why),
    };
    let rx = match device.queue(0, RX_QUEUE_AT, BUFFERS as u16) {
        Ok(queue) => queue,
        Err(why) => stop(why),
    };
    let tx = match device.queue(1, TX_QUEUE_AT, 4) {
        Ok(queue) => queue,
        Err(why) => stop(why),
    };
    let (Some(rx_frame), Some(tx_frame)) = (pages(BUFFERS * BUFFER / 4096, RX_AT), pages(1, TX_AT)) else {
        stop("no memory for frames");
    };
    // Its own address, or one of the kind nobody is given at a factory.
    let mut mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    if agreed & F_MAC != 0 {
        for (i, b) in mac.iter_mut().enumerate() {
            *b = device.config8(i);
        }
    }
    let mut net = Net { device, rx, tx, mac, rx_frame, posted: [0; virtio::MAX_QUEUE as usize], tx_frame };
    let room = (net.rx.size() as usize).min(BUFFERS);
    for n in 0..room {
        net.post(n);
    }
    net.device.ready();
    net.rx.notify();

    let Some(name) = nic::register() else {
        stop("eth0 to eth7 are all taken");
    };
    let by = if net.device.by_message { "a message of its own" } else { "its line" };
    println!(
        "[virtnet] {}, interrupt {} ({}), address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        core::str::from_utf8(&name).unwrap_or("a card"),
        net.device.irq,
        by,
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    nic::serve(&mut net)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[virtnet] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
