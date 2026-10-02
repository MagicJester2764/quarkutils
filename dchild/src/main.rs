#![no_std]
#![no_main]

//! The other half of `dtest`'s cross-process check.
//!
//! Started by `dtest` with one end of a socketpair already at descriptor 3.
//! Allocates memory, writes a witness into it, and sends the descriptor back —
//! which is what a Wayland client does with `wl_shm`, minus the drawing.
//!
//! Given `quit` it only exits, for counting how many programs a parent can
//! run; given `orphan` it leaves a dead thread behind for the parent to check
//! on; given `serve` it answers one call with 42, and given `register NAME` it
//! does that under a name. `lookup NAME` calls whatever has that name and exits
//! with the answer. `hold N` opens a file N times and exits without closing
//! any of them, saying how many it got. `echo` answers every call with its
//! tag plus one, until a call whose tag is 0. `cwd` exits 0 if the relative
//! name `passwd` opens: whoever started it gave it `/etc` as its directory.
//! `lock PATH` locks the whole file, says so on descriptor 3, and holds it
//! until that closes; `lock2 PATH` locks byte 1, says so, waits for byte 0,
//! and says so again once it has it. `unlinked PATH` makes a file, removes it
//! while holding it open, and waits for ever: stopping the machine then is a
//! crash with an orphan on the disk. `crashed DIR` leaves that orphan in DIR
//! and beside it a file of a known pattern, synced, and says so; with
//! `writing` after it, it then changes the directory for as long as it is
//! let — files written and cut short, renamed, removed, directories made
//! and taken away — so that the machine is stopped with a change half
//! recorded. `hog` reserves four gigabytes and
//! touches them until something stops it. `mapwrite PATH` maps a file shared,
//! writes into it, and exits without asking for it to be written back.
//! `fault` writes through a null pointer; `sleep` sleeps ten seconds; `late`
//! sleeps a fifth of one and ends with status 3, having answered nobody.
//! `leave` starts a thread that never ends and then ends the program with
//! status 5: descriptor 3, which the thread never closes, has to close.
//! `claim` ends the program with status -11, as if a fault had: a parent
//! has to be told 245, since a negative status is the kernel's to give.
//! `groups N...` exits 0 if the groups it is in are exactly those numbers.
//! `whoami TID` is a client of a server at TID that may say who a task is:
//! it makes a child it never starts, asks about itself and the child, and
//! exits with a bit for each thing it then finds to be so.
//! `user DIR` is run as somebody who is not root, in a directory root laid
//! out, and exits with a bit for each thing it finds as it should be: what
//! it may read, what it may not, and what it may take out of a directory.
//! `userfat DIR NAME` is the same for a filesystem that keeps no owners.
//! `usersys TID` is the rest of what such a user is not allowed, the ending
//! of root's task TID among it. `authuser NAME OTHER OLD NEW` asks the
//! server that says who somebody is to change passwords, its own and
//! OTHER's; `become NAME ID PASSWORD` asks it to make this program NAME
//! with this account's own password, and exits 0 if it is then user ID.
//! `fdclient TID` is a client of a server at TID that serves descriptors: it
//! asks for one, reads and writes through it, copies and closes it, and exits
//! with a bit for each thing that worked.
//! `sigstate` exits with what it was started doing about signals 2, 10 and
//! 12, two bits each: a program a spawner made has said nothing, whatever its
//! spawner had said. `sigignore` ignores signal 15, says so on descriptor 3,
//! and sleeps.
//! `beat` writes a byte to descriptor 3 every twentieth of a second for four
//! seconds: something a parent can see has stopped. `leader` begins a
//! session, takes descriptor 0 as its terminal, says on descriptor 3 how
//! that went, a bit for each thing, and reads the terminal.

use quark_rt::ipc::{Message, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{nameserver, println, sync, syscall, thread, vfs};

quark_rt::manifest!([CapReq::phys_alloc(16)]);

const CONN: usize = 3;
const MINE: usize = 0x97_0000_0000;
const WITNESS: u64 = 0x0D15_EA5E_D15C_0DE5;
/// Where the verdict on the capability test goes, in the memory both halves
/// share. Reported this way rather than over the stream because the stream's
/// message boundaries are what the other half is asserting about.
const VERDICT: usize = MINE + 128;
/// A CSpace slot of this child's own, well clear of anything the manifest
/// filled, to mint into.
const SCRATCH: usize = 8;
/// A slot in somebody else's CSpace to try to fill.
const VICTIM_SLOT: usize = 14;
/// `sys_task_info`'s state for a task that has exited.
const DEAD: u8 = 3;

extern "C" fn quit() -> ! {
    syscall::sys_exit_code(0);
}

/// The requests `dtest` answers as a server of descriptors, and the two
/// cookies it serves.
const ASK_OPEN: u64 = 0x51;
/// What `whoami` asks its server: who am I, and who is this child of mine.
const ASK_WHO: u64 = 0x52;
const ASK_HELD: u64 = 0x52;
const ASK_CHDIR: u64 = 0x53;
const COOKIE_FILE: u64 = 0x5151;
const COOKIE_DIR: u64 = 0x7700_0000_0077;

/// A number an argument spells.
fn number(text: &[u8]) -> usize {
    text.iter().fold(0usize, |n, &d| n.wrapping_mul(10).wrapping_add(d.wrapping_sub(b'0') as usize % 10))
}

/// Ask a server who this is, and who a child of this task's is that was
/// made and never started. A bit of the answer for each thing that is then
/// so: the server answered; this task is user 1234 in group 5678; in groups
/// 42 and 43 besides; the child is user 4321 in group 8765; in group 44.
fn who_am_i(server: usize) -> i32 {
    let made = syscall::sys_task_create().ok();
    let msg = Message { sender: 0, tag: ASK_WHO, data: [made.unwrap_or(0) as u64, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    let mut ok = 0;
    if matches!(syscall::sys_call_timeout(server, &msg, &mut reply, 200), syscall::CallOutcome::Replied)
        && reply.tag == 0
    {
        ok |= 1;
    }
    if syscall::sys_get_uid() == (1234, 5678) {
        ok |= 2;
    }
    let mut groups = [0u32; syscall::MAX_GROUPS];
    if syscall::sys_groups(0, &mut groups) == Ok(2) && groups[..2] == [42, 43] {
        ok |= 4;
    }
    if let Some(child) = made {
        if syscall::sys_get_tuid(child) == Ok((4321, 8765)) {
            ok |= 8;
        }
        if syscall::sys_groups(child, &mut groups) == Ok(1) && groups[0] == 44 {
            ok |= 16;
        }
    }
    ok
}

/// Who `dtest` says a child is before it runs `user` or `usersys`.
const USER: u32 = 4000;
const USER_GROUP: u32 = 4000;
/// And a group it is in besides.
const ALSO_IN: u32 = 4001;

/// A path in `dir`.
fn under<'a>(dir: &[u8], name: &[u8], out: &'a mut [u8; 160]) -> &'a [u8] {
    let n = dir.len().min(120);
    out[..n].copy_from_slice(&dir[..n]);
    out[n] = b'/';
    out[n + 1..n + 1 + name.len()].copy_from_slice(name);
    &out[..n + 1 + name.len()]
}

/// What a user who is not root finds in a directory `dtest` laid out as
/// root. A bit of the answer for each thing that is as it should be.
fn as_user(dir: &[u8]) -> i32 {
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else { return 0 };
    let no = Err(vfs::ERR_PERMISSION);
    let (mut one, mut two) = ([0u8; 160], [0u8; 160]);
    // Whether a path opens with `flags`. What opened is closed again.
    let opens = |path: &[u8], flags: u64| {
        vfs::open_with(v, path, flags).map(|o| {
            let _ = vfs::close(v, o.handle);
        })
    };
    // Whether a file that opens can be written through what opened.
    let writes = |path: &[u8]| {
        vfs::open(v, path).and_then(|(h, _, _)| {
            let wrote = vfs::write(v, h, b"x", 0).map(drop);
            let _ = vfs::close(v, h);
            wrote
        })
    };
    let mode = |path: &[u8], mode: u32| vfs::set_attr(v, path, vfs::ATTR_MODE, mode, 0, 0, 0, 0);
    let mut ok = 0;

    // Root's own file.
    if opens(under(dir, b"secret", &mut one), 0) == no {
        ok |= 1;
    }
    // A file of a group this user is in besides its own: read, not written.
    let shared = under(dir, b"shared", &mut one);
    let mut text = [0u8; 8];
    let read = vfs::open(v, shared).is_ok_and(|(h, _, _)| {
        let got = vfs::read(v, h, &mut text, 0) == Ok(6) && &text[..6] == b"shared";
        let _ = vfs::close(v, h);
        got
    });
    if read && writes(shared) == no && opens(shared, vfs::OPEN_TRUNCATE) == no {
        ok |= 2;
    }
    // A file anybody may read, in a directory only root may change.
    let open = under(dir, b"open", &mut one);
    if opens(open, 0).is_ok()
        && writes(open) == no
        && opens(open, vfs::OPEN_TRUNCATE) == no
        && vfs::unlink(v, open) == no
        && opens(under(dir, b"new", &mut two), vfs::OPEN_CREATE) == no
        && vfs::mkdir(v, under(dir, b"newdir", &mut two)) == no
    {
        ok |= 4;
    }
    // A directory anybody may make files in, and only a file's owner take
    // them out of.
    let mut three = [0u8; 160];
    let roots = under(dir, b"sticky/roots", &mut one);
    let mine = under(dir, b"sticky/mine", &mut two);
    let made = opens(mine, vfs::OPEN_CREATE).is_ok();
    if made
        && vfs::unlink(v, roots) == no
        && vfs::rename(v, roots, under(dir, b"sticky/taken", &mut three)) == no
        && vfs::rename(v, mine, roots) == no
        && vfs::rmdir(v, under(dir, b"sticky/rootsdir", &mut three)) == no
    {
        ok |= 8;
    }
    if made
        && vfs::rename(v, mine, under(dir, b"sticky/moved", &mut three)).is_ok()
        && vfs::unlink(v, under(dir, b"sticky/moved", &mut three)).is_ok()
    {
        ok |= 16;
    }
    // A directory of root's own: not looked in, and nothing in it reached.
    if opens(under(dir, b"private", &mut one), 0) == no && opens(under(dir, b"private/inside", &mut two), 0) == no {
        ok |= 32;
    }
    // A file's mode is its owner's to change; whose it is, is root's.
    let kept = under(dir, b"sticky/kept", &mut two);
    let own = opens(kept, vfs::OPEN_CREATE).is_ok();
    if own
        && mode(kept, 0o600).is_ok()
        && vfs::lstat(v, kept).is_ok_and(|st| st.mode & 0o7777 == 0o600 && st.uid == USER && st.gid == USER_GROUP)
        && vfs::set_attr(v, kept, vfs::ATTR_GID, 0, 0, ALSO_IN, 0, 0).is_ok()
        && vfs::set_attr(v, kept, vfs::ATTR_UID, 0, 0, 0, 0, 0) == no
        && vfs::set_attr(v, kept, vfs::ATTR_GID, 0, 0, 0, 0, 0) == no
        && mode(under(dir, b"open", &mut one), 0o666) == no
    {
        ok |= 64;
    }
    if own {
        let _ = vfs::unlink(v, kept);
    }
    ok
}

/// A file `name` in `dir`, on a filesystem that keeps no owners, as a user.
/// A bit for its being read, and one for nothing about it being changed.
fn user_fat(dir: &[u8], name: &[u8]) -> i32 {
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else { return 0 };
    let no = Err(vfs::ERR_PERMISSION);
    let (mut one, mut two) = ([0u8; 160], [0u8; 160]);
    let file = under(dir, name, &mut one);
    let Ok((h, _, _)) = vfs::open(v, file) else { return 0 };
    let mut text = [0u8; 64];
    let mut ok = 0;
    if vfs::read(v, h, &mut text, 0).is_ok_and(|n| n > 0) {
        ok |= 1;
    }
    let written = vfs::write(v, h, b"x", 0).map(drop);
    let cut = vfs::truncate(v, h, 0);
    let _ = vfs::close(v, h);
    if written == no
        && cut == no
        && vfs::open_with(v, file, vfs::OPEN_TRUNCATE).map(drop) == no
        && vfs::unlink(v, file) == no
        && vfs::rename(v, file, under(dir, b"MOVED.BIN", &mut two)) == no
        && vfs::open_with(v, under(dir, b"NEW.TXT", &mut two), vfs::OPEN_CREATE).map(drop) == no
        && vfs::mkdir(v, under(dir, b"NEWDIR", &mut two)) == no
    {
        ok |= 2;
    }
    ok
}

/// What else a user is not allowed. A bit for each thing that is so.
fn user_sys(victim: usize) -> i32 {
    let me = syscall::sys_getpid() as usize;
    let mut ok = 0;
    let mut groups = [0u32; syscall::MAX_GROUPS];
    if syscall::sys_get_uid() == (USER, USER_GROUP) && syscall::sys_groups(0, &mut groups) == Ok(1) && groups[0] == ALSO_IN
    {
        ok |= 1;
    }
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else { return ok };
    // The passwords.
    if vfs::open(v, b"/etc/shadow").err() == Some(vfs::ERR_PERMISSION) {
        ok |= 2;
    }
    // Who it is, is not its own to say.
    if syscall::sys_set_uid(me, 0).is_err()
        && syscall::sys_set_gid(me, 0).is_err()
        && syscall::sys_set_groups(0, &[0]).is_err()
        && syscall::sys_get_uid() == (USER, USER_GROUP)
    {
        ok |= 4;
    }
    // Somebody else's program is not its to end.
    if syscall::sys_task_kill(victim).is_err()
        && matches!(syscall::sys_task_info(victim), Ok((state, _, _)) if state != DEAD)
    {
        ok |= 8;
    }
    // What it makes is its own.
    let path: &[u8] = b"/tmp/dtest-user-file";
    let _ = vfs::unlink(v, path);
    if vfs::open_with(v, path, vfs::OPEN_CREATE).map(|o| vfs::close(v, o.handle)).is_ok()
        && vfs::lstat(v, path).is_ok_and(|st| st.uid == USER && st.gid == USER_GROUP)
        && vfs::unlink(v, path).is_ok()
    {
        ok |= 16;
    }
    // And the account files are not its to change.
    if vfs::open_with(v, b"/etc/passwd", vfs::OPEN_TRUNCATE).err() == Some(vfs::ERR_PERMISSION)
        && vfs::unlink(v, b"/etc/passwd") == Err(vfs::ERR_PERMISSION)
        && vfs::open_with(v, b"/etc/dtest-made", vfs::OPEN_CREATE).err() == Some(vfs::ERR_PERMISSION)
    {
        ok |= 32;
    }
    ok
}

/// Passwords, asked for as an ordinary user. A bit for each thing that is
/// as it should be: another's is not this one's to set, its own is not set
/// by somebody who does not know it, and is by somebody who does.
fn auth_user(name: &[u8], other: &[u8], old: &[u8], new: &[u8]) -> i32 {
    use quark_rt::auth;
    let mut ok = 0;
    if auth::passwd(other, b"", b"not for this user to set") == Err(auth::ERR_NOT_ALLOWED) {
        ok |= 1;
    }
    if auth::passwd(name, b"not the old one", new) == Err(auth::ERR_WRONG) {
        ok |= 2;
    }
    if auth::passwd(name, old, new).is_ok() {
        ok |= 4;
    }
    ok
}

/// Everything a client does with a descriptor a server gave it. One bit of
/// the answer for each thing that came out right.
fn fd_client(server: usize) -> i32 {
    let me = syscall::sys_getpid() as usize;
    let ask = |tag, word| {
        let msg = Message { sender: 0, tag, data: [word, 0, 0, 0, 0, 0] };
        let mut reply = Message::empty();
        match syscall::sys_call_timeout(server, &msg, &mut reply, 200) {
            syscall::CallOutcome::Replied if reply.tag == 0 => Some(reply.data[0]),
            _ => None,
        }
    };
    let mut ok = 0;

    let Some(fd) = ask(ASK_OPEN, 0) else { return 0 };
    let fd = fd as usize;
    if fd >= 3 && syscall::sys_fd_served(fd) == Ok((server, COOKIE_FILE)) {
        ok |= 1;
    }
    // Through the kernel: this program has no idea what is behind the number.
    if syscall::sys_fd_write(fd, b"hello") == 5 {
        ok |= 2;
    }
    let mut buf = [0u8; 8];
    if syscall::sys_fd_read(fd, &mut buf) == 5 && &buf[..5] == b"world" {
        ok |= 4;
    }
    // A forked child has a descriptor of its own for the same object.
    match syscall::sys_fork() {
        Ok(0) => {
            let same = syscall::sys_fd_served(fd) == Ok((server, COOKIE_FILE));
            let _ = syscall::sys_fd_close(fd);
            syscall::sys_exit_program(if same { 0 } else { 1 });
        }
        Ok(child) => {
            let mut status = None;
            while status.is_none() {
                match syscall::sys_wait() {
                    Ok((t, code)) if t == child => status = Some(code),
                    Ok(_) => {}
                    Err(()) => status = Some(-1),
                }
            }
            // The child closed its own; this one is untouched.
            if status == Some(0) && syscall::sys_fd_served(fd).is_ok() {
                ok |= 8;
            }
        }
        Err(()) => {}
    }
    // A copy keeps the object after the first descriptor closes...
    if syscall::sys_fd_dup(me, 20, fd).is_ok()
        && syscall::sys_fd_close(fd).is_ok()
        && ask(ASK_HELD, COOKIE_FILE) == Some(1)
    {
        ok |= 16;
    }
    // ...and the last close is the end of it.
    if syscall::sys_fd_close(20).is_ok() && ask(ASK_HELD, COOKIE_FILE) == Some(0) {
        ok |= 32;
    }
    // A working directory is a descriptor at a number of its own.
    if ask(ASK_CHDIR, 0).is_some()
        && syscall::sys_fd_served(syscall::FD_CWD) == Ok((server, COOKIE_DIR))
    {
        ok |= 64;
    }
    // Left open on purpose: the server hears about it when this program ends.
    ok
}

/// A thread that is never going to finish by itself.
extern "C" fn linger() -> ! {
    loop {
        syscall::sleep_ticks(1000);
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // Asked only to run: the parent is counting how many programs it can
    // start, not talking to this one.
    if quark_rt::args::argv(1) == Some(&b"quit"[..]) {
        syscall::sys_exit_code(0);
    }
    // Leave a thread behind, dead but never collected, and say which one: the
    // parent checks that collecting this program takes the thread with it.
    if quark_rt::args::argv(1) == Some(&b"orphan"[..]) {
        let Ok(t) = thread::spawn_with_stack(quit, 1) else {
            syscall::sys_exit_code(-1);
        };
        // sys_wait would reap it, so watch for it to die instead.
        while syscall::sys_task_info(t.tid()).map(|(state, _, _)| state) != Ok(DEAD) {
            syscall::sleep_ticks(1);
        }
        syscall::sys_exit_code(t.tid() as i32);
    }

    if let Some(mode @ (b"lock" | b"lock2")) = quark_rt::args::argv(1) {
        let path = quark_rt::args::argv(2).unwrap_or(b"");
        let held = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs| {
            let (h, _, _) = vfs::open(vfs, path).ok()?;
            if mode == b"lock" {
                vfs::lock(vfs, h, vfs::LOCK_EXCLUSIVE, 0, 0, vfs::LOCK_WAIT).ok()?;
                let _ = syscall::sys_fd_write(CONN, b"L");
            } else {
                vfs::lock(vfs, h, vfs::LOCK_EXCLUSIVE, 1, 1, 0).ok()?;
                let _ = syscall::sys_fd_write(CONN, b"1");
                vfs::lock(vfs, h, vfs::LOCK_EXCLUSIVE, 0, 1, vfs::LOCK_WAIT).ok()?;
                let _ = syscall::sys_fd_write(CONN, b"2");
            }
            Some(())
        });
        // Held until the parent lets go of its end.
        let mut buf = [0u8; 1];
        while held.is_some() && matches!(syscall::sys_fd_read(CONN, &mut buf), 1..=0xFFFF) {}
        syscall::sys_exit_code(if held.is_some() { 0 } else { 1 });
    }
    if quark_rt::args::argv(1) == Some(&b"hog"[..]) {
        const HOG: usize = 0xB0_0000_0000;
        if syscall::sys_map_anon(HOG, 1 << 20, false).is_err() {
            syscall::sys_exit_code(1);
        }
        for page in 0..1usize << 20 {
            unsafe { core::ptr::write_volatile((HOG + page * 4096) as *mut u8, 1) };
        }
        // Four gigabytes, and nobody stopped it.
        syscall::sys_exit_code(2);
    }
    if quark_rt::args::argv(1) == Some(&b"fdclient"[..]) {
        let server = quark_rt::args::argv(2).map_or(0, |n| {
            n.iter().fold(0usize, |acc, &d| acc * 10 + (d.wrapping_sub(b'0') as usize % 10))
        });
        syscall::sys_exit_program(fd_client(server));
    }
    if quark_rt::args::argv(1) == Some(&b"leave"[..]) {
        if thread::spawn_with_stack(linger, 1).is_err() {
            syscall::sys_exit_code(1);
        }
        // Long enough for it to be asleep, so that what ends it is this
        // program ending and not a race it happened to lose.
        syscall::sleep_ticks(5);
        syscall::sys_exit_program(5);
    }
    if quark_rt::args::argv(1) == Some(&b"groups"[..]) {
        let mut groups = [0u32; syscall::MAX_GROUPS];
        let n = syscall::sys_groups(0, &mut groups).unwrap_or(usize::MAX);
        let wanted = quark_rt::args::argc().saturating_sub(2);
        let same = n == wanted
            && (0..wanted).all(|i| quark_rt::args::argv(i + 2).is_some_and(|a| number(a) as u32 == groups[i]));
        syscall::sys_exit_program(if same { 0 } else { 1 + n.min(100) as i32 });
    }
    if quark_rt::args::argv(1) == Some(&b"whoami"[..]) {
        let server = quark_rt::args::argv(2).map_or(0, number);
        syscall::sys_exit_program(who_am_i(server));
    }

    if quark_rt::args::argv(1) == Some(&b"user"[..]) {
        syscall::sys_exit_program(as_user(quark_rt::args::argv(2).unwrap_or(b"")));
    }
    if quark_rt::args::argv(1) == Some(&b"userfat"[..]) {
        let arg = |i| quark_rt::args::argv(i).unwrap_or(b"");
        syscall::sys_exit_program(user_fat(arg(2), arg(3)));
    }
    if quark_rt::args::argv(1) == Some(&b"usersys"[..]) {
        syscall::sys_exit_program(user_sys(quark_rt::args::argv(2).map_or(0, number)));
    }
    if quark_rt::args::argv(1) == Some(&b"authuser"[..]) {
        let arg = |i| quark_rt::args::argv(i).unwrap_or(b"");
        syscall::sys_exit_program(auth_user(arg(2), arg(3), arg(4), arg(5)));
    }
    if quark_rt::args::argv(1) == Some(&b"become"[..]) {
        let arg = |i| quark_rt::args::argv(i).unwrap_or(b"");
        let me = syscall::sys_getpid() as usize;
        let status = match quark_rt::auth::bless(me, arg(2), arg(4), quark_rt::auth::OWN) {
            Ok(_) if syscall::sys_get_uid().0 as usize == number(arg(3)) => 0,
            Ok(_) => 1,
            Err(code) => 100 + code as i32,
        };
        syscall::sys_exit_program(status);
    }
    if quark_rt::args::argv(1) == Some(&b"claim"[..]) {
        syscall::sys_exit_program(-11);
    }
    if quark_rt::args::argv(1) == Some(&b"fault"[..]) {
        unsafe { core::ptr::write_volatile(core::hint::black_box(0usize) as *mut u8, 1) };
        syscall::sys_exit_code(0);
    }
    if quark_rt::args::argv(1) == Some(&b"late"[..]) {
        syscall::sleep_ticks(20);
        syscall::sys_exit_code(3);
    }
    if quark_rt::args::argv(1) == Some(&b"sleep"[..]) {
        syscall::sleep_ticks(1000);
        syscall::sys_exit_code(0);
    }
    if quark_rt::args::argv(1) == Some(&b"beat"[..]) {
        for _ in 0..80 {
            if syscall::sys_fd_write(CONN, b".") != 1 {
                break;
            }
            syscall::sleep_ticks(5);
        }
        syscall::sys_exit_code(7);
    }
    if quark_rt::args::argv(1) == Some(&b"leader"[..]) {
        let me = syscall::sys_pid_self();
        let mut went = 0u8;
        if syscall::sys_setsid() == Ok(me) {
            went |= 1;
        }
        // Once: a process that leads a group does not begin another session.
        if syscall::sys_setsid() == Err(syscall::Refused::NotAllowed) {
            went |= 2;
        }
        if syscall::sys_getsid(0) == Some(me) && syscall::sys_getpgid(0) == Some(me) {
            went |= 4;
        }
        if syscall::sys_pty_set_session(0).is_ok() {
            went |= 8;
        }
        if syscall::sys_pty_front(0) == Some(me) && syscall::sys_pty_session(0) == Some(me) {
            went |= 16;
        }
        // The signal Ctrl-Z raises. This group has nobody to start it again
        // — its parent is in another session — so it is not stopped, and
        // gets as far as saying so.
        let _ = syscall::sys_sig_raise(syscall::sys_getpid() as usize, syscall::SIGTSTP);
        went |= 32;
        let _ = syscall::sys_fd_write(CONN, &[went]);
        let mut line = [0u8; 8];
        let n = syscall::sys_fd_read(0, &mut line);
        syscall::sys_exit_code(n as i32);
    }
    if quark_rt::args::argv(1) == Some(&b"sigstate"[..]) {
        let said = |signo| syscall::sys_sig_action_get(signo).unwrap_or(3) as i32;
        syscall::sys_exit_code(said(2) | said(10) << 2 | said(12) << 4);
    }
    if quark_rt::args::argv(1) == Some(&b"sigignore"[..]) {
        let _ = syscall::sys_sig_action(syscall::SIGTERM, syscall::SIG_IGNORE);
        let _ = syscall::sys_fd_write(CONN, b"i");
        syscall::sleep_ticks(1000);
        syscall::sys_exit_code(0);
    }
    if quark_rt::args::argv(1) == Some(&b"mapwrite"[..]) {
        const AT: usize = 0xB4_0000_0000;
        let path = quark_rt::args::argv(2).unwrap_or(b"");
        let mapped = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs| {
            let o = vfs::open_with(vfs, path, 0).ok()?;
            let (slot, _) = vfs::map(vfs, o.handle, true).ok()?;
            let flags = syscall::OBJECT_MAP_WRITE | syscall::OBJECT_MAP_SHARED;
            let made = syscall::sys_object_map(slot, AT, 1, 0, flags);
            let _ = syscall::sys_cap_delete(slot);
            let _ = vfs::close(vfs, o.handle);
            made.ok()
        });
        if mapped.is_none() {
            syscall::sys_exit_code(1);
        }
        let text = b"from the child";
        for (i, &b) in text.iter().enumerate() {
            unsafe { core::ptr::write_volatile((AT + i) as *mut u8, b) };
        }
        syscall::sys_exit_code(0);
    }
    if quark_rt::args::argv(1) == Some(&b"unlinked"[..]) {
        let path = quark_rt::args::argv(2).unwrap_or(b"");
        let held = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs| {
            let o = vfs::open_with(vfs, path, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE).ok()?;
            let data = [0x5Au8; 1000];
            for i in 0..5 {
                vfs::write(vfs, o.handle, &data, i * 1000).ok()?;
            }
            vfs::unlink(vfs, path).ok()?;
            Some(o.handle)
        });
        match held {
            Some(_) => println!("holding {}", core::str::from_utf8(path).unwrap_or("?")),
            None => syscall::sys_exit_code(1),
        }
        loop {
            let mut msg = Message::empty();
            let _ = syscall::sys_recv(TID_ANY, &mut msg);
        }
    }
    if quark_rt::args::argv(1) == Some(&b"crashed"[..]) {
        crashed(
            quark_rt::args::argv(2).unwrap_or(b"/tmp"),
            quark_rt::args::argv(3) == Some(&b"writing"[..]),
        );
    }
    if quark_rt::args::argv(1) == Some(&b"cwd"[..]) {
        let found = nameserver::lookup_retry(b"vfs", 20).is_some_and(|vfs| {
            vfs::open(vfs, b"passwd").map(|(h, _, _)| vfs::close(vfs, h)).is_ok()
        });
        syscall::sys_exit_code(if found { 0 } else { 1 });
    }

    // Answer one call, whoever makes it, with 42: something for the parent to
    // reach, or to fail to reach.
    if quark_rt::args::argv(1) == Some(&b"serve"[..]) {
        serve_once();
    }
    if quark_rt::args::argv(1) == Some(&b"register"[..]) {
        let name = quark_rt::args::argv(2).unwrap_or(b"");
        if nameserver::register(name).is_err() {
            syscall::sys_exit_code(2);
        }
        serve_once();
    }
    if quark_rt::args::argv(1) == Some(&b"hold"[..]) {
        let want = quark_rt::args::argv(2).map_or(0, |n| {
            n.iter().fold(0usize, |acc, &d| acc * 10 + (d.wrapping_sub(b'0') as usize % 10))
        });
        let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
            syscall::sys_exit_code(-1);
        };
        let mut held = 0;
        for _ in 0..want {
            if vfs::open(vfs_tid, b"/etc/passwd").is_ok() {
                held += 1;
            }
        }
        syscall::sys_exit_code(held);
    }
    // Answer call after call, for a parent making a great many of them.
    if quark_rt::args::argv(1) == Some(&b"echo"[..]) {
        loop {
            let mut msg = Message::empty();
            if syscall::sys_recv_timeout(TID_ANY, &mut msg, 500).is_err() {
                syscall::sys_exit_code(1);
            }
            let answer = Message { sender: 0, tag: msg.tag.wrapping_add(1), data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &answer);
            if msg.tag == 0 {
                syscall::sys_exit_code(0);
            }
        }
    }
    // Reach a service by name alone: nothing but the lookup gives this the
    // right to call it.
    if quark_rt::args::argv(1) == Some(&b"lookup"[..]) {
        let name = quark_rt::args::argv(2).unwrap_or(b"");
        let Some(tid) = nameserver::lookup(name) else {
            syscall::sys_exit_code(2);
        };
        let mut reply = Message::empty();
        match syscall::sys_call_timeout(tid, &Message::empty(), &mut reply, 100) {
            syscall::CallOutcome::Replied => syscall::sys_exit_code(reply.tag as i32),
            _ => syscall::sys_exit_code(3),
        }
    }

    // Wait for the parent's byte before answering, so this proves the stream
    // carries data in both directions between address spaces.
    let mut buf = [0u8; 8];
    let n = syscall::sys_fd_read(CONN, &mut buf);
    if n != 4 || &buf[..4] != b"go!\n" {
        println!("[dchild] bad greeting: {} bytes", n);
        syscall::sys_exit_code(2);
    }

    let Ok(mem) = syscall::sys_memfd_create(2) else {
        println!("[dchild] no memory");
        syscall::sys_exit_code(3);
    };
    if syscall::sys_mmap_fd(mem, MINE).is_err() {
        println!("[dchild] cannot map my own memory");
        syscall::sys_exit_code(4);
    }
    unsafe { core::ptr::write_volatile(MINE as *mut u64, WITNESS) };

    // Can this child push a capability into a task it has no authority over?
    //
    // It holds no TaskMgmt at all — its manifest asks for phys_alloc and
    // nothing else — and its parent is not calling it, so the answer must be
    // no. A grant can never *raise* anyone's authority, since it only ever
    // adds; what it can do is fill every slot, and a service that can no
    // longer be handed a capability can no longer be handed the display.
    //
    // An Endpoint to itself is a capability any task may mint, which is what
    // makes this test about the grant rather than about the mint.
    let me = syscall::sys_getpid() as usize;
    let parent = syscall::sys_task_info(me).map(|(_, p, _)| p).unwrap_or(0);
    let minted = syscall::sys_cap_mint(SCRATCH, syscall::CAP_TYPE_ENDPOINT, me as u64, 0).is_ok();
    let refused = syscall::sys_cap_grant(parent, SCRATCH, VICTIM_SLOT).is_err();
    unsafe {
        core::ptr::write_volatile(VERDICT as *mut u64, (minted && refused) as u64);
    }

    if syscall::sys_fd_send(CONN, b"here", Some(mem)) != Ok(4) {
        println!("[dchild] send failed");
        syscall::sys_exit_code(5);
    }

    // A lock in memory the two of us share. The parent holds it when this
    // arrives, so acquiring it means blocking in one address space and being
    // woken from another — which works because the kernel keys its wait queue
    // on the physical address of the word, not the virtual one.
    let shared = unsafe { &*((MINE + 64) as *const sync::Mutex<u64>) };
    {
        let mut held = shared.lock();
        *held += 1;
    }
    println!("[dchild] sent, and took the shared lock");
    syscall::sys_exit_code(0);
}

/// Page `n` of the file a crash test leaves: every byte says which page it
/// is in and where.
fn crash_page(page: &mut [u8; 4096], n: u32) {
    for (i, b) in page.iter_mut().enumerate() {
        *b = (n as usize * 131 + i * 7 + 3) as u8;
    }
}

/// What a machine is stopped in the middle of: see the top of the file.
fn crashed(dir: &[u8], writing: bool) -> ! {
    fn path<'a>(buf: &'a mut [u8; 128], dir: &[u8], name: &[u8]) -> &'a [u8] {
        let d = dir.len().min(100);
        buf[..d].copy_from_slice(&dir[..d]);
        buf[d..d + name.len()].copy_from_slice(name);
        &buf[..d + name.len()]
    }
    let Some(vfs) = nameserver::lookup_retry(b"vfs", 20) else {
        syscall::sys_exit_code(1);
    };
    let (mut a, mut b, mut c) = ([0u8; 128], [0u8; 128], [0u8; 128]);
    let orphan = path(&mut a, dir, b"/crash-orphan");
    let synced = path(&mut b, dir, b"/crash-synced");
    let unsynced = path(&mut c, dir, b"/crash-unsynced");
    let (mut d, mut e) = ([0u8; 128], [0u8; 128]);
    let renamed = path(&mut d, dir, b"/crash-renamed");
    let made = path(&mut e, dir, b"/crash-directory");
    let mut page = [0u8; 4096];

    let left = (|| {
        // Held open and removed: the handle is never closed.
        let o = vfs::open_with(vfs, orphan, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE).ok()?;
        let data = [0x5Au8; 1000];
        for i in 0..5 {
            vfs::write(vfs, o.handle, &data, i * 1000).ok()?;
        }
        vfs::unlink(vfs, orphan).ok()?;
        // Written, and waited for.
        let o = vfs::open_with(vfs, synced, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE).ok()?;
        for n in 0..75u32 {
            crash_page(&mut page, n);
            vfs::write(vfs, o.handle, &page, n * 4096).ok()?;
        }
        vfs::sync(vfs).ok()?;
        Some(())
    })();
    if left.is_none() {
        println!("dchild: could not leave the files in {}", core::str::from_utf8(dir).unwrap_or("?"));
        syscall::sys_exit_code(1);
    }
    if !writing {
        println!("left {}", core::str::from_utf8(dir).unwrap_or("?"));
        loop {
            let mut msg = Message::empty();
            let _ = syscall::sys_recv(TID_ANY, &mut msg);
        }
    }
    println!("writing {}", core::str::from_utf8(dir).unwrap_or("?"));
    // And changed without waiting, for ever, in turns that make every way
    // a change is recorded happen: a few pages and then something that
    // commits them; seventy, which is more than one transaction takes; a
    // few and a pause, which commits them by itself. Most turns are short,
    // because what a stop is looking for is the moment a transaction is in
    // the journal and not yet where it belongs, and short turns are mostly
    // that moment.
    const PAGES: [u32; 12] = [2, 1, 3, 2, 70, 1, 2, 4, 1, 3, 2, 1];
    for turn in 0usize.. {
        let Ok(o) = vfs::open_with(vfs, unsynced, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE) else {
            syscall::sys_exit_code(1);
        };
        for n in 0..PAGES[turn % PAGES.len()] {
            crash_page(&mut page, n);
            if vfs::write(vfs, o.handle, &page, n * 4096).is_err() {
                syscall::sys_exit_code(1);
            }
        }
        let _ = vfs::close(vfs, o.handle);
        match turn % 6 {
            // A name moved and a file removed: an inode freed, and made
            // again by the next turn's open.
            1 => {
                let _ = vfs::rename(vfs, unsynced, renamed);
                let _ = vfs::unlink(vfs, renamed);
            }
            3 => {
                let _ = vfs::mkdir(vfs, made);
                let _ = vfs::rmdir(vfs, made);
            }
            // Nothing asked of the file server for a while.
            4 => syscall::sleep_ms(40),
            _ => {}
        }
    }
    syscall::sys_exit_code(0);
}

/// Answer one call, from anybody, with 42, and exit. Waits five seconds at
/// most, so a parent whose caller never came is not left waiting for good.
fn serve_once() -> ! {
    let mut msg = Message::empty();
    if syscall::sys_recv_timeout(TID_ANY, &mut msg, 500).is_err() {
        syscall::sys_exit_code(1);
    }
    let _ = syscall::sys_reply(msg.sender, &Message { sender: 0, tag: 42, data: [0; 6] });
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[dchild] PANIC: {}", info);
    syscall::sys_exit_code(255);
}
