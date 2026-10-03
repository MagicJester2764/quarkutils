#![no_std]
#![no_main]

//! `lspci [-v]`: what is in the machine, and which program drives it.
//!
//! The kernel found every PCI device at boot, and the device manager holds
//! them all and starts their drivers; this asks it (`quark_rt::devices`)
//! and says what it was told. It reaches no device itself. With `-v`, each
//! device's interrupt, its BARs and its driver, a line each.

use quark_rt::devices;
use quark_rt::{args, nameserver, print, println, syscall};

/// A length as `lspci` writes one: 256, 4K, 1M, 16G.
fn size(bytes: u64, out: &mut [u8; 24]) -> &str {
    let (n, unit) = match bytes {
        b if b >= 1 << 30 && b % (1 << 30) == 0 => (b >> 30, "G"),
        b if b >= 1 << 20 && b % (1 << 20) == 0 => (b >> 20, "M"),
        b if b >= 1 << 10 && b % (1 << 10) == 0 => (b >> 10, "K"),
        b => (b, ""),
    };
    let mut digits = [0u8; 20];
    let mut len = 0;
    let mut v = n;
    loop {
        digits[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..len {
        out[i] = digits[len - 1 - i];
    }
    out[len..len + unit.len()].copy_from_slice(unit.as_bytes());
    core::str::from_utf8(&out[..len + unit.len()]).unwrap_or("?")
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let verbose = match args::argv(1) {
        None => false,
        Some(b"-v") => true,
        Some(_) => {
            println!("usage: lspci [-v]");
            syscall::sys_exit_code(2);
        }
    };
    if args::argv(2).is_some() {
        println!("usage: lspci [-v]");
        syscall::sys_exit_code(2);
    }
    let Some(manager) = nameserver::lookup(devices::NAME) else {
        println!("lspci: no device manager is running");
        syscall::sys_exit_code(1);
    };
    let mut index = 0;
    while let Some(device) = devices::entry(manager, index) {
        index += 1;
        let h = device.header;
        let mut at = [0u8; 7];
        print!(
            "{} {} [{:04x}]: {:04x}:{:04x}",
            core::str::from_utf8(h.address().write(&mut at)).unwrap_or("?"),
            h.kind(),
            h.class >> 8,
            h.vendor,
            h.device
        );
        if h.revision != 0 {
            print!(" (rev {:02x})", h.revision);
        }
        if h.class & 0xFF != 0 {
            print!(" (prog-if {:02x})", h.class & 0xFF);
        }
        let driver = core::str::from_utf8(device.driver_name()).unwrap_or("?");
        if !verbose {
            if device.driver != 0 {
                print!(" [{}]", driver);
            }
            println!();
            continue;
        }
        println!();
        if h.subsystem_vendor != 0 || h.subsystem != 0 {
            println!("\tSubsystem: {:04x}:{:04x}", h.subsystem_vendor, h.subsystem);
        }
        if h.pin != 0 {
            println!("\tInterrupt: pin {} routed to IRQ {}", (b'A' + h.pin - 1) as char, h.line);
        }
        for n in 0..6 {
            let Some(bar) = devices::bar(manager, h.address(), n) else { break };
            if !bar.present() {
                continue;
            }
            let mut text = [0u8; 24];
            if bar.is_ports() {
                println!("\tI/O ports at {:x} [size={}]", bar.base, size(bar.size, &mut text));
            } else {
                println!(
                    "\tMemory at {:x} ({}-bit, {}prefetchable) [size={}]",
                    bar.base,
                    if bar.flags & quark_rt::pci::BAR_WIDE != 0 { 64 } else { 32 },
                    if bar.flags & quark_rt::pci::BAR_PREFETCH != 0 { "" } else { "non-" },
                    size(bar.size, &mut text)
                );
            }
        }
        if h.msi != 0 {
            println!("\tCapabilities: [{:02x}] MSI", h.msi);
        }
        if h.msix != 0 {
            println!("\tCapabilities: [{:02x}] MSI-X", h.msix);
        }
        if device.driver != 0 {
            println!("\tDriver: {} (task {})", driver, device.driver);
        }
    }
    if index == 0 {
        println!("lspci: the device manager knows of no device");
        syscall::sys_exit_code(1);
    }
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("lspci: {}", info);
    syscall::sys_exit_code(1);
}
