#![no_std]
#![no_main]

use quark_rt::{args, println, syscall};

use quark_rt::manifest::CapReq;

// Signals every task to exit, then writes the ACPI poweroff ports — or, to
// start the machine again, the reset control register.
quark_rt::manifest!([
    CapReq::task_mgmt(0),
    CapReq::ioport(0x604, 0x604),
    CapReq::ioport(0xB004, 0xB004),
    CapReq::ioport(RESET_CONTROL, RESET_CONTROL),
]);

/// The reset control register, which every PC chipset since PIIX has at this
/// port: bit 1 says a hard reset, and bit 2, going from 0 to 1, does it.
const RESET_CONTROL: u16 = 0xCF9;

const MAX_TASKS: usize = 64;

/// ACPI PM1a Control register port (QEMU PIIX4 / i440fx).
const ACPI_PM1A_CNT: u16 = 0x604;

/// S5 sleep value: SLP_EN (bit 13) | SLP_TYP=S5 (bits 10-12, value varies).
/// QEMU i440fx/PIIX4 uses SLP_TYP=0 for S5, so just SLP_EN.
const ACPI_S5_VALUE: u16 = 1 << 13;

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let force = has_flag(b"-f") || has_flag(b"--force");
    let again = has_flag(b"-r") || has_flag(b"--reboot");
    if (1..args::argc()).any(|i| {
        !matches!(args::argv(i), Some(b"-f" | b"--force" | b"-r" | b"--reboot"))
    }) {
        println!("usage: shutdown [-r] [-f]");
        println!("  -r  start the machine again instead of turning it off");
        println!("  -f  do not wait for programs to end by themselves");
        syscall::sys_exit_code(2);
    }
    let my_tid = syscall::sys_getpid() as usize;

    // Turning a machine off is a capability — the port that does it — and a
    // session holds it or does not. Said before anything is ended: this used
    // to end what it could, fail to turn the machine off, and leave whoever
    // ran it with no session.
    let port = if again { RESET_CONTROL } else { ACPI_PM1A_CNT } as u64;
    let may = (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(my_tid, slot), Ok(c) if c.valid
            && c.cap_type == syscall::CAP_TYPE_IOPORT && c.param0 <= port && port <= c.param1)
    });
    if !may {
        println!(
            "shutdown: this account may not {}",
            if again { "restart the machine" } else { "turn the machine off" }
        );
        syscall::sys_exit_code(1);
    }

    // The file servers are among what is about to be ended, and a write is
    // answered a moment before it is recorded for good. Have it recorded.
    if let Some(vfs_tid) = quark_rt::nameserver::lookup(b"vfs") {
        let _ = quark_rt::vfs::sync(vfs_tid);
    }

    // Phase 1: Signal all user tasks to terminate
    let mut signaled = 0usize;
    for tid in 2..MAX_TASKS {
        if tid == my_tid {
            continue;
        }
        if let Ok((state, _, _)) = syscall::sys_task_info(tid) {
            if state == 3 {
                continue; // Dead
            }
            let sig = if force { syscall::SIG_KILL } else { syscall::SIG_TERM };
            if syscall::sys_signal(tid, sig).is_ok() {
                signaled += 1;
            }
        }
    }

    if signaled > 0 {
        if force {
            println!("shutdown: killed {} tasks", signaled);
        } else {
            println!("shutdown: sent SIGTERM to {} tasks, waiting...", signaled);
            // Give them two and a half seconds to go by themselves. The
            // kernel would end each one five seconds after the signal anyway;
            // the survivors are killed below rather than waited for.
            syscall::sleep_ms(2500);
        }
    }

    println!("shutdown: {}...", if again { "starting again" } else { "powering off" });

    // Phase 2: Force-kill any survivors
    if !force {
        for tid in 2..MAX_TASKS {
            if tid == my_tid {
                continue;
            }
            if let Ok((state, _, _)) = syscall::sys_task_info(tid) {
                if state != 3 {
                    let _ = syscall::sys_signal(tid, syscall::SIG_KILL);
                }
            }
        }
    }

    if again {
        // A hard reset: say which kind, then ask for it.
        syscall::sys_ioport_write(RESET_CONTROL, 0x02);
        syscall::sys_ioport_write(RESET_CONTROL, 0x06);
        syscall::sleep_ms(500);
        println!("shutdown: the machine would not reset; turning it off instead");
    }

    // Phase 3: ACPI S5 power-off
    syscall::sys_ioport_write16(ACPI_PM1A_CNT, ACPI_S5_VALUE);

    // If ACPI didn't work, try alternate QEMU ports
    syscall::sys_ioport_write16(0xB004, ACPI_S5_VALUE);

    // Last resort: HLT loop
    println!("shutdown: ACPI power-off failed, system halted");
    loop {
        core::hint::spin_loop();
    }
}

fn has_flag(flag: &[u8]) -> bool {
    let argc = args::argc();
    for i in 1..argc {
        if let Some(arg) = args::argv(i) {
            if arg == flag {
                return true;
            }
        }
    }
    false
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("shutdown: PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
