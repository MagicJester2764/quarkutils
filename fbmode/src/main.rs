#![no_std]
#![no_main]

//! `fbmode [WIDTH HEIGHT]`: what size the display is, or have it another.
//!
//! Only a display whose driver can change it has other sizes — a virtio
//! GPU's; the bootloader's framebuffer is one size for as long as the
//! machine is up. The display is claimed, the size asked for, and let go
//! once `fb` has handed it back in that size, so whoever had it before has
//! it again, in the new size (`quark_rt::display::set_mode`).

use quark_rt::{args, display, println, syscall};

fn number(text: &[u8]) -> Option<u64> {
    if text.is_empty() || text.len() > 5 {
        return None;
    }
    text.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let usage = || -> ! {
        println!("usage: fbmode [WIDTH HEIGHT]");
        syscall::sys_exit_code(2);
    };
    let mode = match (args::argv(1), args::argv(2), args::argv(3)) {
        (None, _, _) => display::mode().ok_or("no framebuffer device"),
        (Some(w), Some(h), None) => match (number(w), number(h)) {
            (Some(w), Some(h)) if w > 0 && h > 0 => display::set_mode(w, h),
            _ => usage(),
        },
        _ => usage(),
    };
    match mode {
        Ok(m) if m.phys == 0 => {
            println!("fbmode: there is no display yet");
            syscall::sys_exit_code(1);
        }
        Ok(m) => {
            println!("{}x{}, {} bits a pixel", m.width, m.height, m.bpp);
            syscall::sys_exit_code(0);
        }
        Err(why) => {
            println!("fbmode: {}", why);
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("fbmode: {}", info);
    syscall::sys_exit_code(1);
}
