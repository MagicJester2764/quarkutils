#![no_std]
#![no_main]

use quark_rt::{print, println, syscall};

const MAX_TASKS: usize = 64;

const STATE_NAMES: [&str; 5] = ["READY", "RUN", "BLOCK", "DEAD", "STOP"];

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // A process id is what a program is known by: it is never given to
    // another, and it is what `kill` and `wait` in a Unix shell mean by a
    // number. A task id is a slot in the kernel's table, reused as soon as it
    // is free, and is what this system's own calls take. Both are shown; a
    // thread shares its program's process id and has a task id of its own.
    // How nice it is and how long it has run are its program's, and so is
    // what it was started as, which its spawner told the kernel.
    println!("  PID  PPID  TID  STATE  UID  NI       TIME  CMD");
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
            let nice = if pid == 0 { 0 } else { syscall::sys_nice(pid, None).unwrap_or(0) };
            // Minutes, seconds and hundredths, as `ps` and `top` print it.
            let ran = if tid == 0 { 0 } else { syscall::sys_usage_of(tid).map_or(0, |u| u.total_ns()) };
            let hundredths = ran / 10_000_000;
            print!(
                "{:5} {:5} {:4}  {:5}  {:3} {:3} {:4}:{:02}.{:02}  ",
                pid,
                ppid,
                tid,
                state_str,
                uid,
                nice,
                hundredths / 6000,
                hundredths / 100 % 60,
                hundredths % 100
            );
            let mut line = [0u8; syscall::PROGRAM_NAME_MAX];
            match syscall::sys_program_name(tid, &mut line) {
                _ if tid == 0 => println!("[idle]"),
                Some(n) if n > 0 => {
                    // The arguments, a space between each, as `ps -f` has it.
                    let line = &mut line[..n];
                    let end = line.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
                    for b in line[..end].iter_mut() {
                        if *b == 0 {
                            *b = b' ';
                        }
                    }
                    println!("{}", core::str::from_utf8(&line[..end]).unwrap_or("?"));
                }
                _ => println!("?"),
            }
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
