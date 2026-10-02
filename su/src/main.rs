#![no_std]
#![no_main]

//! Be somebody else for a while.
//!
//! ```text
//! su [-] [USER] [-c COMMAND]
//! ```
//!
//! Starts USER's shell — root's, if nobody is named — as USER, on this
//! terminal, and waits for it. With `-` the shell starts at USER's home as a
//! login would start it; without, it starts where this was. With `-c` the
//! shell is handed one command to run instead of a terminal to read.
//!
//! USER's password is asked for, unless it is root asking. This program
//! holds nothing that could make anybody anybody: it builds the shell and
//! asks `auth` to say whose it is. There is no setuid bit to give it the
//! right, here or anywhere on this system.

use quark_rt::accounts;
use quark_rt::auth;
use quark_rt::nameserver;
use quark_rt::session::{self, Refused, Session};
use quark_rt::spawn::Scratch;
use quark_rt::stdio::read_secret;
use quark_rt::{args, print, println, syscall};

const FILE_BUF_BASE: usize = 0x94_0000_0000;
const SPAWN_SCRATCH: Scratch = Scratch { elf: 0x95_0000_0000, stack: 0x96_0000_0000, args: 0x97_0000_0000 };
const TEXT_AT: usize = 0x98_0000_0000;
const TEXT_PAGES: usize = 4;

fn fail(what: core::fmt::Arguments) -> ! {
    println!("su: {}", what);
    syscall::sys_exit_code(1);
}

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut login = false;
    let mut name: &[u8] = b"root";
    let mut command: Option<&[u8]> = None;
    let mut named = false;
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        match arg {
            b"-" | b"-l" | b"--login" => login = true,
            b"-c" => {
                i += 1;
                match args::argv(i) {
                    Some(c) => command = Some(c),
                    None => fail(format_args!("-c wants a command")),
                }
            }
            _ if arg.starts_with(b"-") || named => {
                println!("usage: su [-] [USER] [-c COMMAND]");
                syscall::sys_exit_code(2);
            }
            _ => {
                name = arg;
                named = true;
            }
        }
        i += 1;
    }

    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        fail(format_args!("there is no file server"));
    };
    if syscall::sys_mmap(TEXT_AT, TEXT_PAGES).is_err() {
        fail(format_args!("no memory"));
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(TEXT_AT as *mut u8, TEXT_PAGES * 4096) };
    let Some(passwd) = accounts::read(vfs_tid, b"", b"passwd", buf) else {
        fail(format_args!("/etc/passwd cannot be read"));
    };
    let Some(user) = accounts::user_named(passwd, name) else {
        fail(format_args!("there is no user called {}", text(name)));
    };

    // Root is asked for nothing, and neither is anybody of an account that
    // has no password.
    let (me, _) = syscall::sys_get_uid();
    let mut password = [0u8; quark_rt::crypt::MAX_PASSWORD + 2];
    let mut typed = 0;
    if me != 0 {
        match auth::needs(name) {
            Ok(false) => {}
            Ok(true) => {
                print!("Password: ");
                typed = read_secret(&mut password);
                while typed > 0 && matches!(password[typed - 1], b'\n' | b'\r') {
                    typed -= 1;
                }
            }
            Err(code) => fail(format_args!("{}", auth::why(code))),
        }
    }

    let shell_name = user.shell.rsplit(|&b| b == b'/').next().unwrap_or(user.shell);
    let ours = shell_name.eq_ignore_ascii_case(b"qsh") || shell_name.eq_ignore_ascii_case(b"qsh.elf");
    let mut argv0 = [0u8; 65];
    argv0[0] = b'-';
    let dashed = shell_name.len().min(64);
    argv0[1..1 + dashed].copy_from_slice(&shell_name[..dashed]);

    let mut vars = [[0u8; 80]; 4];
    let mut lens = [0usize; 4];
    for (i, (key, value)) in
        [(&b"HOME="[..], user.home), (&b"USER="[..], user.name), (&b"LOGNAME="[..], user.name), (&b"SHELL="[..], user.shell)]
            .iter()
            .enumerate()
    {
        let v = &value[..value.len().min(80 - key.len())];
        vars[i][..key.len()].copy_from_slice(key);
        vars[i][key.len()..key.len() + v.len()].copy_from_slice(v);
        lens[i] = key.len() + v.len();
    }
    let term: &[u8] = if syscall::sys_pty_number(0).is_ok() { b"TERM=linux" } else { b"TERM=dumb" };
    let env: [&[u8]; 6] = [
        &vars[0][..lens[0]],
        &vars[1][..lens[1]],
        &vars[2][..lens[2]],
        &vars[3][..lens[3]],
        b"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        term,
    ];

    // Its arguments: Quark's own shell is told where to begin; anybody
    // else's is named with a dash for a login, and given `-c COMMAND`.
    let mut argv: [&[u8]; 4] = [b""; 4];
    let mut argc = 0;
    let first: &[u8] = if login && !ours { &argv0[..1 + dashed] } else { user.shell };
    argv[argc] = first;
    argc += 1;
    if ours && command.is_none() {
        argv[argc] = if login { user.home } else { b"." };
        argc += 1;
    }
    if let Some(c) = command {
        argv[argc] = b"-c";
        argv[argc + 1] = c;
        argc += 2;
    }

    let begun = |program: &[u8]| {
        session::prepare(
            vfs_tid,
            &Session {
                user: &user,
                password: &password[..typed],
                flags: 0,
                program,
                args: &argv[..argc],
                env: if ours { &[] } else { &env },
                home: login,
            },
            FILE_BUF_BASE,
            &SPAWN_SCRATCH,
        )
    };
    let info = match begun(user.shell) {
        Err(Refused::NoProgram) => {
            let mut alt = [0u8; 64];
            let len = user.shell.len().min(64);
            alt[..len].copy_from_slice(&user.shell[..len]);
            alt[..len].make_ascii_lowercase();
            let alt = alt[..len].strip_suffix(b".elf").unwrap_or(&alt[..len]);
            begun(alt)
        }
        other => other,
    };
    password.fill(0);
    let info = match info {
        Ok(info) => info,
        Err(Refused::NoProgram) => fail(format_args!("{} will not load", text(user.shell))),
        Err(Refused::Auth(auth::ERR_WRONG)) => fail(format_args!("that is not {}'s password", text(name))),
        Err(Refused::Auth(code)) => fail(format_args!("{}", auth::why(code))),
    };

    // What is typed at the terminal is for the shell now, not for this.
    let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_IGNORE);
    let _ = syscall::sys_sig_action(syscall::SIGQUIT, syscall::SIG_IGNORE);
    let tid = info.tid;
    if info.start().is_err() {
        info.discard();
        fail(format_args!("the shell would not start"));
    }
    let status = loop {
        match syscall::sys_wait() {
            Ok((t, code)) if t == tid => break code,
            Ok(_) => continue,
            Err(()) => break 1,
        }
    };
    // The terminal back, as `login` takes it back: a shell with job control
    // put itself in front, and has gone.
    if let Some(group) = syscall::sys_getpgid(0) {
        let _ = syscall::sys_pty_set_front(0, group, true);
    }
    // Ended by a signal, it is reported the way a shell reports it.
    syscall::sys_exit_code(if status < 0 { 128 - status } else { status });
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("su: {}", info);
    syscall::sys_exit_code(255);
}
