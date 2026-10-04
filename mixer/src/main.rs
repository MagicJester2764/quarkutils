#![no_std]
#![no_main]

//! `mixer [VOLUME]`: the volume of everything the sound server plays, from
//! 0 to 100, or set to VOLUME.

use quark_rt::{args, println, sound, syscall};

fn number(text: &[u8]) -> Option<u64> {
    if text.is_empty() || text.len() > 3 {
        return None;
    }
    text.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let set = match (args::argv(1), args::argv(2)) {
        (None, _) => None,
        (Some(v), None) => match number(v).filter(|&v| v <= 100) {
            Some(v) => Some(v),
            None => {
                println!("usage: mixer [VOLUME], 0 to 100");
                syscall::sys_exit_code(2);
            }
        },
        _ => {
            println!("usage: mixer [VOLUME], 0 to 100");
            syscall::sys_exit_code(2);
        }
    };
    match sound::volume(set) {
        Some(v) => {
            println!("volume {}", v);
            syscall::sys_exit_code(0);
        }
        None => {
            println!("mixer: there is no sound server");
            syscall::sys_exit_code(1);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("mixer: {}", info);
    syscall::sys_exit_code(1);
}
