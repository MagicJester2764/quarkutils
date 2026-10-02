#![no_std]
#![no_main]

//! `auth`: who somebody is.
//!
//! The one program, with `init`, that holds the right to say who a task is,
//! and the only reader of `/etc/shadow`. It does two things.
//!
//! It **blesses a child**. A client — `login`, `su` — builds a child the
//! ordinary way, holding nothing but its descriptors and the nameserver's
//! endpoint, and before starting it asks for it to be made somebody. This
//! checks the password, tells the kernel who the child is, hands the child
//! what that account's sessions hold, and answers; the client starts the
//! child. Nothing here runs a program, and nothing is ever narrowed: a
//! fresh child holds nothing to narrow.
//!
//! And it **changes a password**, which is a line of `/etc/shadow`
//! rewritten.
//!
//! There is no setuid program on this system and there cannot be: a program
//! is loaded by whoever starts it, so nothing can vouch that what runs is the
//! file whose mode said so. And a spawner hands on only what it holds, so no
//! manifest could give `su` the right either: a shell that could give it
//! could keep it. A program that needs to be somebody else asks here.

use quark_rt::accounts::{self, Rights};
use quark_rt::auth::*;
use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{crypt, nameserver, println, syscall, vfs};

// Everything any session may hold, which is what this hands out of: nobody
// is given what the giver has not got.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_SERVER),
    CapReq::set_uid(),
    CapReq::task_mgmt(0),
    CapReq::phys_alloc(64),
    CapReq::ioport(0x604, 0x604),
    CapReq::ioport(0xB004, 0xB004),
    CapReq::ioport(0xCF9, 0xCF9),
    CapReq::clock(),
]);

/// The slot a capability is made in before it is handed over.
const SCRATCH: usize = 14;

const FILE_MAX: usize = 16 * 1024;
static mut PASSWD_TEXT: [u8; FILE_MAX] = [0; FILE_MAX];
static mut SHADOW_TEXT: [u8; FILE_MAX] = [0; FILE_MAX];
static mut GROUP_TEXT: [u8; FILE_MAX] = [0; FILE_MAX];
static mut RIGHTS_TEXT: [u8; FILE_MAX] = [0; FILE_MAX];
static mut SCRATCH_TEXT: [u8; FILE_MAX] = [0; FILE_MAX];

/// One of the account files, read whole. `None` if it is not there.
fn load(vfs_tid: usize, name: &[u8], buf: *mut [u8; FILE_MAX]) -> Option<&'static [u8]> {
    accounts::read(vfs_tid, b"", name, unsafe { &mut *buf })
}

/// Wrong passwords, remembered: somebody who has got one wrong is not heard
/// about that name again until a moment has passed, longer each time.
///
/// Refusing early rather than answering late — this serves everybody, and a
/// server asleep for one caller's mistake is asleep for all of them.
///
/// Kept by who asked as well as about whom: counted by account alone, any
/// user could keep root out of its own account by guessing at it. And kept
/// by the *name* that was typed, hashed into one of a few places, rather
/// than by the account it names — so that a name nobody has is counted
/// exactly as a name somebody has, and how a wrong guess is treated says
/// nothing about which names there are. Nothing here is found a place when
/// it is first needed, so nothing can be pushed out to make room: a table
/// with room for the last few names had its count for root thrown away by
/// anybody who guessed at enough other names.
#[derive(Clone, Copy)]
struct Tries {
    /// The name the last wrong one was for, hashed: a right password for
    /// that name is what clears the count, and no other name's is.
    name: u32,
    wrong: u32,
    until: u64,
    last: u64,
}

const NO_TRIES: Tries = Tries { name: 0, wrong: 0, until: 0, last: 0 };
/// How many places one asker's names fall into.
const PLACES: usize = 16;
/// How long after the last wrong password one is still held against a
/// name: ten minutes.
const FORGET: u64 = 60_000;

/// One asker's counts.
#[derive(Clone, Copy)]
struct Asker {
    uid: u32,
    used: bool,
    tries: [Tries; PLACES],
}

const NO_ASKER: Asker = Asker { uid: 0, used: false, tries: [NO_TRIES; PLACES] };
/// The first is user 0's, for good: `login` is user 0, and no number of
/// users guessing at each other takes the console's place.
static mut ASKERS: [Asker; 32] = [NO_ASKER; 32];

/// A name as a number.
fn hashed(name: &[u8]) -> u32 {
    name.iter().fold(0x811C_9DC5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// What is kept for `asker`, found a place if it has none. `None` if every
/// place is somebody's who has guessed wrong lately — and then this asker
/// is not heard at all, since a wrong guess of its own could not be counted.
fn counts(asker: u32, now: u64) -> Option<&'static mut Asker> {
    let all = unsafe { &mut *core::ptr::addr_of_mut!(ASKERS) };
    if asker == 0 {
        all[0].used = true;
        return Some(&mut all[0]);
    }
    let quiet = |a: &Asker| a.tries.iter().all(|t| t.wrong == 0 || now > t.last + FORGET);
    let at = match all[1..].iter().position(|a| a.used && a.uid == asker) {
        Some(i) => i + 1,
        None => {
            let i = all[1..].iter().position(|a| !a.used || quiet(a))? + 1;
            all[i] = Asker { uid: asker, used: true, tries: [NO_TRIES; PLACES] };
            i
        }
    };
    Some(&mut all[at])
}

/// What a request lent: pieces one after another, each as long as the
/// message said, copied into `out`.
fn lent<'a, const N: usize>(sender: usize, lens: [u64; N], out: &'a mut [u8; 512]) -> Option<[&'a [u8]; N]> {
    let mut total = 0usize;
    for &len in &lens {
        total = total.checked_add(usize::try_from(len).ok()?)?;
    }
    if total > out.len() || (total > 0 && syscall::sys_lent_read(sender, 0, &mut out[..total]) != Ok(total)) {
        return None;
    }
    let out: &'a [u8] = out;
    let mut pieces: [&'a [u8]; N] = [&[]; N];
    let mut at = 0;
    for (piece, &len) in pieces.iter_mut().zip(&lens) {
        *piece = &out[at..at + len as usize];
        at += len as usize;
    }
    Some(pieces)
}

fn answer(tag: u64, code: u64, more: u64) -> Message {
    Message { sender: 0, tag, data: [code, more, 0, 0, 0, 0] }
}

/// Whether `password` opens the account whose hash is `hash`.
///
/// No line in `/etc/shadow`, or an empty hash, is an account nobody has
/// given a password: it asks for none. `!` or `*` is one nobody can log in
/// to with a password at all — and nobody is told that is why. It is
/// checked against a hash no password makes, so that it takes as long as a
/// wrong password, and it is answered as one: "that account is locked",
/// said at a login prompt, is "that account is there".
fn opens(hash: Option<&[u8]>, password: &[u8]) -> Result<(), u64> {
    match hash {
        None | Some(b"") => Ok(()),
        Some(h) if accounts::locked(h) => {
            let _ = crypt::verify(password, NOBODYS);
            Err(ERR_WRONG)
        }
        Some(h) if crypt::verify(password, h) => Ok(()),
        Some(_) => Err(ERR_WRONG),
    }
}

/// Check `password` against `hash`, the hash of the account called `name`,
/// for a caller who is `asker`: with the wait a wrong one earns, and the
/// record of this one.
fn checked(asker: u32, name: &[u8], hash: Option<&[u8]>, password: &[u8]) -> Result<(), Message> {
    let now = syscall::sys_ticks();
    let wait = |ticks: u64| answer(TAG_ERROR, ERR_WAIT, ticks.div_ceil(100));
    let Some(counts) = counts(asker, now) else {
        return Err(wait(100));
    };
    let tag = hashed(name);
    let tries = &mut counts.tries[tag as usize % PLACES];
    if tries.wrong > 0 && now > tries.last + FORGET {
        *tries = NO_TRIES;
    }
    if tries.until > now {
        return Err(wait(tries.until - now));
    }
    match opens(hash, password) {
        Ok(()) => {
            if tries.name == tag {
                *tries = NO_TRIES;
            }
            Ok(())
        }
        Err(code) => {
            if code == ERR_WRONG {
                // After the hashing, which is what took the time.
                let now = syscall::sys_ticks();
                *tries = Tries { name: tag, wrong: tries.wrong + 1, until: 0, last: now };
                // The first two cost nothing — fingers slip. Then one
                // second, two, four ... half a minute at the most.
                if tries.wrong > 2 {
                    tries.until = now + (1u64 << (tries.wrong - 3).min(5)).min(30) * 100;
                }
            }
            Err(answer(TAG_ERROR, code, 0))
        }
    }
}

/// A hash no password makes: what a name nobody has is checked against, so
/// that being told no takes as long, and is counted the same, as it does
/// for a name somebody has.
const NOBODYS: &[u8] =
    b"$6$nobodyhasthis$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz.";

/// Hand `child` one capability, made here and given away.
fn give(child: usize, cap_type: u64, p0: u64, p1: u64) {
    if syscall::sys_cap_mint(SCRATCH, cap_type, p0, p1).is_ok() {
        let _ = syscall::sys_cap_grant_any(child, SCRATCH);
        let _ = syscall::sys_cap_delete(SCRATCH);
    }
}

fn needs(vfs_tid: usize, sender: usize, msg: &Message, text: &mut [u8; 512]) -> Message {
    let Some([user]) = lent(sender, [msg.data[0]], text) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    let passwd = load(vfs_tid, b"passwd", core::ptr::addr_of_mut!(PASSWD_TEXT));
    let shadow = load(vfs_tid, b"shadow", core::ptr::addr_of_mut!(SHADOW_TEXT));
    // An account that is not there is asked for a password like any other:
    // what is typed is refused afterwards, and nothing has been said about
    // which names exist.
    let known = passwd.is_some_and(|p| accounts::user_named(p, user).is_some());
    let asks = !known || !matches!(shadow.and_then(|s| accounts::hash_of(s, user)), None | Some(b""));
    answer(TAG_OK, asks as u64, 0)
}

fn bless(vfs_tid: usize, sender: usize, msg: &Message, text: &mut [u8; 512]) -> Message {
    let child = msg.data[0] as usize;
    let flags = msg.data[3];
    let Some([user, password]) = lent(sender, [msg.data[1], msg.data[2]], text) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    if password.len() > crypt::MAX_PASSWORD {
        return answer(TAG_ERROR, ERR_BAD, 0);
    }
    let Ok((caller_uid, _)) = syscall::sys_get_tuid(sender) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };

    let Some(passwd) = load(vfs_tid, b"passwd", core::ptr::addr_of_mut!(PASSWD_TEXT)) else {
        return answer(TAG_ERROR, ERR_IO, 0);
    };
    let shadow = load(vfs_tid, b"shadow", core::ptr::addr_of_mut!(SHADOW_TEXT));
    let rights = load(vfs_tid, b"rights", core::ptr::addr_of_mut!(RIGHTS_TEXT));
    let Some(target) = accounts::user_named(passwd, user) else {
        // The answer a wrong password gets, after the work a wrong password
        // takes, and counted as one is — where a password would have been
        // asked for at all.
        if caller_uid != 0 || flags & CHECK != 0 {
            if let Err(refusal) = checked(caller_uid, user, Some(NOBODYS), password) {
                return refusal;
            }
        }
        return answer(TAG_ERROR, ERR_WRONG, 0);
    };

    // Whose password is being asked for, if anybody's.
    let asked = if caller_uid == 0 && flags & CHECK == 0 {
        // Root may be anybody, and is asked nothing.
        None
    } else if flags & OWN != 0 {
        let Some(me) = accounts::user_numbered(passwd, caller_uid) else {
            return answer(TAG_ERROR, ERR_NOT_ALLOWED, 0);
        };
        if !accounts::rights_of(rights, me.name, me.uid).has(Rights::BECOME) {
            return answer(TAG_ERROR, ERR_NOT_ALLOWED, 0);
        }
        Some(me)
    } else {
        Some(target)
    };
    if let Some(whose) = asked {
        let hash = shadow.and_then(|s| accounts::hash_of(s, whose.name));
        if let Err(refusal) = checked(caller_uid, whose.name, hash, password) {
            return refusal;
        }
    }

    let mut groups = [0u32; accounts::MAX_GROUPS];
    let n = load(vfs_tid, b"group", core::ptr::addr_of_mut!(GROUP_TEXT))
        .map_or(0, |g| accounts::groups_of(g, target.name, target.gid, &mut groups));
    let may = accounts::rights_of(rights, target.name, target.uid);
    // Somewhere to be, where that is asked for: the account's home. It is
    // this server's to enter, being user 0, and may not be the caller's — a
    // home is its owner's. Entered now, while there is still waiting to be
    // done, and handed over below with the rest.
    let at_home = flags & HOME != 0 && vfs::chdir(vfs_tid, target.home).is_ok();

    // From here until the child has everything, nothing waits. The kernel
    // checks that `child` is the caller's to have blessed at the moment it
    // is, and what is handed over next is handed to the task that check was
    // about: no other task has run in between to take its number.
    if syscall::sys_identify(sender, child, target.uid, target.gid, &groups[..n]).is_err() {
        if at_home {
            let _ = vfs::chdir(vfs_tid, b"/");
        }
        return answer(TAG_ERROR, ERR_NOT_YOURS, 0);
    }
    if may.has(Rights::TASKS) || may.has(Rights::POWER) {
        // Ending every program is part of turning a machine off.
        give(child, syscall::CAP_TYPE_TASK_MGMT, 0, 0);
    }
    if may.has(Rights::POWER) {
        for port in [0x604u64, 0xB004, 0xCF9] {
            give(child, syscall::CAP_TYPE_IOPORT, port, port);
        }
    }
    if may.has(Rights::CLOCK) {
        give(child, syscall::CAP_TYPE_CLOCK, 0, 0);
    }
    if may.has(Rights::ALL) {
        give(child, syscall::CAP_TYPE_PHYS_ALLOC, 64, 0);
        give(child, syscall::CAP_TYPE_SET_UID, 0, 0);
    }
    if at_home {
        // A directory is a descriptor, in the place the kernel keeps for
        // one, and the kernel copies it: still nothing that waits.
        let _ = syscall::sys_fd_dup(child, syscall::FD_CWD, syscall::FD_CWD);
        // And this server out of it again, or it is a program in somebody's
        // home for as long as nobody else logs in. Nothing after this names
        // the child.
        let _ = vfs::chdir(vfs_tid, b"/");
    }
    answer(TAG_OK, target.uid as u64, target.gid as u64)
}

fn passwd(vfs_tid: usize, sender: usize, msg: &Message, text: &mut [u8; 512]) -> Message {
    let Some([user, old, new]) = lent(sender, [msg.data[0], msg.data[1], msg.data[2]], text) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    if new.is_empty() || new.len() > crypt::MAX_PASSWORD || old.len() > crypt::MAX_PASSWORD {
        return answer(TAG_ERROR, ERR_BAD, 0);
    }
    let Ok((caller_uid, _)) = syscall::sys_get_tuid(sender) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    let Some(passwd) = load(vfs_tid, b"passwd", core::ptr::addr_of_mut!(PASSWD_TEXT)) else {
        return answer(TAG_ERROR, ERR_IO, 0);
    };
    let Some(target) = accounts::user_named(passwd, user) else {
        return answer(TAG_ERROR, ERR_NO_USER, 0);
    };
    // One's own, with the old one; anybody's, for root.
    if caller_uid != 0 && caller_uid != target.uid {
        return answer(TAG_ERROR, ERR_NOT_ALLOWED, 0);
    }
    let shadow = load(vfs_tid, b"shadow", core::ptr::addr_of_mut!(SHADOW_TEXT)).unwrap_or(b"");
    if caller_uid != 0 {
        if let Err(refusal) = checked(caller_uid, target.name, accounts::hash_of(shadow, target.name), old) {
            return refusal;
        }
    }

    // The new line: the hash, and the day it was set.
    let mut random = [0u8; 12];
    if syscall::sys_getrandom(&mut random) != Ok(random.len()) {
        return answer(TAG_ERROR, ERR_IO, 0);
    }
    let mut hash = [0u8; crypt::MAX_HASH];
    let Some(hash_len) = crypt::make(new, &random, &mut hash) else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    let mut day = [0u8; 10];
    let day = accounts::digits((syscall::unix_time() / 86400) as u32, &mut day);
    let mut line = [0u8; 256];
    let Some(line) = accounts::line(&[target.name, &hash[..hash_len], day, b"", b"", b"", b"", b"", b""], &mut line)
    else {
        return answer(TAG_ERROR, ERR_BAD, 0);
    };
    let out = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH_TEXT) };
    let Some(len) = accounts::with_record(shadow, target.name, b':', Some(line), out) else {
        return answer(TAG_ERROR, ERR_IO, 0);
    };
    match accounts::write(vfs_tid, b"", b"shadow", &out[..len], 0o600) {
        Ok(()) => answer(TAG_OK, 0, 0),
        Err(_) => answer(TAG_ERROR, ERR_IO, 0),
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    if nameserver::register(NAME).is_err() {
        println!("[auth] There is one already. Exiting.");
        syscall::sys_exit();
    }
    println!("[auth] Registered with nameserver.");
    // The file server starts after this does, and is asked for by name when
    // somebody first needs a file read.
    let mut vfs_tid = 0;
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        let sender = msg.sender;
        // The kernel's notices are not requests, and are not answered.
        if sender == 0 {
            continue;
        }
        if vfs_tid == 0 {
            vfs_tid = nameserver::lookup_retry(b"vfs", 20).unwrap_or(0);
        }
        // What a request lends is read into this, and a password is among
        // it. It is this loop's, so that whichever way a request ends, what
        // was typed is not left lying in the one program everybody's
        // password passes through.
        let mut text = [0u8; 512];
        let reply = match msg.tag {
            quark_rt::ipc::TAG_PING => answer(quark_rt::ipc::TAG_PING, 0, 0),
            _ if vfs_tid == 0 => answer(TAG_ERROR, ERR_IO, 0),
            TAG_NEEDS => needs(vfs_tid, sender, &msg, &mut text),
            TAG_BLESS => bless(vfs_tid, sender, &msg, &mut text),
            TAG_PASSWD => passwd(vfs_tid, sender, &msg, &mut text),
            _ => answer(TAG_ERROR, ERR_BAD, 0),
        };
        text.fill(0);
        // Written through a pointer the compiler cannot see the end of, so
        // that clearing something nothing reads again is not optimised away.
        core::hint::black_box(&mut text);
        let _ = syscall::sys_reply(sender, &reply);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[auth] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
