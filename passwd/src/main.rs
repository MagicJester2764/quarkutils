#![no_std]
#![no_main]

//! Change a password.
//!
//! ```text
//! passwd [USER]            the caller's own, or USER's if it is root asking
//! passwd -d USER           take the password away: the account asks for none
//! passwd -l USER           lock it: nobody logs in to it with a password
//! passwd --root DIR ...    in a system mounted at DIR, not this one
//! ```
//!
//! The password is kept as a hash in `/etc/shadow`, which only root may
//! read, so somebody changing their own cannot write it themselves: `auth`
//! does, having checked the old one. With `--root` there is no `auth` to ask
//! — the system at DIR is not running — and root, who alone may do it,
//! writes the file.

use quark_rt::accounts;
use quark_rt::auth;
use quark_rt::crypt;
use quark_rt::nameserver;
use quark_rt::stdio::read_secret;
use quark_rt::{args, print, println, syscall};

const TEXT_AT: usize = 0x98_0000_0000;
const TEXT_PAGES: usize = 12;

fn fail(what: core::fmt::Arguments) -> ! {
    println!("passwd: {}", what);
    syscall::sys_exit_code(1);
}

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn usage() -> ! {
    println!("usage: passwd [--root DIR] [-d | -l] [USER]");
    syscall::sys_exit_code(2);
}

/// A password typed twice the same. `None` if they differ or it is empty.
fn new_password(buf: &mut [u8; crypt::MAX_PASSWORD + 2]) -> Option<usize> {
    let mut again = [0u8; crypt::MAX_PASSWORD + 2];
    print!("New password: ");
    let a = typed(buf);
    print!("Again: ");
    let b = typed(&mut again);
    let same = a == b && buf[..a] == again[..b];
    again.fill(0);
    if !same {
        println!("passwd: they are not the same");
        return None;
    }
    if a == 0 {
        println!("passwd: a password has to be something: `passwd -d` takes one away");
        return None;
    }
    if a > crypt::MAX_PASSWORD {
        println!("passwd: that is longer than {} characters", crypt::MAX_PASSWORD);
        return None;
    }
    Some(a)
}

/// A line read without being shown, less its newline.
fn typed(buf: &mut [u8]) -> usize {
    let mut n = read_secret(buf);
    while n > 0 && matches!(buf[n - 1], b'\n' | b'\r') {
        n -= 1;
    }
    n
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let mut root: &[u8] = b"";
    let mut named: Option<&[u8]> = None;
    let (mut delete, mut lock) = (false, false);
    let mut i = 1;
    while let Some(arg) = args::argv(i) {
        match arg {
            b"--root" | b"-R" => {
                i += 1;
                root = args::argv(i).unwrap_or_else(|| usage());
            }
            b"-d" => delete = true,
            b"-l" => lock = true,
            _ if arg.starts_with(b"-") || named.is_some() => usage(),
            _ => named = Some(arg),
        }
        i += 1;
    }
    if delete && lock {
        usage();
    }

    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        fail(format_args!("there is no file server"));
    };
    if syscall::sys_mmap(TEXT_AT, TEXT_PAGES).is_err() {
        fail(format_args!("no memory"));
    }
    let pages = unsafe { core::slice::from_raw_parts_mut(TEXT_AT as *mut u8, TEXT_PAGES * 4096) };
    let (passwd_buf, rest) = pages.split_at_mut(4 * 4096);
    let (shadow_buf, out_buf) = rest.split_at_mut(4 * 4096);
    let Some(passwd) = accounts::read(vfs_tid, root, b"passwd", passwd_buf) else {
        fail(format_args!("the accounts cannot be read"));
    };
    let (me, _) = syscall::sys_get_uid();
    let user = match named {
        Some(name) => accounts::user_named(passwd, name),
        None => accounts::user_numbered(passwd, me),
    };
    let Some(user) = user else {
        fail(format_args!("there is no such user"));
    };
    if (delete || lock || !root.is_empty()) && me != 0 {
        fail(format_args!("only root does that"));
    }

    let mut new = [0u8; crypt::MAX_PASSWORD + 2];

    // In a system that is not running, or taking a password away or locking
    // it: root writes the file.
    if !root.is_empty() || delete || lock {
        let shadow = accounts::read(vfs_tid, root, b"shadow", shadow_buf).unwrap_or(b"");
        let mut hash = [0u8; crypt::MAX_HASH];
        let hash_len = if delete {
            0
        } else if lock {
            hash[0] = b'!';
            1
        } else {
            let Some(n) = new_password(&mut new) else {
                syscall::sys_exit_code(1);
            };
            let mut random = [0u8; 12];
            if syscall::sys_getrandom(&mut random) != Ok(random.len()) {
                fail(format_args!("no randomness to salt it with"));
            }
            let made = crypt::make(&new[..n], &random, &mut hash);
            new.fill(0);
            made.unwrap_or_else(|| fail(format_args!("that could not be hashed")))
        };
        let mut day = [0u8; 10];
        let day = accounts::digits((syscall::unix_time() / 86400) as u32, &mut day);
        let mut line = [0u8; 256];
        let Some(line) =
            accounts::line(&[user.name, &hash[..hash_len], day, b"", b"", b"", b"", b"", b""], &mut line)
        else {
            fail(format_args!("that name will not go in a file"));
        };
        let Some(len) = accounts::with_record(shadow, user.name, b':', Some(line), out_buf) else {
            fail(format_args!("/etc/shadow is too long"));
        };
        if let Err(code) = accounts::write(vfs_tid, root, b"shadow", &out_buf[..len], 0o600) {
            fail(format_args!("/etc/shadow could not be written ({})", code));
        }
        println!(
            "passwd: {}",
            match (delete, lock) {
                (true, _) => "that account asks for no password now",
                (_, true) => "that account is locked",
                _ => "changed",
            }
        );
        syscall::sys_exit_code(0);
    }

    // Here, and now: the old one if it is not root asking, and the new one.
    let mut old = [0u8; crypt::MAX_PASSWORD + 2];
    let mut old_len = 0;
    if me != 0 {
        match auth::needs(user.name) {
            Ok(true) => {
                print!("Current password: ");
                old_len = typed(&mut old);
            }
            Ok(false) => {}
            Err(code) => fail(format_args!("{}", auth::why(code))),
        }
    }
    let Some(n) = new_password(&mut new) else {
        old.fill(0);
        syscall::sys_exit_code(1);
    };
    let changed = auth::passwd(user.name, &old[..old_len], &new[..n]);
    old.fill(0);
    new.fill(0);
    match changed {
        Ok(()) => {
            println!("passwd: changed");
            syscall::sys_exit_code(0);
        }
        Err(auth::ERR_WRONG) => fail(format_args!("that is not {}'s password", text(user.name))),
        Err(code) => fail(format_args!("{}", auth::why(code))),
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("passwd: {}", info);
    syscall::sys_exit_code(255);
}
