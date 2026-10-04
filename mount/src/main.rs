#![no_std]
#![no_main]

//! Put a filesystem somewhere, or say what is where.
//!
//! ```text
//! mount                               what is mounted
//! mount [--mkdir] DEVICE DIR          the filesystem on DEVICE, at DIR
//! mount [--mkdir] -t tmpfs SIZE DIR   a filesystem in memory, at DIR
//! ```
//!
//! `DEVICE` is a disk or a partition as `/dev` names it — `/dev/disk0p2`,
//! or `disk0p2` — and `DIR` a directory, which `--mkdir` makes if it is not
//! there.
//!
//! A filesystem in memory is as big as `SIZE` says — `64M`, `1G`, or a
//! number of mebibytes — and empty, its root anybody's to write in as
//! `/tmp` is. What is in it is in the memory of the server that serves it,
//! and goes when that does: at `umount`, and when the machine stops.
//!
//! **A mount is a server.** This starts a file server on the volume, the
//! same program that serves the root, and hands it to the file server `DIR`
//! is in, which from then on passes it whatever goes through `DIR`. That is
//! the whole of it: there is no table in the kernel, and the listing shows
//! which process serves each. The server claims its volume from the disk's
//! driver for as long as it runs, which is what stops anything formatting a
//! partition that is mounted.

use quark_rt::ipc::Message;
use quark_rt::manifest::CapReq;
use quark_rt::spawn::{self, Scratch};
use quark_rt::{args, block, nameserver, println, syscall, vfs};

// What starting a program takes, which is what this does.
quark_rt::manifest!([CapReq::task_mgmt(0), CapReq::phys_alloc(64)]);

/// Where the file server's image is read to before it is loaded.
const IMAGE_AT: usize = 0x9A_0000_0000;

static SCRATCH: Scratch = Scratch {
    elf: 0x9B_0000_0000,
    stack: 0x9C_0000_0000,
    args: 0x9D_0000_0000,
};

/// The block drivers there may be, by the names they register under.
const DRIVERS: [&[u8]; 12] = [
    b"disk0", b"disk1", b"disk2", b"disk3", b"ram0", b"ram1", b"ram2", b"ram3", b"ram4", b"ram5",
    b"ram6", b"ram7",
];

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

fn fail(what: core::fmt::Arguments) -> ! {
    println!("mount: {}", what);
    syscall::sys_exit_code(1);
}

/// `disk0p2` or `/dev/disk0p2`: the driver's name, and the volume.
fn device(arg: &[u8]) -> Option<(&'static [u8], u64)> {
    let name = arg.strip_prefix(b"/dev/").unwrap_or(arg);
    let (driver, rest) = DRIVERS.iter().find_map(|d| name.strip_prefix(*d).map(|rest| (*d, rest)))?;
    let volume = match rest {
        [] => 0,
        [b'p', digits @ ..] if !digits.is_empty() && digits.len() <= 2 && digits[0] != b'0' => {
            digits.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))?
        }
        _ => return None,
    };
    Some((driver, volume))
}

/// `path` written out whole: from the root, with no `.`, no `..` and no
/// doubled slash. It is what is written down as where the mount is.
fn whole(vfs_tid: usize, path: &[u8], out: &mut [u8; 512]) -> Option<usize> {
    let mut from = [0u8; 512];
    let mut from_len = 0;
    if path.first() != Some(&b'/') {
        from_len = vfs::getcwd(vfs_tid, &mut from).ok()?;
    }
    let mut len = 0;
    for part in from[..from_len].split(|&b| b == b'/').chain(path.split(|&b| b == b'/')) {
        match part {
            b"" | b"." => {}
            b".." => {
                while len > 0 && out[len - 1] != b'/' {
                    len -= 1;
                }
                len = len.saturating_sub(1);
            }
            _ => {
                if len + 1 + part.len() > out.len() {
                    return None;
                }
                out[len] = b'/';
                out[len + 1..len + 1 + part.len()].copy_from_slice(part);
                len += 1 + part.len();
            }
        }
    }
    if len == 0 {
        out[0] = b'/';
        len = 1;
    }
    Some(len)
}

fn list(vfs_tid: usize) -> ! {
    let mut record = [0u8; 512];
    for index in 0.. {
        match vfs::mounted(vfs_tid, index, &mut record) {
            Ok(Some(m)) => {
                let (source, target) = vfs::mount_record(&record[..m.len]);
                println!(
                    "{} on {} type {} (process {})",
                    text(source),
                    text(target),
                    vfs::kind_name(m.kind),
                    m.pid
                );
            }
            Ok(None) => break,
            Err(code) => fail(format_args!("the file server would not say ({})", code)),
        }
    }
    syscall::sys_exit_code(0);
}

fn usage() -> ! {
    println!("usage: mount");
    println!("       mount [--mkdir] DEVICE DIR");
    println!("       mount [--mkdir] -t tmpfs SIZE DIR");
    syscall::sys_exit_code(2);
}

/// `64M`, `1G`, `512K` or `64`: how many mebibytes, if it is a whole number
/// of them.
fn megabytes(size: &[u8]) -> Option<u64> {
    let (digits, scale) = match size.last()? {
        b'K' | b'k' => (&size[..size.len() - 1], 1u64),
        b'M' | b'm' => (&size[..size.len() - 1], 1 << 10),
        b'G' | b'g' => (&size[..size.len() - 1], 1 << 20),
        _ => (size, 1 << 10),
    };
    if digits.is_empty() || digits.len() > 12 {
        return None;
    }
    let n = digits.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))?;
    let kilobytes = n.checked_mul(scale)?;
    (kilobytes % 1024 == 0 && kilobytes > 0).then_some(kilobytes / 1024)
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        fail(format_args!("there is no file server"));
    };
    let mut words: [&[u8]; 3] = [b""; 3];
    let mut n = 0;
    let mut make = false;
    let mut kind: Option<&[u8]> = None;
    let mut i = 1;
    while i < args::argc() {
        match args::argv(i) {
            Some(b"--mkdir") => make = true,
            Some(b"-t") if kind.is_none() => {
                i += 1;
                kind = Some(args::argv(i).unwrap_or_else(|| usage()));
            }
            Some(word) if n < 2 && !word.starts_with(b"-") => {
                words[n] = word;
                n += 1;
            }
            _ => usage(),
        }
        i += 1;
    }
    if n == 0 && !make && kind.is_none() {
        list(vfs_tid);
    }
    if n != 2 {
        usage();
    }
    let (source, dir) = (words[0], words[1]);
    let in_memory = match kind {
        None => false,
        Some(b"tmpfs") => true,
        Some(other) => fail(format_args!("{} is not a kind of filesystem this mounts by name; tmpfs is", text(other))),
    };

    // Said first, before a file server is started to be refused its disk.
    if syscall::sys_get_uid().0 != 0 {
        fail(format_args!("only root mounts a filesystem"));
    }

    // What the server is told to serve: a driver's volume, or memory.
    let mut size_text = [0u8; 20];
    let (driver, volume): (&[u8], u64) = if in_memory {
        let Some(mb) = megabytes(source) else {
            fail(format_args!("{} is not a size: 64M, 1G, or a number of mebibytes", text(source)));
        };
        let mut at = size_text.len();
        let mut left = mb;
        loop {
            at -= 1;
            size_text[at] = b'0' + (left % 10) as u8;
            left /= 10;
            if left == 0 {
                break;
            }
        }
        (b"mem", 0)
    } else {
        let Some((driver, volume)) = device(source) else {
            fail(format_args!("{} is not a disk or a partition of one", text(source)));
        };
        let there = nameserver::lookup(driver).and_then(|tid| block::info(tid, volume).ok());
        if there.is_none() {
            fail(format_args!("there is no {}", text(source)));
        }
        (driver, volume)
    };
    let size_text: &[u8] = {
        let start = size_text.iter().position(|&b| b != 0).unwrap_or(size_text.len());
        &size_text[start..]
    };

    let mut target = [0u8; 512];
    let Some(target_len) = whole(vfs_tid, dir, &mut target) else {
        fail(format_args!("{} is not a path this can write down", text(dir)));
    };
    let target = &target[..target_len];
    if make {
        match vfs::mkdir(vfs_tid, target) {
            Ok(()) | Err(vfs::ERR_EXISTS) => {}
            Err(vfs::ERR_PERMISSION) => fail(format_args!("{} cannot be made: permission denied", text(target))),
            Err(code) => fail(format_args!("{} cannot be made ({})", text(target), code)),
        }
    }

    // The file server for it: this program's child, started on the volume
    // and told it is to be mounted rather than to be the root.
    let grant = |image: &[u8], tid: usize| {
        quark_rt::manifest::grant_image(tid, image, 12);
    };
    let loaded = spawn::load_path(vfs_tid, b"/usr/bin/vfs", IMAGE_AT, &SCRATCH, grant)
        .or_else(|()| spawn::load_path(vfs_tid, b"/usr/bin/VFS.ELF", IMAGE_AT, &SCRATCH, grant));
    let Ok(server) = loaded else {
        fail(format_args!("there is no /usr/bin/vfs to serve it"));
    };
    let mut digits = [0u8; 2];
    let volume_text: &[u8] = if volume >= 10 {
        digits = [b'0' + (volume / 10) as u8, b'0' + (volume % 10) as u8];
        &digits
    } else {
        digits[0] = b'0' + volume as u8;
        &digits[..1]
    };
    let second = if in_memory { size_text } else { volume_text };
    let started = spawn::set_args(&server, &[b"vfs", driver, second, b"mount"], &SCRATCH).is_ok()
        // It finds the disk's driver by name.
        && syscall::sys_cap_grant(server.tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT).is_ok()
        && server.start().is_ok();
    if !started {
        let _ = syscall::sys_task_kill(server.tid);
        fail(format_args!("the file server for {} could not be started", text(source)));
    }

    // It answers when it has found a filesystem and is serving it. If it
    // found none, or the volume is somebody else's, it has said so and gone,
    // and the call fails.
    let ping = Message { sender: 0, tag: quark_rt::ipc::TAG_PING, data: [0; 6] };
    let mut reply = Message::empty();
    let up = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, server.tid as u64, 0).is_ok()
        && syscall::sys_call(server.tid, &ping, &mut reply).is_ok()
        // A server that went while this waited is answered for by the
        // kernel, with a refusal: an answer, and not the one asked for.
        && reply.tag == quark_rt::ipc::TAG_PING;
    let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
    if !up {
        let _ = syscall::sys_task_kill(server.tid);
        if in_memory {
            fail(format_args!("there is not {} of memory to spare", text(source)));
        }
        fail(format_args!(
            "{} has no filesystem this system knows, or is in use",
            text(source)
        ));
    }

    // Where it came from, as the listing says it: the device, or, as Linux
    // says of a filesystem in memory, "tmpfs".
    let mut from = [0u8; 32];
    let from: &[u8] = if in_memory {
        b"tmpfs"
    } else {
        let name = source.strip_prefix(b"/dev/").unwrap_or(source);
        from[..5].copy_from_slice(b"/dev/");
        from[5..5 + name.len()].copy_from_slice(name);
        &from[..5 + name.len()]
    };
    if let Err(code) = vfs::mount(vfs_tid, server.tid, from, target) {
        let _ = syscall::sys_task_kill(server.tid);
        match code {
            vfs::ERR_PERMISSION => fail(format_args!("only root mounts a filesystem")),
            vfs::ERR_NOT_FOUND => fail(format_args!("there is no {}", text(target))),
            vfs::ERR_NOT_DIR => fail(format_args!("{} is not a directory", text(target))),
            vfs::ERR_BUSY => fail(format_args!("something is mounted on {} already", text(target))),
            vfs::ERR_NOT_SUPPORTED => fail(format_args!(
                "the filesystem {} is in cannot have another mounted in it",
                text(target)
            )),
            code => fail(format_args!("{} could not be mounted on {} ({})", text(source), text(target), code)),
        }
    }
    // Written down for programs that look there; the file server is the
    // record that counts.
    let _ = vfs::write_mtab(vfs_tid);
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("mount: {}", info);
    syscall::sys_exit_code(255);
}
