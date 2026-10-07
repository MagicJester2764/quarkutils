#![no_std]
#![no_main]

use quark_rt::{args, println, syscall};

use quark_rt::manifest::CapReq;

// Signals every task to exit, then asks the kernel to turn the machine off
// or to start it again, which it does the way the firmware's tables say.
//
// The three ports are what this did before the kernel could, and what it
// still does on a machine whose firmware says nothing of power: they are
// the ones the machine QEMU pretends to be listens on, and on any other
// they do nothing.
quark_rt::manifest!([
    CapReq::task_mgmt(0),
    CapReq::power(),
    CapReq::ioport(0x604, 0x604),
    CapReq::ioport(0xB004, 0xB004),
    CapReq::ioport(RESET_CONTROL, RESET_CONTROL),
]);

/// The reset control register, which every PC chipset since PIIX has at this
/// port: bit 1 says a hard reset, and bit 2, going from 0 to 1, does it.
const RESET_CONTROL: u16 = 0xCF9;

/// The power-management control register of the machine QEMU pretends to
/// be (PIIX4), for a machine with no tables to say where its own is.
const ACPI_PM1A_CNT: u16 = 0x604;

/// "Sleep now", with the kind of sleep that machine calls off: 0.
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

    // Turning a machine off is a capability, and a session holds it or does
    // not. Said before anything is ended: this used to end what it could,
    // fail to turn the machine off, and leave whoever ran it with no session.
    let may = (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(my_tid, slot), Ok(c) if c.valid && c.cap_type == syscall::CAP_TYPE_POWER)
    });
    if !may {
        println!(
            "shutdown: this account may not {}",
            if again { "restart the machine" } else { "turn the machine off" }
        );
        syscall::sys_exit_code(1);
    }

    // The services first, in order — what nothing still running needs, then
    // what it needed, each given SIGTERM and five seconds — and then the log
    // written and the files synced: a service that writes as it goes has its
    // file server still there to write to. init answers once it has done.
    if let Some(init) = quark_rt::services::manager() {
        if let Err(code) = quark_rt::services::stop_all(init, force) {
            println!("shutdown: the services were not stopped in order: {}", quark_rt::services::why(code));
        }
    }

    // The file servers are among what is about to be ended, and a write is
    // answered a moment before it is recorded for good. Have it recorded.
    if let Some(vfs_tid) = quark_rt::nameserver::lookup(b"vfs") {
        let _ = quark_rt::vfs::sync(vfs_tid);
    }

    // Phase 1: Signal all user tasks to terminate
    let mut signaled = 0usize;
    for tid in syscall::tasks().filter(|&t| t >= 2) {
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
        for tid in syscall::tasks().filter(|&t| t >= 2) {
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
        // The kernel's to do, and it always can. What follows is for a
        // kernel that would not.
        syscall::sys_restart();
        // A hard reset: say which kind, then ask for it.
        syscall::sys_ioport_write(RESET_CONTROL, 0x02);
        syscall::sys_ioport_write(RESET_CONTROL, 0x06);
        syscall::sleep_ms(500);
        println!("shutdown: the machine would not reset; turning it off instead");
    }

    // Phase 3: off. The kernel, by the firmware's tables; and where they do
    // not say how, or it did not work, the ports.
    syscall::sys_power_off();
    syscall::sys_ioport_write16(ACPI_PM1A_CNT, ACPI_S5_VALUE);
    syscall::sys_ioport_write16(0xB004, ACPI_S5_VALUE);

    // Last resort: HLT loop
    println!("shutdown: the machine would not turn off; it is halted");
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
