#![no_std]
#![no_main]

//! `lsusb`: what is plugged in, on every USB controller's driver.
//!
//! Each controller's driver registers as `usb0`, `usb1` and so on and says
//! what it drives (`quark_rt::usb`); this asks each and says what it was
//! told — where a device is, its ids, its name, how fast it runs and what
//! it is to the driver. It reaches no device itself.

use quark_rt::usb;
use quark_rt::{args, print, println, syscall};

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    if args::argv(1).is_some() {
        println!("usage: lsusb");
        syscall::sys_exit_code(2);
    }
    let mut controllers = 0;
    for (name, server) in usb::controllers() {
        controllers += 1;
        let name = core::str::from_utf8(&name).unwrap_or("usb");
        let mut index = 0;
        while let Some(device) = usb::device(server, index) {
            index += 1;
            print!("{} port {}", name, device.port);
            // The way through hubs, nearest first.
            let mut route = device.route;
            for _ in 0..device.depth {
                print!(".{}", route & 0xF);
                route >>= 4;
            }
            print!(
                ": ID {:04x}:{:04x} {} ({})",
                device.vendor,
                device.product,
                core::str::from_utf8(device.name()).unwrap_or("?"),
                device.speed_name()
            );
            let roles = [
                (usb::ROLE_KEYBOARD, "keyboard"),
                (usb::ROLE_MOUSE, "mouse"),
                (usb::ROLE_HUB, "hub"),
                (usb::ROLE_DISK, "disk"),
            ];
            let mut first = true;
            for (bit, what) in roles {
                if device.roles & bit != 0 {
                    print!("{}{}", if first { " [" } else { ", " }, what);
                    first = false;
                }
            }
            println!("{}", if first { "" } else { "]" });
        }
        if index == 0 {
            println!("{}: nothing plugged in", name);
        }
    }
    if controllers == 0 {
        println!("lsusb: no USB controller is driven");
        syscall::sys_exit_code(1);
    }
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("lsusb: {}", info);
    syscall::sys_exit_code(1);
}
