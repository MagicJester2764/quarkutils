#![no_std]
#![no_main]

//! Somebody sits down: who are they, and what do they get.
//!
//! `login` asks for a name and, where the account has one, a password. It
//! builds the account's shell, has `auth` make it that user — which is where
//! the password is checked and where what the user's session holds comes
//! from — starts it, and waits for it to end. Then it asks again.
//!
//! It holds nothing itself. It used to hold the right to say who a task is,
//! authority over every task and the ports that turn the machine off, and
//! handed all but the first to every shell it started, whoever's.

use quark_rt::accounts;
use quark_rt::auth;
use quark_rt::nameserver;
use quark_rt::session::{self, Refused, Session};
use quark_rt::spawn::Scratch;
use quark_rt::stdio::{read_line, read_secret};
use quark_rt::{print, println, syscall, vfs};

const PAGE_SIZE: usize = 4096;

// Login temp address ranges (non-overlapping with init 0x82-0x88, shell 0x90-0x93)
const FILE_BUF_BASE: usize = 0x94_0000_0000;
// Staging areas for quark_rt::spawn, in this task's own address space.
const ELF_TEMP: usize = 0x95_0000_0000;
const STACK_TEMP: usize = 0x96_0000_0000;
const ARGS_TEMP_PAGE: usize = 0x97_0000_0000;

/// Staging areas quark_rt::spawn maps through while building a child.
const SPAWN_SCRATCH: Scratch = Scratch {
    elf: ELF_TEMP,
    stack: STACK_TEMP,
    args: ARGS_TEMP_PAGE,
};
/// Where `/etc/passwd` is read to, a page at most. Read again at every prompt.
static mut PASSWD: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

/// What a wrong name and a wrong password are both answered with: which of
/// the two it was is not for the terminal to say.
const INCORRECT: &str = "Login incorrect";

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let vfs_tid = match nameserver::lookup_retry(b"vfs", 50) {
        Some(tid) => tid,
        None => {
            println!("login: vfs not found");
            syscall::sys_exit();
        }
    };

    // A terminal's Ctrl-C is raised for every program that has it open. It
    // is for what the session runs, which this is not: this holds the
    // terminal for it.
    let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_IGNORE);
    let _ = syscall::sys_sig_action(syscall::SIGQUIT, syscall::SIG_IGNORE);
    // On a terminal, this is a session: begun here, with the terminal taken
    // as its own, and ended when this ends — which is after one login.
    // Whoever logs in next is in another, and the kernel gives a terminal's
    // slave to nobody outside the session that has it. A session that
    // outlived its user is how the last user's program came to read the
    // next one's password.
    let on_terminal = syscall::sys_pty_number(0).is_ok();
    if on_terminal {
        let _ = syscall::sys_setsid();
        let _ = syscall::sys_pty_set_session(0);
    }
    let mut line_buf = [0u8; 64];
    let mut password = [0u8; quark_rt::crypt::MAX_PASSWORD + 2];

    // What the system says of itself to whoever is about to log in, if it
    // says anything: /etc/issue. Once, and again after each session.
    let mut greet = true;
    loop {
        if greet {
            show(vfs_tid, b"/etc/issue");
            greet = false;
        }
        print!("login: ");
        let n = read_line(&mut line_buf);
        let typed = trim(&line_buf[..n]);
        // Ctrl+C or empty input — re-prompt
        if typed.is_empty() {
            continue;
        }
        let mut name = [0u8; accounts::MAX_NAME];
        if typed.len() > name.len() {
            println!("{}", INCORRECT);
            continue;
        }
        let name = {
            name[..typed.len()].copy_from_slice(typed);
            &name[..typed.len()]
        };

        // A password, if the account has one — and if there is no such
        // account, which is asked for one like any other.
        let secret = match auth::needs(name) {
            Ok(false) => &password[..0],
            Ok(true) => {
                print!("Password: ");
                let n = read_secret(&mut password);
                trim(&password[..n])
            }
            Err(code) => {
                println!("login: {}", auth::why(code));
                continue;
            }
        };

        let passwd = match load_passwd_file(vfs_tid) {
            Some(data) => data,
            None => {
                println!("login: cannot read /etc/passwd");
                continue;
            }
        };
        let Some(user) = accounts::user_named(passwd, name) else {
            // Asked about all the same — there is no child to be made
            // anybody, and no account for it to be — so that being told no
            // takes as long, and is counted the same, as for a name
            // somebody has.
            let refused = auth::bless(0, name, secret, auth::CHECK);
            password.fill(0);
            match refused {
                Err(auth::ERR_WAIT) => println!("login: {}", auth::why(auth::ERR_WAIT)),
                _ => println!("{}", INCORRECT),
            }
            continue;
        };

        // The shell: Quark's own under either of the names it has had, and
        // anybody else's as it is spelled.
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
        // What the terminal understands, if this is one. A program told
        // nothing assumes nothing, and prints no colour.
        let term: &[u8] = if syscall::sys_pty_number(0).is_ok() { b"TERM=linux" } else { b"TERM=dumb" };
        let env: [&[u8]; 6] = [
            &vars[0][..lens[0]],
            &vars[1][..lens[1]],
            &vars[2][..lens[2]],
            &vars[3][..lens[3]],
            b"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            term,
        ];
        // Quark's own shell is told where home is and goes there itself, and
        // makes the environment its programs run in. Anybody else's is
        // started the way login starts one on any Unix: at home, with the
        // environment that says who and where, under a name with a dash in
        // front — the only way a shell is told it is a login shell.
        let qsh_args: [&[u8]; 2] = [user.shell, user.home];
        let unix_args: [&[u8]; 1] = [&argv0[..1 + dashed]];
        let begun = |program: &[u8]| {
            session::prepare(
                vfs_tid,
                &Session {
                    user: &user,
                    password: secret,
                    flags: auth::CHECK,
                    program,
                    args: if ours { &qsh_args } else { &unix_args },
                    env: if ours { &[] } else { &env },
                    home: !ours,
                },
                FILE_BUF_BASE,
                &SPAWN_SCRATCH,
            )
        };
        let info = match begun(user.shell) {
            // A FAT root spells it the other way: lowercase, with no `.ELF`.
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
        // Whatever was typed is not kept past the one use of it.
        password.fill(0);
        let info = match info {
            Ok(info) => info,
            Err(Refused::NoProgram) => {
                println!("login: cannot load shell: {}", core::str::from_utf8(user.shell).unwrap_or("?"));
                continue;
            }
            Err(Refused::Auth(auth::ERR_WRONG)) => {
                println!("{}", INCORRECT);
                continue;
            }
            Err(Refused::Auth(code)) => {
                println!("login: {}", auth::why(code));
                continue;
            }
        };

        // And what the system says to whoever has logged in: /etc/motd.
        show(vfs_tid, b"/etc/motd");

        // Start shell and wait for it to exit
        if info.start().is_err() {
            println!("login: failed to start shell");
            info.discard();
            continue;
        }
        greet = true;

        let _ = syscall::sys_wait();

        // The terminal back. A shell with job control put itself in front,
        // and it has gone: until somebody is in front again nothing typed is
        // for anybody, and a read from behind is not a read. Without being
        // stopped for asking — this *is* behind, and has nobody to continue
        // it. A shell that left things as they were makes this a no-op, and
        // so does standard input not being a terminal.
        if let Some(group) = syscall::sys_getpgid(0) {
            let _ = syscall::sys_pty_set_front(0, group, true);
        }
        // One login to a session. Whatever started this on a terminal
        // starts another, which begins another session.
        if on_terminal {
            println!("");
            syscall::sys_exit_code(0);
        }

        println!(""); // blank line before next login prompt
    }
}

/// A line without the spaces and the newline round it.
fn trim(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r' | b' ') {
        end -= 1;
    }
    let mut start = 0;
    while start < end && line[start] == b' ' {
        start += 1;
    }
    &line[start..end]
}

/// Print a file, if it is there to print.
fn show(vfs_tid: usize, path: &[u8]) {
    let Ok((handle, _, is_dir)) = vfs::open(vfs_tid, path) else {
        return;
    };
    let mut page = [0u8; 512];
    let mut at = 0u32;
    // A greeting, not a document: a few lines.
    while !is_dir && at < 4096 {
        match vfs::read(vfs_tid, handle, &mut page, at) {
            Ok(n) if n > 0 => {
                quark_rt::stdio::print_bytes(&page[..n as usize]);
                at += n;
            }
            _ => break,
        }
    }
    let _ = vfs::close(vfs_tid, handle);
}

fn load_passwd_file(vfs_tid: usize) -> Option<&'static [u8]> {
    let (handle, file_size, _) = vfs::open(vfs_tid, b"/etc/passwd")
        .or_else(|_| vfs::open(vfs_tid, b"/etc/PASSWD"))
        .ok()?;
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(PASSWD) };
    let want = (file_size as usize).min(buf.len());
    let mut len = 0;
    while len < want {
        match vfs::read(vfs_tid, handle, &mut buf[len..want], len as u32) {
            Ok(n) if n > 0 => len += n as usize,
            _ => break,
        }
    }
    let _ = vfs::close(vfs_tid, handle);
    Some(&buf[..len])
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("login: PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
