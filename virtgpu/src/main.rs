#![no_std]
#![no_main]

//! A display on virtio: QEMU's `virtio-gpu`, its 2D half (virtio 1.x, §5.7).
//!
//! Started by the device manager for one, holding it. A virtio GPU shows a
//! *resource* — a picture the host keeps — on a scanout, and a resource's
//! picture is copied to the host from memory the guest gives it, a part at a
//! time, when the guest says to. So this asks the device how big its display
//! is, has the kernel give the device's screen memory (`SYS_DISPLAY_MEMORY`:
//! nobody's, and the device reaches it), makes a resource that size backed
//! by it and puts it on the display, and offers the display to `fb`, which
//! lends it out as it lends the bootloader's framebuffer. Then whoever has
//! the display draws into the memory and says which parts it drew on — to
//! `fb`, which passes it on (`quark_rt::display`) — and this copies those
//! parts to the host and has it show them.
//!
//! Another size comes two ways: the host's window changes size, which the
//! device says (its display event), or whoever has the display asks for one
//! through `fb`. The screen's memory was asked for big enough for the most
//! this offers, and a new mode is a new picture of the new size from the
//! same memory, shown in the old one's place, and the display offered
//! again — which `fb` hands its holder back in.

use quark_rt::ipc::{Message, TAG_NOTIFICATION, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{display, nameserver, pci, println, syscall, virtio};

// A driver's band; a virtio GPU (modern, which is all it is); and frames for
// its queue and a page of requests.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives(0x1AF4, 0x1050),
    CapReq::phys_alloc(4),
]);

/// Where the device's registers are mapped, and the slots their ranges are
/// minted in; the control queue's page; a page of requests, the answer in
/// its second half.
const DEVICE_AT: usize = 0xA0_0000_0000;
const DEVICE_SLOTS: usize = 2;
const QUEUE_AT: usize = 0x86_0000_0000;
const REQUEST_AT: usize = 0x86_0000_1000;
const ANSWER: usize = 2048;
/// Where the capability over the screen's memory is kept.
const SCREEN_SLOT: usize = 20;

/// The screen is asked for at least this big, so that a bigger mode later
/// has room: 1920 by 1200, four bytes a pixel.
const ROOM: u64 = 1920 * 1200 * 4;
/// What is shown when the device says nothing of its display's size.
const DEFAULT: (u32, u32) = (1280, 800);

// Commands and answers (§5.7.6).
const GET_DISPLAY_INFO: u32 = 0x0100;
const RESOURCE_CREATE_2D: u32 = 0x0101;
const RESOURCE_UNREF: u32 = 0x0102;
const SET_SCANOUT: u32 = 0x0103;
const RESOURCE_FLUSH: u32 = 0x0104;
const TRANSFER_TO_HOST_2D: u32 = 0x0105;
const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const OK_NODATA: u32 = 0x1100;
const OK_DISPLAY_INFO: u32 = 0x1101;
/// Blue, green, red and one byte nothing reads: what `fb`'s clients draw
/// as `0x00RRGGBB` in a little-endian word.
const FORMAT_B8G8R8X8: u32 = 2;

// What fb is told, and asked.
const TAG_FB_DRIVER: u64 = 5;
const TAG_FB_DISPLAY: u64 = 6;
const TAG_GPU_MODE: u64 = 1;
/// The largest mode asked for that is answered: no wider or taller.
const LARGEST: u32 = 8192;

/// How long a command may take: tenths of a second.
const PATIENCE: u32 = 50;

/// The device's own configuration (§5.7.4): what it has to say, and where
/// the driver says it has heard it.
const EVENTS_READ: usize = 0;
const EVENTS_CLEAR: usize = 4;
/// The display's size has changed.
const EVENT_DISPLAY: u32 = 1;

fn write32(at: usize, v: u32) {
    unsafe { core::ptr::write_volatile((REQUEST_AT + at) as *mut u32, v) }
}
fn write64(at: usize, v: u64) {
    write32(at, v as u32);
    write32(at + 4, (v >> 32) as u32);
}
fn read32(at: usize) -> u32 {
    unsafe { core::ptr::read_volatile((REQUEST_AT + at) as *const u32) }
}

struct Gpu {
    device: virtio::Device,
    queue: virtio::Queue,
    request: u64,
    width: u32,
    height: u32,
    /// The picture shown: 1 or 2, a new mode's made beside the old.
    resource: u32,
    /// The screen's memory: where, and how much.
    base: u64,
    bytes: u64,
}

impl Gpu {
    /// Begin a command of `kind`: its header, written.
    fn header(&self, kind: u32) {
        unsafe { core::ptr::write_bytes(REQUEST_AT as *mut u8, 0, 4096) };
        write32(0, kind);
    }

    /// Send the command written, `len` bytes, and wait for its answer, of
    /// `answer` bytes at most: what kind of answer it was.
    fn run(&mut self, len: u32, answer: u32) -> Option<u32> {
        let parts = [(self.request, len, false), (self.request + ANSWER as u64, answer, true)];
        self.queue.add(&parts)?;
        self.queue.notify();
        let mut waited = 0;
        loop {
            if self.queue.take().is_some() {
                break;
            }
            if !self.device.wait(10) {
                waited += 1;
                if waited > PATIENCE {
                    println!("[virtgpu] the device has not answered a command in {} seconds", PATIENCE / 10);
                    return None;
                }
            }
        }
        self.device.settle();
        Some(read32(ANSWER))
    }

    /// A rectangle, four words at `at`.
    fn rect(at: usize, x: u32, y: u32, w: u32, h: u32) {
        write32(at, x);
        write32(at + 4, y);
        write32(at + 8, w);
        write32(at + 12, h);
    }

    /// The first scanout's size, as the device says it.
    fn display_size(&mut self) -> Option<(u32, u32)> {
        self.header(GET_DISPLAY_INFO);
        if self.run(24, 24 + 16 * 24)? != OK_DISPLAY_INFO {
            return None;
        }
        let (w, h, enabled) = (read32(ANSWER + 24 + 8), read32(ANSWER + 24 + 12), read32(ANSWER + 24 + 16));
        (enabled != 0 && w != 0 && h != 0).then_some((w, h))
    }

    /// Make a picture `w` by `h`, back it with the screen's memory, and
    /// show it in place of the one shown, which is let go.
    fn show(&mut self, w: u32, h: u32) -> bool {
        let old = self.resource;
        let new = if old == 1 { 2 } else { 1 };
        self.header(RESOURCE_CREATE_2D);
        write32(24, new);
        write32(28, FORMAT_B8G8R8X8);
        write32(32, w);
        write32(36, h);
        if self.run(40, 24) != Some(OK_NODATA) {
            return false;
        }
        self.header(RESOURCE_ATTACH_BACKING);
        write32(24, new);
        write32(28, 1);
        write64(32, self.base);
        write32(40, w * h * 4);
        if self.run(48, 24) != Some(OK_NODATA) {
            return false;
        }
        self.header(SET_SCANOUT);
        Self::rect(24, 0, 0, w, h);
        write32(40, 0);
        write32(44, new);
        if self.run(48, 24) != Some(OK_NODATA) {
            return false;
        }
        if old != 0 {
            self.header(RESOURCE_UNREF);
            write32(24, old);
            let _ = self.run(32, 24);
        }
        self.resource = new;
        self.width = w;
        self.height = h;
        true
    }

    /// Whether a mode `w` by `h` fits what this has.
    fn fits(&self, w: u32, h: u32) -> bool {
        w != 0 && h != 0 && w <= LARGEST && h <= LARGEST && w as u64 * h as u64 * 4 <= self.bytes
    }

    /// Show the display `w` by `h` and offer it to `fb` so: whether it is.
    /// Nothing is copied to the new picture until its holder has drawn in
    /// it and said so: what is in memory until then is the old one's rows,
    /// laid out for another width, and shown it is a screen of stripes.
    fn resize(&mut self, w: u32, h: u32) -> bool {
        if !self.show(w, h) {
            println!("[virtgpu] the device would not show {}x{}", w, h);
            return false;
        }
        if offer(self.base, w, h).is_none() {
            println!("[virtgpu] fb would not take {}x{}", w, h);
            return false;
        }
        true
    }

    /// Whether the device has said its display changed size, heard: what
    /// size it is now, if that is one to follow.
    fn display_changed(&mut self) -> Option<(u32, u32)> {
        if self.device.config32(EVENTS_READ) & EVENT_DISPLAY == 0 {
            return None;
        }
        self.device.set_config32(EVENTS_CLEAR, EVENT_DISPLAY);
        let (w, h) = self.display_size()?;
        if (w, h) == (self.width, self.height) {
            return None;
        }
        if !self.fits(w, h) {
            println!("[virtgpu] the display is {}x{}, more than the screen's memory: left as it is", w, h);
            return None;
        }
        Some((w, h))
    }

    /// Copy `[x0, x1) × [y0, y1)` to the host, and have it show it.
    fn flush(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        let (w, h) = (x1 - x0, y1 - y0);
        self.header(TRANSFER_TO_HOST_2D);
        Self::rect(24, x0, y0, w, h);
        write64(40, (y0 as u64 * self.width as u64 + x0 as u64) * 4);
        write32(48, self.resource);
        if self.run(56, 24) != Some(OK_NODATA) {
            return;
        }
        self.header(RESOURCE_FLUSH);
        Self::rect(24, x0, y0, w, h);
        write32(40, self.resource);
        let _ = self.run(48, 24);
    }
}

fn stop(why: &str) -> ! {
    println!("[virtgpu] {}", why);
    syscall::sys_exit_code(1);
}

/// A page of this program's own memory at `at`: where it is.
fn page(at: usize) -> Option<u64> {
    let frame = syscall::sys_phys_alloc(1).ok()?;
    syscall::sys_map_phys(frame, at, 1).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, 4096) };
    Some(frame as u64)
}

/// Offer the display to `fb`: this program as its driver, and then the mode
/// with the screen's memory.
fn offer(base: u64, width: u32, height: u32) -> Option<usize> {
    let fb = nameserver::lookup_retry(b"fb", 30)?;
    let mut reply = Message::empty();
    let msg = Message { sender: 0, tag: TAG_FB_DRIVER, data: [0; 6] };
    if syscall::sys_call_offer_self(fb, &msg, &mut reply).is_err() || reply.tag != 0 {
        return None;
    }
    let (w, h) = (width as u64, height as u64);
    let mode = Message {
        sender: 0,
        tag: TAG_FB_DISPLAY,
        data: [base, w << 32 | h, (w * 4) << 32 | 32, 16 << 16 | 8 << 8, 0, 0],
    };
    if syscall::sys_call_offer(fb, &mode, &mut reply, SCREEN_SLOT).is_err() || reply.tag != 0 {
        return None;
    }
    Some(fb)
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(address) = pci::this_device() else {
        stop("started without a device: the device manager starts this, for a virtio GPU.");
    };
    let mut device = match virtio::Device::open(address, DEVICE_AT, DEVICE_SLOTS) {
        Ok(device) => device,
        Err(why) => stop(why),
    };
    if let Err(why) = device.accept(0) {
        stop(why);
    }
    if !device.hear_changes() {
        println!("[virtgpu] the device will not say when its display changes size");
    }
    let queue = match device.queue(0, QUEUE_AT, 16) {
        Ok(queue) => queue,
        Err(why) => stop(why),
    };
    let Some(request) = page(REQUEST_AT) else {
        stop("no memory for the device's commands");
    };
    device.ready();
    let mut gpu = Gpu { device, queue, request, width: 0, height: 0, resource: 0, base: 0, bytes: 0 };
    let (width, height) = gpu.display_size().unwrap_or(DEFAULT);

    let pages = (width as u64 * height as u64 * 4).max(ROOM).div_ceil(4096);
    let _ = syscall::sys_cap_delete(SCREEN_SLOT);
    let Ok(base) = syscall::sys_display_memory(address.raw(), pages, SCREEN_SLOT) else {
        stop("no memory to be had for the screen");
    };
    gpu.base = base;
    gpu.bytes = pages * 4096;
    if !gpu.show(width, height) {
        stop("the device would not show a picture");
    }
    gpu.flush(0, 0, width, height);
    println!(
        "[virtgpu] {}x{}, interrupt {} ({})",
        width,
        height,
        gpu.device.irq,
        if gpu.device.by_message { "a message of its own" } else { "its line" }
    );
    let Some(fb) = offer(base, width, height) else {
        stop("fb would not take the display");
    };

    // What was drawn on, as tiles, until it has been copied: from fb, as a
    // notification.
    let mut drawn = 0u64;
    loop {
        // Asked each time round and not only when the device interrupts: its
        // message may have been taken while a command was waited for.
        if let Some((w, h)) = gpu.display_changed() {
            if gpu.resize(w, h) {
                drawn = 0;
            }
        }
        let mut msg = Message::empty();
        if let Some(earlier) = quark_rt::ipc::kept() {
            msg = earlier;
        } else if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        if msg.sender == 0 {
            if msg.tag == TAG_NOTIFICATION {
                drawn |= msg.data[0];
            } else if msg.tag == gpu.device.irq as u64 {
                gpu.device.settle();
            }
        } else if msg.sender == fb && msg.tag == TAG_GPU_MODE {
            // Answered first: fb is waiting, and the new mode is offered to
            // it with a call of this program's own.
            let (w, h) = ((msg.data[0] >> 32) as u32, msg.data[0] as u32);
            let fits = gpu.fits(w, h);
            let answer = Message { sender: 0, tag: if fits { 0 } else { u64::MAX }, data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &answer);
            if fits && (w, h) != (gpu.width, gpu.height) && gpu.resize(w, h) {
                drawn = 0;
            }
            continue;
        } else {
            let _ = syscall::sys_reply(msg.sender, &Message { sender: 0, tag: u64::MAX, data: [0; 6] });
        }
        if let Some((x0, y0, x1, y1)) = display::bounds(drawn, gpu.width as u64, gpu.height as u64) {
            drawn = 0;
            gpu.flush(x0 as u32, y0 as u32, x1 as u32, y1 as u32);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[virtgpu] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
