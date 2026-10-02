#![no_std]
#![no_main]

//! Open the console's terminal and run `login` on it.
//!
//! A program's standard input used to be a message to the input server and
//! its output a pipe to the console, and neither is a terminal: `isatty` says
//! no, there is nothing for `tcsetattr` to set, and a shell that edits its own
//! command line has nowhere to turn the echo off. This is what puts a session
//! on a real one.
//!
//! It asks the console for its terminal — the console holds the master and
//! says which pty it is — opens the slave, and starts `login` with the slave
//! as its standard input, output and error. Everything `login` starts
//! inherits those. When `login` goes it is started again.
//!
//! The slave stays open here between sessions, so the terminal never sees its
//! last holder go: a hangup is for a terminal whose user has left, and this
//! one has only finished a session.
//!
//! The *session*, in the sense a shell with job control needs, is not this
//! program's: `login` begins one each time it is started, and takes the
//! terminal as that session's own. So each login is a session, a session
//! ends when its `login` does, and the kernel gives a terminal's slave to
//! nobody outside the session that has it — which is what keeps a program
//! somebody left running, and then logged out, from reading what the next
//! person types. This led one session for as long as the machine was up,
//! and everybody who ever logged in was in it.

use quark_rt::ipc::Message;
use quark_rt::spawn::{self, Scratch};
use quark_rt::{nameserver, println, syscall, vfs};

// This asks for nothing, and `login` after it holds nothing: what a session
// holds comes from `auth`, by whose account it is. Both used to hold the
// right to say who a task is and the ports that turn the machine off, to
// have them to hand on.

/// Ask the console for its terminal: the reply's first word is the pty.
const TAG_TTY_OPEN: u64 = 0x110;

const FILE_BUF: usize = 0x94_0000_0000;
static SPAWN_SCRATCH: Scratch = Scratch {
    elf: 0x95_0000_0000,
    stack: 0x96_0000_0000,
    args: 0x97_0000_0000,
};

/// The console's terminal, once the console is there to ask.
fn open_terminal() -> Option<usize> {
    let console = nameserver::lookup_retry(b"console", 50)?;
    let ask = Message { sender: 0, tag: TAG_TTY_OPEN, data: [0; 6] };
    let mut reply = Message::empty();
    // The console may be busy drawing a boot's worth of messages.
    for _ in 0..50 {
        if let syscall::CallOutcome::Replied = syscall::sys_call_timeout(console, &ask, &mut reply, 100) {
            if reply.tag == 0 {
                return syscall::sys_pty_open(reply.data[0] as usize).ok();
            }
            return None;
        }
    }
    None
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // A terminal's Ctrl-C is raised for every program that has it open. It
    // is for what the session runs, which this is not: this holds the
    // terminal for it.
    let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_IGNORE);
    let _ = syscall::sys_sig_action(syscall::SIGQUIT, syscall::SIG_IGNORE);
    let program = quark_rt::args::argv(1).unwrap_or(b"/usr/bin/login");
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 50) else {
        println!("getty: no file server");
        syscall::sys_exit_code(1);
    };
    let Some(tty) = open_terminal() else {
        println!("getty: the console has no terminal to give");
        syscall::sys_exit_code(1);
    };
    // As the terminal is before anybody has changed it. A session that ends
    // with the echo off — a shell killed at its prompt — must not leave the
    // next one typing blind.
    let fresh = syscall::sys_pty_get_termios(tty).ok();

    loop {
        if let Some(t) = fresh.as_ref() {
            let _ = syscall::sys_pty_set_termios(tty, t);
        }
        let grant = |image: &[u8], tid: usize| {
            quark_rt::manifest::grant_image(tid, image, syscall::SLOT_SCRATCH);
        };
        let info = spawn::load_path(vfs_tid, program, FILE_BUF, &SPAWN_SCRATCH, grant).or_else(|()| {
            // A FAT root spells it the other way.
            spawn::load_path(vfs_tid, b"/usr/bin/LOGIN.ELF", FILE_BUF, &SPAWN_SCRATCH, grant)
        });
        let Ok(info) = info else {
            println!("getty: cannot start the login program");
            syscall::sys_exit_code(1);
        };
        // The nameserver, which is how it finds everything else; the terminal
        // on all three; and somewhere to be.
        let _ = syscall::sys_cap_grant(info.tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT);
        for fd in 0..3 {
            let _ = syscall::sys_fd_dup(info.tid, fd, tty);
        }
        let _ = vfs::give_cwd(vfs_tid, info.tid);
        if spawn::set_args(&info, &[b"login"], &SPAWN_SCRATCH).is_err() || info.start().is_err() {
            println!("getty: could not start the login program");
            syscall::sys_exit_code(1);
        }
        let _ = syscall::sys_wait_for(info.tid);
        // Its session went with it, and the terminal is nobody's until the
        // next one takes it. Not at once: a login program that dies as it
        // starts would otherwise be started as fast as the machine can load
        // it — and one that ended a session in the ordinary way is not kept
        // waiting for long.
        syscall::sleep_ticks(10);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("getty: {}", info);
    syscall::sys_exit_code(255);
}
