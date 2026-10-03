#![no_std]
#![no_main]

//! A USB host controller (xHCI), and what is plugged into it: hubs,
//! keyboards and mice, and disks.
//!
//! Started by the device manager for an xHCI controller, holding it. The
//! controller is this program's first thread's, which takes it from the
//! firmware, resets it and starts it, gives each device plugged in an
//! address and configures what of it is driven here: a keyboard's and a
//! mouse's reports — the boot protocol's, which is what a BIOS reads —
//! become keys and movement for `input`; a hub's ports are driven as the
//! controller's own are; a disk (bulk-only, SCSI) is a thread of this
//! program serving it as `diskN`. A second thread answers `input` and
//! anybody asking what is plugged in (`quark_rt::usb`), so that neither
//! waits on the controller. Everything the controller reaches is this
//! program's own memory.

mod dev;
mod disk;
mod hc;
mod mem;
mod service;
mod shared;
mod trb;

use quark_rt::manifest::CapReq;
use quark_rt::{pci, println, syscall, thread};

// A driver's band; an xHCI controller; frames for its rings, its devices'
// contexts and its disks' sectors.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives_interface(0x0C, 0x03, 0x30),
    CapReq::phys_alloc(mem::MAX_PAGES as u64),
]);

/// Where the controller's registers (BAR 0) are mapped, its MSI-X table's
/// BAR when that is another, and the slots their ranges are minted in.
const REGS_AT: usize = 0xA0_0000_0000;
const TABLE_AT: usize = 0xA1_0000_0000;
const REGS_SLOT: usize = 10;
const TABLE_SLOT: usize = 11;
/// Where the capability to notify this thread is kept, for the disks'
/// threads to tell it of requests.
const SELF_SLOT: usize = 40;
/// How long links are given to come up before what is plugged in is said
/// to have been seen to.
const SETTLE_NS: u64 = 500_000_000;

fn stop(why: &str) -> ! {
    println!("[usb] {}", why);
    syscall::sys_exit_program(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(device) = pci::this_device().filter(|&d| pci::info(d).is_some()) else {
        stop("started without a controller: the device manager starts this, for an xHCI controller.");
    };
    let Some(regs) = pci::map_bar(device, 0, REGS_AT, REGS_SLOT) else {
        stop("may not map the controller's registers");
    };
    if pci::claim(device).is_err() {
        stop("the controller is another program's");
    }
    if pci::enable(device, pci::COMMAND_MEMORY | pci::COMMAND_MASTER).is_err() {
        stop("the controller may not be turned on");
    }
    let map = |bar: usize| if bar == 0 { Some(regs) } else { pci::map_bar(device, bar, TABLE_AT, TABLE_SLOT) };
    let hc = match hc::Hc::start(device, regs, map) {
        Ok(hc) => hc,
        Err(why) => stop(why),
    };
    println!(
        "[usb] xHCI controller: {} ports, interrupt {} ({})",
        hc.ports,
        hc.interrupt.number(),
        hc.interrupt.describe()
    );
    let me = syscall::sys_getpid() as usize;
    shared::SHARED.lock().main = me;
    let _ = syscall::sys_cap_delete(SELF_SLOT);
    if syscall::sys_cap_mint(SELF_SLOT, syscall::CAP_TYPE_ENDPOINT, me as u64, 0).is_err() {
        println!("[usb] disks' threads will not be able to ask anything of this one");
    }
    if thread::spawn_with_stack(service::serve, 8).is_err() {
        stop("no thread to answer with");
    }
    let mut usb = dev::Usb::new(hc);
    usb.scan();
    // What is plugged in as the machine starts is seen to before this says
    // it is up: half a second for links to come up, and then until nothing
    // is left to do. Asked by the device manager before a session starts,
    // and a keyboard found after the login prompt was a prompt with the
    // keyboard's line printed after it.
    let started = syscall::sys_clock();
    let mut settled = false;
    loop {
        usb.see_to();
        if !settled && !usb.busy() && syscall::sys_clock().wrapping_sub(started) >= SETTLE_NS {
            settled = true;
        }
        if settled {
            usb.hc.answer_pings();
        }
        if !usb.busy() {
            let wait = if settled { usb.next_wait() } else { usb.next_wait().min(50_000_000) };
            usb.hc.wait(wait);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[usb] PANIC: {}", info);
    syscall::sys_exit_program(1);
}
