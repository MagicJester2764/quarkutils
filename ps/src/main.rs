#![no_std]
#![no_main]

use quark_rt::{println, syscall};

const MAX_TASKS: usize = 64;

const STATE_NAMES: [&str; 4] = ["READY", "RUN", "BLOCK", "DEAD"];

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // A process id is what a program is known by: it is never given to
    // another, and it is what `kill` and `wait` in a Unix shell mean by a
    // number. A task id is a slot in the kernel's table, reused as soon as it
    // is free, and is what this system's own calls take. Both are shown; a
    // thread shares its program's process id and has a task id of its own.
    println!("  PID  PPID  TID  STATE  UID");
    for tid in 0..MAX_TASKS {
        if let Ok((state, parent, uid)) = syscall::sys_task_info(tid) {
            if state == 3 { continue; } // skip Dead tasks
            let state_str = if (state as usize) < STATE_NAMES.len() {
                STATE_NAMES[state as usize]
            } else {
                "???"
            };
            // Task 0 is the kernel's idle task and is in no program; asked
            // about 0, the kernel answers for the caller.
            let pid = if tid == 0 { 0 } else { syscall::sys_pid(tid).unwrap_or(0) };
            let ppid = if parent == 0 { 0 } else { syscall::sys_pid(parent).unwrap_or(0) };
            println!("{:5} {:5} {:4}  {:5}  {:3}", pid, ppid, tid, state_str, uid);
        }
    }
    syscall::sys_exit();
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("ps: PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
