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
]);

const TAG_IDENTIFY: u64 = 1;
const TAG_RAISE: u64 = 2;
const TAG_MAY_MAP: u64 = 3;

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
    for bus in 0..8u8 {
        for slot in 0..32u8 {
            let id = config_read((bus, slot), 0);
            if id as u16 == VENDOR && (id >> 16) as u16 == DEVICE {
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
