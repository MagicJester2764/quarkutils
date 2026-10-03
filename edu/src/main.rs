#![no_std]
#![no_main]

//! A driver for a device that interrupts by sending a message.
//!
//! The device is QEMU's `edu`, which exists to be written a driver for: a
//! megabyte of registers, of which one raises an interrupt when it is
//! written to and one takes the interrupt away. It is on no real machine.
//! It is here because it is the smallest device there is that does the
//! three things every device newer than the ISA bus does and nothing in
//! this system did before it:
//!
//! - **Its registers are memory, at an address the firmware chose.** A
//!   driver asks, in its manifest, for the right to map where devices are
//!   (`DeviceMemory`, which `init` holds from the kernel); reads the
//!   address out of the device's configuration; mints a range of physical
//!   memory for exactly that; and maps it.
//! - **It has no interrupt line of its own.** On a PC it would share one of
//!   four with whatever else is plugged in. Asked to, it sends its
//!   interrupt as a message to a processor instead (MSI), and the kernel
//!   gives the driver a number that is only this device's
//!   (`SYS_MSI_ALLOC`).
//! - **Nothing in the tree knows it is there.** A distribution starts it
//!   with a `start` line in `/etc/init.conf`; on a machine with no such
//!   device it says so and ends.
//! - **It copies memory itself (DMA)**, by physical address. The device is
//!   this program's (`SYS_DEVICE_CLAIM`), and on a machine with an IOMMU it
//!   reaches the two pages this asked the kernel for and nothing else.
//!
//! A driver for a real device of this kind is this file with different
//! registers.
//!
//! It serves two requests, to whoever looks up `edu` — which is how
//! `dtest msi` holds the kernel to all of the above:
//!
//! | Tag | Asks | Answer in `data` |
//! |---|---|---|
//! | 1 | who are you | `[identity register, 1 if the device answers a check, interrupt number, 1 by message / 0 by line]` |
//! | 2 | raise an interrupt, with `data[0]` | `[1 if it arrived, what the device said it was for, interrupt number]` |
//! | 3 | may you map `data[0]` up to `data[1]` | `[1 if the kernel gave a range for it]` |
//! | 4 | copy `data[2]` bytes from `data[0]` to `data[1]` through the device: physical addresses, 0 for this driver's own source and destination | `[1 if both copies finished, the first eight bytes of its own destination, its own source, its own destination, 1 if the device reaches only this driver's memory, 1 if the device can address its pages]` |
//! | 5 | how often has the device reached for what it may not | `[the count]` |
//! | 6 | give your destination page back, have the device copy to where it was, and take a page again | `[1 if the copy finished, the count before, the count after, 1 if a page was had again]` |
//! | 7 | claim the first device that is vendor `data[0]`'s device `data[1]` | `[0 or 1 claimed (as the call answers), 2 refused, 3 no such device]` |
//!
//! The third is the other half of the first: what this driver is given
//! reaches where devices are and nowhere else, and saying so takes somebody
//! who holds it to ask.

use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{nameserver, println, syscall};

// A driver's band; the two ports every PCI device is configured through;
// an interrupt, whichever it turns out to be; and where devices are.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::ioport(0xCF8, 0xCFF),
    CapReq::irq(0xFF),
    CapReq::device_memory(),
    CapReq::phys_alloc(2),
]);

const TAG_IDENTIFY: u64 = 1;
const TAG_RAISE: u64 = 2;
const TAG_MAY_MAP: u64 = 3;
const TAG_COPY: u64 = 4;
const TAG_STOPPED: u64 = 5;
const TAG_GIVE_BACK: u64 = 6;
const TAG_CLAIM: u64 = 7;

const VENDOR: u16 = 0x1234;
const DEVICE: u16 = 0x11E8;

const PCI_ADDRESS: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const PCI_COMMAND: u8 = 0x04;
const PCI_BAR0: u8 = 0x10;
const PCI_CAPABILITIES: u8 = 0x34;
const PCI_INTERRUPT_LINE: u8 = 0x3C;
const COMMAND_MEMORY: u32 = 1 << 1;
const COMMAND_MASTER: u32 = 1 << 2;
const COMMAND_NO_LINE: u32 = 1 << 10;
const STATUS_HAS_CAPABILITIES: u32 = 1 << (16 + 4);
const CAPABILITY_MSI: u8 = 0x05;
const MSI_ENABLE: u32 = 1 << 16;
const MSI_64BIT: u32 = 1 << (16 + 7);

/// Where the device's registers are mapped, and the slot the capability
/// for them is kept in: above what a manifest's grants fill.
const REGISTERS: usize = 0xA2_0000_0000;
const REGISTERS_SLOT: usize = 11;
/// A slot to try a range in, and give it straight back.
const TRIAL_SLOT: usize = 10;
const REG_IDENTITY: usize = 0x00;
const REG_CHECK: usize = 0x04;
const REG_INTERRUPT_STATUS: usize = 0x24;
const REG_INTERRUPT_RAISE: usize = 0x60;
const REG_INTERRUPT_ACK: usize = 0x64;
const REG_DMA_SOURCE: usize = 0x80;
const REG_DMA_DESTINATION: usize = 0x88;
const REG_DMA_COUNT: usize = 0x90;
const REG_DMA_COMMAND: usize = 0x98;
const DMA_RUN: u64 = 1;
const DMA_TO_MEMORY: u64 = 1 << 1;
/// Where the device keeps what it copies, in its own addresses.
const DEVICE_BUFFER: u64 = 0x4_0000;
/// How far the device can address memory: twenty-eight bits. An address
/// past that it cuts short and copies somewhere else.
const DEVICE_REACH: u64 = 1 << 28;
/// This driver's two pages for the device to copy between, and what is in
/// the first.
const OWN_SOURCE: usize = REGISTERS + 0x1000;
const OWN_DESTINATION: usize = REGISTERS + 0x2000;
const OWN_PATTERN: &[u8; 8] = b"edu-own.";

fn config_read(at: (u8, u8), offset: u8) -> u32 {
    let address = 0x8000_0000u32 | (at.0 as u32) << 16 | (at.1 as u32) << 11 | (offset as u32 & 0xFC);
    syscall::sys_ioport_write32(PCI_ADDRESS, address);
    syscall::sys_ioport_read32(PCI_DATA)
}

fn config_write(at: (u8, u8), offset: u8, value: u32) {
    let address = 0x8000_0000u32 | (at.0 as u32) << 16 | (at.1 as u32) << 11 | (offset as u32 & 0xFC);
    syscall::sys_ioport_write32(PCI_ADDRESS, address);
    syscall::sys_ioport_write32(PCI_DATA, value);
}

/// The bus and slot the device is in, if the machine has one.
fn find() -> Option<(u8, u8)> {
    find_device(VENDOR, DEVICE)
}

/// The bus and slot of the first device that is `vendor`'s `device`.
fn find_device(vendor: u16, device: u16) -> Option<(u8, u8)> {
    for bus in 0..8u8 {
        for slot in 0..32u8 {
            let id = config_read((bus, slot), 0);
            if id as u16 == vendor && (id >> 16) as u16 == device {
                return Some((bus, slot));
            }
        }
    }
    None
}

fn register(reg: usize) -> u32 {
    unsafe { core::ptr::read_volatile((REGISTERS + reg) as *const u32) }
}

fn set_register(reg: usize, value: u32) {
    unsafe { core::ptr::write_volatile((REGISTERS + reg) as *mut u32, value) };
}

fn register64(reg: usize) -> u64 {
    unsafe { core::ptr::read_volatile((REGISTERS + reg) as *const u64) }
}

fn set_register64(reg: usize, value: u64) {
    unsafe { core::ptr::write_volatile((REGISTERS + reg) as *mut u64, value) };
}

/// Have the device copy `count` bytes from `from` to `to`, into memory if
/// `to_memory` and into itself if not; and wait for it, two seconds at
/// most. Whether it finished.
fn dma(from: u64, to: u64, count: u64, to_memory: bool) -> bool {
    set_register64(REG_DMA_SOURCE, from);
    set_register64(REG_DMA_DESTINATION, to);
    set_register64(REG_DMA_COUNT, count);
    set_register64(REG_DMA_COMMAND, DMA_RUN | if to_memory { DMA_TO_MEMORY } else { 0 });
    for _ in 0..200 {
        if register64(REG_DMA_COMMAND) & DMA_RUN == 0 {
            return true;
        }
        syscall::sleep_ticks(1);
    }
    false
}

/// A page of memory for the device, mapped at `at`: its physical address.
fn page_for_device(at: usize) -> Option<u64> {
    let frame = syscall::sys_phys_alloc_low(1).ok()?;
    syscall::sys_map_phys(frame, at, 1).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, 4096) };
    Some(frame as u64)
}

/// Where the device's MSI capability is in its configuration, if it has
/// one: a list, each entry naming its kind and the next.
fn msi_capability(at: (u8, u8)) -> Option<u8> {
    if config_read(at, PCI_COMMAND) & STATUS_HAS_CAPABILITIES == 0 {
        return None;
    }
    let mut offset = (config_read(at, PCI_CAPABILITIES) & 0xFC) as u8;
    // A list in a device's own memory is not trusted to end.
    for _ in 0..48 {
        if offset < 0x40 {
            return None;
        }
        let entry = config_read(at, offset);
        if entry as u8 == CAPABILITY_MSI {
            return Some(offset);
        }
        offset = ((entry >> 8) & 0xFC) as u8;
    }
    None
}

/// Have the device send its interrupts as messages: ask the kernel for an
/// interrupt of this driver's own, and tell the device where to send and
/// what.
fn by_message(at: (u8, u8)) -> Option<u8> {
    let capability = msi_capability(at)?;
    let msi = syscall::sys_msi_alloc().ok()?;
    let control = config_read(at, capability);
    config_write(at, capability + 4, msi.address);
    if control & MSI_64BIT != 0 {
        config_write(at, capability + 8, 0);
        config_write(at, capability + 12, msi.data as u32);
    } else {
        config_write(at, capability + 8, msi.data as u32);
    }
    // One message, enabled; and its line, which it would otherwise go on
    // raising as well, turned off.
    config_write(at, capability, (control & !(0x7 << (16 + 4))) | MSI_ENABLE);
    let command = config_read(at, PCI_COMMAND) & 0xFFFF;
    config_write(at, PCI_COMMAND, command | COMMAND_NO_LINE);
    Some(msi.irq)
}

/// Or by the line it was wired to, as a device on the ISA bus would.
fn by_line(at: (u8, u8)) -> Option<u8> {
    let line = config_read(at, PCI_INTERRUPT_LINE) as u8;
    (line < 16 && syscall::sys_irq_register(line).is_ok()).then_some(line)
}

/// Raise an interrupt for `value` and wait for it: whether it arrived, and
/// what the device said it was for.
fn raise(irq: u8, value: u32) -> (bool, u32) {
    set_register(REG_INTERRUPT_RAISE, value);
    let mut told = Message::empty();
    let mut arrived = false;
    // Only the kernel is listened to: a client that calls meanwhile waits.
    for _ in 0..4 {
        if syscall::sys_recv_timeout(0, &mut told, 50).is_err() {
            break;
        }
        if told.sender == 0 && told.tag == irq as u64 {
            arrived = true;
            break;
        }
    }
    // Quieten the device, and then say so: in that order, always.
    let status = register(REG_INTERRUPT_STATUS);
    set_register(REG_INTERRUPT_ACK, status);
    syscall::sys_irq_ack(irq);
    (arrived, status)
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(at) = find() else {
        println!("[edu] no such device on this machine.");
        syscall::sys_exit_code(0);
    };
    let bar = config_read(at, PCI_BAR0);
    // Memory, and below four gigabytes: bit 0 says ports, bits 1 and 2 how
    // wide the address is.
    if bar & 0x7 != 0 || bar & !0xFFF == 0 {
        println!("[edu] the device's registers are not where this can map them.");
        syscall::sys_exit_code(1);
    }
    // The device is this program's, and copies to and from what it is given.
    let device = syscall::pci_device(at.0, at.1, 0);
    let guarded = match syscall::sys_device_claim(device) {
        Ok(guarded) => guarded,
        Err(_) => {
            println!("[edu] the device is another program's.");
            syscall::sys_exit_code(1);
        }
    };
    let command = config_read(at, PCI_COMMAND) & 0xFFFF;
    config_write(at, PCI_COMMAND, command | COMMAND_MEMORY | COMMAND_MASTER);
    // The registers this uses are in the first page of the megabyte: a
    // range for that page and no more, from the right to device memory.
    let page = (bar & !0xFFF) as u64;
    if syscall::sys_cap_mint(REGISTERS_SLOT, syscall::CAP_TYPE_PHYS_RANGE, page, page + 0x1000).is_err()
        || syscall::sys_map_phys(page as usize, REGISTERS, 1).is_err()
    {
        println!("[edu] may not map the device's registers: this was not started with the right to device memory.");
        syscall::sys_exit_code(1);
    }

    let (Some(own_source), Some(mut own_destination)) = (page_for_device(OWN_SOURCE), page_for_device(OWN_DESTINATION))
    else {
        println!("[edu] no memory for the device to copy.");
        syscall::sys_exit_code(1);
    };
    unsafe { core::ptr::copy_nonoverlapping(OWN_PATTERN.as_ptr(), OWN_SOURCE as *mut u8, 8) };
    let reachable = own_source < DEVICE_REACH && own_destination < DEVICE_REACH;

    let (irq, message) = match by_message(at) {
        Some(irq) => (irq, true),
        None => match by_line(at) {
            Some(irq) => (irq, false),
            None => {
                println!("[edu] no interrupt to be had for the device.");
                syscall::sys_exit_code(1);
            }
        },
    };
    println!(
        "[edu] registers at {:#x}, interrupt {} ({}).",
        bar & !0xFFF,
        irq,
        if message { "a message of its own" } else { "its line" }
    );
    if nameserver::register(b"edu").is_err() {
        println!("[edu] could not register.");
        syscall::sys_exit_code(1);
    }

    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() || msg.sender == 0 {
            continue;
        }
        let reply = match msg.tag {
            TAG_IDENTIFY => {
                set_register(REG_CHECK, 0x1234_5678);
                let answers = register(REG_CHECK) == !0x1234_5678u32;
                Message {
                    sender: 0,
                    tag: 0,
                    data: [register(REG_IDENTITY) as u64, answers as u64, irq as u64, message as u64, 0, 0],
                }
            }
            TAG_RAISE => {
                let (arrived, status) = raise(irq, msg.data[0] as u32);
                Message { sender: 0, tag: 0, data: [arrived as u64, status as u64, irq as u64, 0, 0, 0] }
            }
            TAG_MAY_MAP => {
                let given =
                    syscall::sys_cap_mint(TRIAL_SLOT, syscall::CAP_TYPE_PHYS_RANGE, msg.data[0], msg.data[1]).is_ok();
                let _ = syscall::sys_cap_delete(TRIAL_SLOT);
                Message { sender: 0, tag: 0, data: [given as u64, 0, 0, 0, 0, 0] }
            }
            TAG_COPY => {
                let mine = |at: u64, own: u64| if at == 0 { own } else { at };
                let (from, to) = (mine(msg.data[0], own_source), mine(msg.data[1], own_destination));
                let count = msg.data[2].clamp(1, 4096);
                unsafe { core::ptr::write_bytes(OWN_DESTINATION as *mut u8, 0, 4096) };
                let done = dma(from, DEVICE_BUFFER, count, false) && dma(DEVICE_BUFFER, to, count, true);
                let first = unsafe { core::ptr::read_volatile(OWN_DESTINATION as *const u64) };
                Message {
                    sender: 0,
                    tag: 0,
                    data: [done as u64, first, own_source, own_destination, guarded as u64, reachable as u64],
                }
            }
            TAG_STOPPED => {
                let stopped = syscall::sys_device_stopped(device).unwrap_or(u64::MAX);
                Message { sender: 0, tag: 0, data: [stopped, 0, 0, 0, 0, 0] }
            }
            TAG_GIVE_BACK => {
                // What the unit stopped is counted on the tick: a tick to
                // count, either side of the copy.
                let count = || {
                    syscall::sleep_ticks(3);
                    syscall::sys_device_stopped(device).unwrap_or(u64::MAX)
                };
                let given_back = own_destination;
                let _ = syscall::sys_munmap(OWN_DESTINATION, 1);
                let _ = syscall::sys_phys_free(given_back as usize, 1);
                let before = count();
                let done = dma(own_source, DEVICE_BUFFER, 8, false) && dma(DEVICE_BUFFER, given_back, 8, true);
                let after = count();
                let again = page_for_device(OWN_DESTINATION);
                if let Some(page) = again {
                    own_destination = page;
                }
                Message { sender: 0, tag: 0, data: [done as u64, before, after, again.is_some() as u64, 0, 0] }
            }
            TAG_CLAIM => {
                let answer = match find_device(msg.data[0] as u16, msg.data[1] as u16) {
                    None => 3,
                    Some((bus, slot)) => match syscall::sys_device_claim(syscall::pci_device(bus, slot, 0)) {
                        Ok(guarded) => guarded as u64,
                        Err(_) => 2,
                    },
                };
                Message { sender: 0, tag: 0, data: [answer, 0, 0, 0, 0, 0] }
            }
            quark_rt::ipc::TAG_PING => Message::empty(),
            _ => Message { sender: 0, tag: u64::MAX, data: [0; 6] },
        };
        let _ = syscall::sys_reply(msg.sender, &reply);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[edu] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
