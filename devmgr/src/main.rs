#![no_std]
#![no_main]

//! The device manager: what is in the machine, and who drives it.
//!
//! The kernel finds every PCI device at boot and keeps what it found; this
//! program holds every one of them (`CapReq::pci_devices`) and is the only
//! one that does, `init` aside. It starts the drivers. A driver says in its
//! own manifest which devices it drives (`CapReq::drives`, `drives_class`,
//! `drives_interface`), and for each device in the machine that it matches
//! and that has no driver yet, this starts it with:
//!
//! - the capability for that device and no other — its configuration, its
//!   BARs, its claim, its interrupt by message, all through that one;
//! - the interrupt line the firmware wired the device to, where it has one
//!   (an IDE controller in compatibility mode: 14 and 15);
//! - what else its manifest asks for, as any spawner grants it;
//! - this program's standard output, and the device's address as its first
//!   argument.
//!
//! Drivers come from two places. `init` offers the ones in the boot image —
//! a disk's driver has to be running before there is a filesystem to read
//! one from — lending each with the call ([`devices::TAG_OFFER`]); and once
//! the root is up it says so ([`devices::TAG_FILES`]) and the rest are read
//! from `/usr/lib/drivers`. Both are taken from this program's parent and
//! nobody else: a program that could hand this a driver would be handed a
//! device.
//!
//! It is each driver's parent: it watches them, and one that ends is
//! collected. One that failed — a status that is not 0: a fault, a signal —
//! is started again for its device, from a copy of its image kept for the
//! purpose: a second later, and twice as long after each failure within a
//! minute of starting, up to a minute; the fifth such failure in a row and
//! the device is left without one. One that ended by itself has left its
//! device driverless. And it says what it found
//! to anybody who asks ([`devices::TAG_DEVICE`], [`devices::TAG_BAR`]),
//! which is all `lspci` is — and whether a program is one of its drivers
//! ([`devices::TAG_IS_DRIVER`]), which is how `input` knows to take keys
//! from a USB host controller's driver and from nobody else.

extern crate alloc;

use alloc::vec::Vec;
use quark_rt::devices::{self, Device as Entry, TAG_BAR, TAG_DEVICE, TAG_FILES, TAG_IS_DRIVER, TAG_OFFER};
use quark_rt::ipc::{death_notice, Message, TAG_PING, TID_ANY};
use quark_rt::manifest::{self, CapReq};
use quark_rt::pci::{self, Info};
use quark_rt::spawn::{self, Scratch};
use quark_rt::{nameserver, println, syscall, vfs};

// A driver's band, to start drivers in theirs; every device; any interrupt
// line, to give each driver its device's; and frames for devices without a
// limit, to give a driver what its manifest asks for.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::pci_devices(),
    CapReq::irq(0xFF),
    CapReq::phys_alloc(0),
]);

/// As many devices as the kernel keeps.
const MAX_DEVICES: usize = 128;
/// Where a driver's image is read into, and the most of it that is read.
const IMAGE_AT: usize = 0x89_0000_0000;
const MAX_IMAGE_PAGES: usize = 2048;
const SCRATCH: Scratch = Scratch { elf: 0x83_0000_0000, stack: 0x84_0000_0000, args: 0x88_0000_0000 };
/// Where a manifest's grants are minted on their way to a child, and the
/// device's and its line's.
const MANIFEST_SCRATCH: usize = 12;
const GRANT_SCRATCH: usize = syscall::SLOT_SCRATCH;
/// Where the right to call a driver is kept while it is asked whether it
/// is up.
const PING_SLOT: usize = syscall::SLOT_ENDPOINT_EXTRA;
/// A failure this soon after a start is one in a row.
const QUICK_NS: u64 = 60_000_000_000;
/// How many in a row leave a device without a driver.
const GIVE_UP: u32 = 5;
/// How long after the first of them a driver is started again, and the
/// longest.
const FIRST_DELAY_NS: u64 = 1_000_000_000;
const MAX_DELAY_NS: u64 = 60_000_000_000;

#[derive(Clone, Copy)]
struct Slot {
    info: Info,
    driver: usize,
    name: [u8; 16],
    /// The image its driver was started from, in [`images`].
    image: Option<usize>,
    /// When its driver was last started, how many times in a row it has
    /// failed soon after, and when it is to be started again (0: it is not).
    started: u64,
    quick: u32,
    due: u64,
}

static mut IMAGES: Vec<([u8; 16], Vec<u8>)> = Vec::new();

/// The images drivers were started from, by name: kept to start them again.
fn images() -> &'static mut Vec<([u8; 16], Vec<u8>)> {
    unsafe { &mut *core::ptr::addr_of_mut!(IMAGES) }
}

/// Where the image of the driver called `name` is kept: a copy of `image`
/// the first time.
fn keep(image: &[u8], name: &[u8; 16]) -> usize {
    let kept = images();
    if let Some(i) = kept.iter().position(|(n, _)| n == name) {
        return i;
    }
    kept.push((*name, image.to_vec()));
    kept.len() - 1
}

fn now() -> u64 {
    syscall::sys_clock()
}

static mut TABLE: [Option<Slot>; MAX_DEVICES] = [None; MAX_DEVICES];
static mut COUNT: usize = 0;

fn table() -> &'static mut [Option<Slot>] {
    unsafe {
        let all = &mut *core::ptr::addr_of_mut!(TABLE);
        &mut all[..*core::ptr::addr_of!(COUNT)]
    }
}

/// The interrupt lines a device is wired to: the two an IDE controller in
/// compatibility mode answers on, or the one the firmware wrote down.
fn lines(info: &Info) -> [Option<u8>; 2] {
    let h = &info.header;
    if h.class >> 8 == 0x0101 {
        let interface = h.class as u8;
        return [(interface & 1 == 0).then_some(14), (interface & 4 == 0).then_some(15)];
    }
    [(h.pin != 0 && h.line != 0 && h.line < 16).then_some(h.line), None]
}

/// Mint a capability and give it to `child`, in its first free slot.
fn give(child: usize, cap_type: u64, param0: u64) -> bool {
    let _ = syscall::sys_cap_delete(GRANT_SCRATCH);
    let given = syscall::sys_cap_mint(GRANT_SCRATCH, cap_type, param0, 0).is_ok()
        && syscall::sys_cap_grant_any(child, GRANT_SCRATCH).is_ok();
    let _ = syscall::sys_cap_delete(GRANT_SCRATCH);
    given
}

/// Start the driver in `image`, called `name`, for the device in `slot`.
fn start(image: &[u8], name: &[u8], slot: &mut Slot) -> bool {
    let Ok(child) = spawn::load(image, &SCRATCH) else {
        return false;
    };
    let address = slot.info.header.address();
    let _ = syscall::sys_cap_grant(child.tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT);
    manifest::grant_image(child.tid, image, MANIFEST_SCRATCH);
    let mut given = give(child.tid, syscall::CAP_TYPE_PCI_DEVICE, address.raw());
    for line in lines(&slot.info).into_iter().flatten() {
        given &= give(child.tid, syscall::CAP_TYPE_IRQ, line as u64);
    }
    let _ = syscall::sys_fd_dup(child.tid, 1, 1);
    let _ = syscall::sys_fd_dup(child.tid, 2, 2);
    let mut text = [0u8; 7];
    let told = spawn::set_args(&child, &[name, address.write(&mut text)], &SCRATCH).is_ok();
    if !given || !told || child.start().is_err() {
        child.discard();
        return false;
    }
    let _ = syscall::sys_task_watch(child.tid);
    slot.driver = child.tid;
    slot.name = [0; 16];
    let n = name.len().min(16);
    slot.name[..n].copy_from_slice(&name[..n]);
    slot.image = Some(keep(image, &slot.name));
    slot.started = now();
    slot.due = 0;
    let mut at = [0u8; 7];
    println!(
        "[devmgr] {} drives {} ({:04x}:{:04x}), task {}",
        core::str::from_utf8(name).unwrap_or("a driver"),
        core::str::from_utf8(address.write(&mut at)).unwrap_or("?"),
        slot.info.header.vendor,
        slot.info.header.device,
        child.tid
    );
    true
}

/// Start the driver in `image` for every device it drives that has none:
/// how many.
fn offer(image: &[u8], name: &[u8]) -> u64 {
    if !manifest::drives_any(image) {
        return 0;
    }
    let mut started = 0;
    for slot in table().iter_mut().flatten() {
        if slot.driver == 0 && manifest::drives(image, slot.info.header.key()) && start(image, name, slot) {
            started += 1;
        }
    }
    started
}

/// Map room for `bytes` of image: its pages, or none.
fn room(bytes: usize) -> Option<usize> {
    let pages = bytes.div_ceil(4096);
    (pages > 0 && pages <= MAX_IMAGE_PAGES && spawn::map_fresh(IMAGE_AT, pages).is_ok()).then_some(pages)
}

/// `init`'s offer: a driver from the boot image, lent with the call.
fn offered(msg: &Message) -> u64 {
    let length = msg.data[0] as usize;
    let Some(pages) = room(length) else { return 0 };
    let image = unsafe { core::slice::from_raw_parts_mut(IMAGE_AT as *mut u8, length) };
    let mut done = 0;
    while done < length {
        match syscall::sys_lent_read(msg.sender, done, &mut image[done..]) {
            Ok(n) if n > 0 => done += n,
            _ => break,
        }
    }
    let mut name = [0u8; 16];
    name[..8].copy_from_slice(&msg.data[1].to_le_bytes());
    name[8..].copy_from_slice(&msg.data[2].to_le_bytes());
    let len = name.iter().position(|&b| b == 0).unwrap_or(16);
    let started = if done == length { offer(image, &name[..len]) } else { 0 };
    spawn::release(IMAGE_AT, pages);
    started
}

/// A driver's name, from its file's: `EDU.ELF` on a FAT root is `edu`.
fn driver_name(file: &[u8], out: &mut [u8; 16]) -> usize {
    let stem = match file.len() {
        n if n > 4 && file[n - 4..].eq_ignore_ascii_case(b".elf") => &file[..n - 4],
        _ => file,
    };
    let n = stem.len().min(16);
    for (o, b) in out.iter_mut().zip(&stem[..n]) {
        *o = b.to_ascii_lowercase();
    }
    n
}

/// Wait, two seconds at most, for driver `tid` to be in its loop: to be
/// receiving, which is where a call to it is taken. What it printed as it
/// started is then on the console before whatever its starter does next —
/// a login prompt, which a driver's line after it pushed off its own.
fn settle(tid: usize) {
    let _ = syscall::sys_cap_delete(PING_SLOT);
    if syscall::sys_cap_mint(PING_SLOT, syscall::CAP_TYPE_ENDPOINT, tid as u64, 0).is_ok() {
        let ping = Message { sender: 0, tag: TAG_PING, data: [0; 6] };
        let mut reply = Message::empty();
        let _ = syscall::sys_call_timeout(tid, &ping, &mut reply, 200);
    }
    let _ = syscall::sys_cap_delete(PING_SLOT);
}

/// The root is up: every driver in `/usr/lib/drivers`, for whatever it
/// drives that has no driver yet — and every driver up, the boot image's
/// too, before this answers. A USB controller's driver says it is up once
/// what was plugged in as the machine started has been seen to.
fn from_files() -> u64 {
    let started = read_files();
    for slot in table().iter().flatten().filter(|s| s.driver != 0) {
        settle(slot.driver);
    }
    started
}

/// Start every driver in `/usr/lib/drivers` for whatever it drives that has
/// no driver yet: how many.
fn read_files() -> u64 {
    let Some(vfs_tid) = nameserver::lookup(b"vfs") else { return 0 };
    let Ok((dir, _, true)) = vfs::open(vfs_tid, devices::DRIVERS) else { return 0 };
    let mut started = 0;
    let mut index = 0;
    while let Ok(Some(entry)) = vfs::readdir(vfs_tid, dir, index) {
        index += 1;
        let file = entry.name_bytes();
        if entry.is_dir || file.is_empty() || file[0] == b'.' {
            continue;
        }
        let mut path = [0u8; 96];
        let prefix = devices::DRIVERS.len();
        if prefix + 1 + file.len() > path.len() {
            continue;
        }
        path[..prefix].copy_from_slice(devices::DRIVERS);
        path[prefix] = b'/';
        path[prefix + 1..prefix + 1 + file.len()].copy_from_slice(file);
        let path = &path[..prefix + 1 + file.len()];
        let Ok((handle, size, false)) = vfs::open(vfs_tid, path) else { continue };
        let size = size as usize;
        if let Some(pages) = room(size) {
            let image = unsafe { core::slice::from_raw_parts_mut(IMAGE_AT as *mut u8, size) };
            let whole = image.chunks_mut(4096).enumerate().all(|(p, page)| {
                vfs::read(vfs_tid, handle, page, (p * 4096) as u32) == Ok(page.len() as u32)
            });
            let mut name = [0u8; 16];
            let n = driver_name(file, &mut name);
            if whole {
                started += offer(image, &name[..n]);
            }
            spawn::release(IMAGE_AT, pages);
        }
        let _ = vfs::close(vfs_tid, handle);
    }
    let _ = vfs::close(vfs_tid, dir);
    started
}

fn answer(msg: &Message, parent: usize) -> Message {
    let ok = |data: [u64; 6]| Message { sender: 0, tag: 0, data };
    let no = Message { sender: 0, tag: u64::MAX, data: [0; 6] };
    match msg.tag {
        TAG_OFFER if msg.sender == parent => ok([offered(msg), 0, 0, 0, 0, 0]),
        TAG_FILES if msg.sender == parent => ok([from_files(), 0, 0, 0, 0, 0]),
        TAG_DEVICE => match table().get(msg.data[0] as usize).copied().flatten() {
            Some(slot) => {
                let len = slot.name.iter().position(|&b| b == 0).unwrap_or(16);
                ok(Entry::new(slot.info.header, slot.driver, &slot.name[..len]).words())
            }
            None => no,
        },
        TAG_BAR => {
            let found = table().iter().flatten().find(|s| s.info.header.address as u64 == msg.data[0]);
            match found.and_then(|s| s.info.bars.get(msg.data[1] as usize).copied()) {
                Some(bar) => ok([bar.base, bar.size, bar.flags, 0, 0, 0]),
                None => no,
            }
        }
        TAG_IS_DRIVER => {
            // By program, which a number that is never used again names:
            // asked about a thread of a driver, or a driver that has gone.
            let space = msg.data[0];
            let driver = space != 0
                && table()
                    .iter()
                    .flatten()
                    .any(|s| s.driver != 0 && syscall::sys_task_space(s.driver) == Ok(space));
            ok([driver as u64, 0, 0, 0, 0, 0])
        }
        TAG_PING => Message::empty(),
        _ => no,
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let me = syscall::sys_getpid() as usize;
    let parent = syscall::sys_task_info(me).map_or(0, |(_, parent, _)| parent);
    let mut count = 0;
    for info in pci::devices() {
        if count == MAX_DEVICES {
            break;
        }
        unsafe {
            (*core::ptr::addr_of_mut!(TABLE))[count] =
                Some(Slot { info, driver: 0, name: [0; 16], image: None, started: 0, quick: 0, due: 0 })
        };
        count += 1;
    }
    unsafe { *core::ptr::addr_of_mut!(COUNT) = count };
    // Holding every device is what this program is: without it there is
    // nothing to say and nothing to start.
    if count == 0 {
        println!("[devmgr] this was not started holding the machine's devices.");
        syscall::sys_exit_code(1);
    }
    println!("[devmgr] {} devices", count);
    if nameserver::register(devices::NAME).is_err() {
        println!("[devmgr] could not register.");
    }

    loop {
        again();
        let t = now();
        let wait = table().iter().flatten().filter(|s| s.due != 0).map(|s| s.due.saturating_sub(t)).min();
        let mut msg = Message::empty();
        let heard = match wait {
            Some(ns) => syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(ns.max(1))).is_ok(),
            None => syscall::sys_recv(TID_ANY, &mut msg).is_ok(),
        };
        if !heard {
            continue;
        }
        if let Some(gone) = death_notice(&msg) {
            ended(gone);
            continue;
        }
        if msg.sender == 0 {
            continue;
        }
        let reply = answer(&msg, parent);
        let _ = syscall::sys_reply(msg.sender, &reply);
    }
}

/// Driver `gone` ended: collected, and started again if it failed.
fn ended(gone: usize) {
    let status = syscall::sys_wait_for(gone).map_or(0, |(_, status)| status);
    let t = now();
    for slot in table().iter_mut().flatten().filter(|s| s.driver == gone) {
        slot.driver = 0;
        let name = slot.name;
        let shown = core::str::from_utf8(&name[..name.iter().position(|&b| b == 0).unwrap_or(16)]).unwrap_or("a driver");
        if status == 0 || slot.image.is_none() {
            slot.name = [0; 16];
            continue;
        }
        slot.quick = if t.saturating_sub(slot.started) < QUICK_NS { slot.quick + 1 } else { 1 };
        if slot.quick >= GIVE_UP {
            println!(
                "[devmgr] {} ended with status {}, the fifth time in a row within a minute of starting; its device is left without a driver",
                shown, status
            );
            continue;
        }
        let delay = (FIRST_DELAY_NS << (slot.quick - 1)).min(MAX_DELAY_NS);
        slot.due = t + delay;
        println!("[devmgr] {} ended with status {}; starting it again in {} s", shown, status, delay / 1_000_000_000);
    }
}

/// Start again every driver that is due.
fn again() {
    let t = now();
    for slot in table().iter_mut().flatten().filter(|s| s.due != 0 && s.due <= t && s.driver == 0) {
        slot.due = 0;
        let Some(i) = slot.image else { continue };
        // A copy: what is kept is not to be read while it is added to.
        let (name, image) = images()[i].clone();
        let len = name.iter().position(|&b| b == 0).unwrap_or(16);
        if !start(&image, &name[..len], slot) {
            println!("[devmgr] {} would not start again", core::str::from_utf8(&name[..len]).unwrap_or("a driver"));
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[devmgr] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
