#![no_std]
#![no_main]

//! Acceptance checks for the kernel and the runtime, from Phase 10 on:
//! descriptors, streams, waiting, the environment, memory and authority.
//!
//! There is no test framework here, so this is one: a program that asserts and
//! exits non-zero. Run it from the shell; `dtest NAME` runs one section, named
//! in the table in `_start`.

use quark_rt::manifest::CapReq;
use quark_rt::wl::wire;
use quark_rt::{nameserver, print, println, spawn, sync, syscall, thread, vfs};

quark_rt::manifest!([CapReq::task_mgmt(0), CapReq::phys_alloc(64)]);

static mut PASSED: u32 = 0;
static mut FAILED: u32 = 0;

/// What failed, kept for the end.
///
/// Two hundred and sixty-odd checks are four screens of text and a console
/// that does not scroll back, so a run that says "three failed" and nothing
/// else is a run somebody has to repeat section by section to read. The names
/// are `&'static str`, so remembering them costs a pointer each.
const RECAP: usize = 16;
static mut FAILURES: [&str; RECAP] = [""; RECAP];

fn check(what: &'static str, ok: bool) {
    unsafe {
        if ok {
            PASSED += 1;
            println!("  ok    {}", what);
        } else {
            if (FAILED as usize) < RECAP {
                FAILURES[FAILED as usize] = what;
            }
            FAILED += 1;
            println!("  FAIL  {}", what);
        }
    }
}

/// A pipe wired to two of our own descriptors, for tests that need a
/// descriptor that behaves like something.
fn own_pipe(read_fd: usize, write_fd: usize) -> Result<(), ()> {
    let me = syscall::sys_getpid() as usize;
    let h = syscall::sys_pipe_create()?;
    syscall::sys_pipe_fd_set(me, read_fd, h, false)?;
    syscall::sys_pipe_fd_set(me, write_fd, h, true)?;
    Ok(())
}

/// A capability over more physical memory than this is not one device's: a
/// framebuffer is a few megabytes, a boot module one. The grants Phase 12
/// removes were four gigabytes.
const DEVICE_SPAN: u64 = 64 << 20;
/// Where the kernel is loaded, which no task may map.
const KERNEL_IMAGE: u64 = 0x10_0000;

fn test_physical_authority() {
    println!("physical memory authority:");
    let mut seen = 0;
    let mut broad = 0;
    let mut kernel = 0;
    for tid in 1..64 {
        if syscall::sys_task_info(tid).is_err() {
            continue;
        }
        for slot in 0.. {
            let Ok(cap) = syscall::sys_cap_read(tid, slot) else { break };
            if cap.cap_type != syscall::CAP_TYPE_PHYS_RANGE || !cap.valid {
                continue;
            }
            seen += 1;
            // A disk made of memory is a device whose memory is as big as
            // the disk: a system running from one holds its whole root that
            // way. What it may map is the size of what it serves, and no
            // more.
            let span = cap.param1.saturating_sub(cap.param0);
            let is_ram_disk = nameserver::lookup(b"ram0") == Some(tid)
                && quark_rt::block::info(tid, 0).is_ok_and(|i| (i.sectors * 512).div_ceil(4096) * 4096 == span);
            if span > DEVICE_SPAN && !is_ram_disk {
                println!("    tid {} may map {:#x}..{:#x}", tid, cap.param0, cap.param1);
                broad += 1;
            }
            if cap.param0 <= KERNEL_IMAGE && KERNEL_IMAGE < cap.param1 {
                kernel += 1;
            }
        }
    }
    check("another task's capabilities can be read", seen > 0);
    check("no task may map more than one device's memory", broad == 0);
    check("no task may map the kernel", kernel == 0);
}

fn test_close() {
    println!("close:");
    if own_pipe(3, 4).is_err() {
        check("pipe wired to fd 3 and 4", false);
        return;
    }
    check("pipe wired to fd 3 and 4", true);

    // Both return a count, or u64::MAX; there is no Result on this path.
    let mut buf = [0u8; 8];
    check("write to the write end", syscall::sys_fd_write(4, b"hi") == 2);
    check(
        "read gets the bytes back",
        syscall::sys_fd_read(3, &mut buf) == 2 && &buf[..2] == b"hi",
    );

    // Closing the last writer is what turns a read into EOF. Without a close
    // call there is no way to say so.
    check("close the write end", syscall::sys_fd_close(4).is_ok());
    check("read now reports EOF", syscall::sys_fd_read(3, &mut buf) == 0);
    check("close the read end", syscall::sys_fd_close(3).is_ok());
    check(
        "closing an empty descriptor fails",
        syscall::sys_fd_close(3).is_err(),
    );
}

fn test_fd_table() {
    println!("descriptor table:");
    // Eight pipes is the per-task limit, which gives sixteen ends — enough to
    // prove the table is deeper than the eight entries it used to have.
    let mut wired = 0;
    for i in 0..8 {
        let r = 3 + i * 2;
        let w = 4 + i * 2;
        if r >= 32 || w >= 32 || own_pipe(r, w).is_err() {
            break;
        }
        wired += 1;
    }
    check("wired eight pipes into sixteen descriptors", wired == 8);

    // The highest of them must actually work, not merely be accepted.
    let mut buf = [0u8; 8];
    check("write to fd 18", syscall::sys_fd_write(18, b"deep") == 4);
    check(
        "read from fd 17",
        syscall::sys_fd_read(17, &mut buf) == 4 && &buf[..4] == b"deep",
    );

    for i in 0..wired {
        let _ = syscall::sys_fd_close(3 + i * 2);
        let _ = syscall::sys_fd_close(4 + i * 2);
    }
}

static SHARE_GO: sync::Semaphore = sync::Semaphore::new(0);
static SHARE_DONE: sync::Semaphore = sync::Semaphore::new(0);
static SHARE_SAW: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// A thread started before the pipe it reads was made.
extern "C" fn sharer() -> ! {
    use core::sync::atomic::Ordering;
    SHARE_GO.acquire();
    let mut saw = 0;
    let mut buf = [0u8; 8];
    if syscall::sys_fd_read(3, &mut buf) == 2 && &buf[..2] == b"ab" {
        saw |= 1;
    }
    // The only write end there is. If this closes it for the program, the
    // creator's next read is the end of the pipe.
    if syscall::sys_fd_close(4).is_ok() {
        saw |= 2;
    }
    SHARE_SAW.store(saw, Ordering::SeqCst);
    SHARE_DONE.release();
    syscall::sys_exit_code(0);
}

/// A descriptor is its program's: one table for every thread, a copy of it
/// for a forked child, and gone when the program is.
fn test_program_table() {
    use core::sync::atomic::Ordering;
    println!("a descriptor belongs to a program:");

    let Ok(t) = thread::spawn_with_stack(sharer, 8) else {
        check("started a thread", false);
        return;
    };
    // Made after the thread, which used to start on a copy of this table and
    // see nothing added to it since.
    if own_pipe(3, 4).is_err() {
        check("pipe wired to fd 3 and 4", false);
        return;
    }
    let _ = syscall::sys_fd_write(4, b"ab");
    SHARE_GO.release();
    SHARE_DONE.acquire();
    let saw = SHARE_SAW.load(Ordering::SeqCst);
    check("a thread reads a descriptor made after it started", saw & 1 != 0);
    check("a thread closes one", saw & 2 != 0);
    let mut buf = [0u8; 8];
    check(
        "and it is closed for the program: the pipe has ended",
        syscall::sys_fd_read(3, &mut buf) == 0,
    );
    check(
        "and its number is free here",
        syscall::sys_fd_write(4, b"x") == u64::MAX,
    );
    let _ = t.join();
    check(
        "a thread ending closes nothing",
        syscall::sys_fd_close(3).is_ok(),
    );

    // A mark is the descriptor's, not the object's.
    let wired = own_pipe(3, 4).is_ok();
    check("pipe wired again", wired);
    if wired {
        let me = syscall::sys_getpid() as usize;
        check("a new descriptor is unmarked", syscall::sys_fd_cloexec(3) == Ok(false));
        check("marked to close on exec", syscall::sys_fd_set_cloexec(3, true).is_ok());
        check("and says so", syscall::sys_fd_cloexec(3) == Ok(true));
        check("a copy of it is not", {
            let copy = syscall::sys_fd_dup(me, 5, 3).is_ok();
            let unmarked = syscall::sys_fd_cloexec(5) == Ok(false);
            let _ = syscall::sys_fd_close(5);
            copy && unmarked
        });
        check("an empty descriptor has no mark", syscall::sys_fd_cloexec(9).is_err());

        // A forked child has its own table: it closes both ends and goes, and
        // the pipe is still whole here.
        match syscall::sys_fork() {
            Ok(0) => {
                let closed = syscall::sys_fd_close(3).is_ok() && syscall::sys_fd_close(4).is_ok();
                // The mark came across with the descriptor it was on.
                syscall::sys_exit_program(if closed { 7 } else { 8 });
            }
            Ok(child) => {
                check("a forked child holds copies", wait_for(child) == Some(7));
                check(
                    "and closing them closed nothing here",
                    syscall::sys_fd_write(4, b"z") == 1 && syscall::sys_fd_read(3, &mut buf) == 1,
                );
            }
            Err(()) => check("fork", false),
        }
        let _ = syscall::sys_fd_close(3);
        let _ = syscall::sys_fd_close(4);
    }

    // A program that ends with a thread still parked. Its descriptors are the
    // program's, so they close when it ends, whichever task was holding on.
    let Some((child, mine)) = lock_child(b"leave", b"") else {
        check("started a program with a thread that never ends", false);
        return;
    };
    check("a program ends with one status, threads and all", wait_for(child.tid) == Some(5));
    let mut ended = false;
    for _ in 0..100 {
        match syscall::sys_fd_read_nb(mine, &mut buf) {
            0 => {
                ended = true;
                break;
            }
            _ => syscall::sleep_ticks(1),
        }
    }
    check("and what it had open is closed", ended);
    let _ = syscall::sys_fd_close(mine);
}

/// A file as a descriptor: in the kernel's table, with its position kept by
/// the server, so that everything a descriptor can do a file can do.
fn test_file_descriptors() {
    println!("files as descriptors:");
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the VFS", false);
        return;
    };
    let _ = vfs::mkdir(v, b"/tmp");
    let path: &[u8] = b"/tmp/dtest-descriptor";
    let _ = vfs::unlink(v, path);
    let both = vfs::OPEN_READ | vfs::OPEN_WRITE;
    let Ok(fd) = vfs::open_fd(v, path, vfs::OPEN_CREATE | both, 0o640) else {
        check("a file opens as a descriptor", false);
        return;
    };
    check("a file opens as a descriptor", fd >= 3);
    check(
        "made with the mode it was asked for",
        vfs::lstat(v, path).is_ok_and(|st| st.mode & 0o7777 == 0o640),
    );
    // Through the kernel: nothing here says "file".
    check("written like any descriptor", syscall::sys_fd_write(fd, b"one\n") == 4);
    check("and the position moved", vfs::seek(v, fd, 0, vfs::SEEK_CUR) == Ok(4));

    // The position is the descriptor's, wherever its copies end up. A child
    // that writes through the one it inherited writes after its parent.
    match syscall::sys_fork() {
        Ok(0) => {
            let wrote = syscall::sys_fd_write(fd, b"two\n") == 4;
            syscall::sys_exit_program(if wrote { 0 } else { 1 });
        }
        Ok(child) => check("a forked child writes through its copy", wait_for(child) == Some(0)),
        Err(()) => check("fork", false),
    }
    let _ = syscall::sys_fd_write(fd, b"three\n");
    let mut buf = [0u8; 32];
    check("back to the start", vfs::seek(v, fd, 0, vfs::SEEK_SET) == Ok(0));
    check(
        "parent, child, parent: nothing written over",
        syscall::sys_fd_read(fd, &mut buf) == 14 && &buf[..14] == b"one\ntwo\nthree\n",
    );
    check("and the read ends where the file does", syscall::sys_fd_read(fd, &mut buf) == 0);

    // A second descriptor from the first shares the position; a second open
    // has its own.
    let copy = syscall::sys_fd_dup_self(fd, 3);
    let _ = vfs::seek(v, fd, 4, vfs::SEEK_SET);
    check(
        "a copy reads from where the original is",
        copy.is_ok_and(|c| syscall::sys_fd_read(c, &mut buf[..4]) == 4 && &buf[..4] == b"two\n"),
    );
    let again = vfs::open_fd(v, path, vfs::OPEN_READ, 0);
    check(
        "another open starts at the start",
        again.is_ok_and(|a| syscall::sys_fd_read(a, &mut buf[..3]) == 3 && &buf[..3] == b"one"),
    );
    check(
        "a descriptor opened to read refuses a write",
        again.is_ok_and(|a| syscall::sys_fd_write(a, b"x") == u64::MAX),
    );
    let appender = vfs::open_fd(v, path, vfs::OPEN_WRITE | vfs::OPEN_APPEND, 0);
    check(
        "one opened to append writes at the end wherever it is",
        appender.is_ok_and(|a| {
            vfs::seek(v, a, 0, vfs::SEEK_SET) == Ok(0)
                && syscall::sys_fd_write(a, b"!") == 1
                && vfs::seek(v, fd, 0, vfs::SEEK_END) == Ok(15)
        }),
    );
    for d in [copy.ok(), again.ok(), appender.ok()].into_iter().flatten() {
        let _ = syscall::sys_fd_close(d);
    }

    // Mode and times, changed and read back.
    check(
        "chmod and a time set",
        vfs::set_attr(v, path, vfs::ATTR_MODE | vfs::ATTR_MTIME, 0o600, 0, 0, 0, 1_234_567).is_ok()
            && vfs::lstat(v, path).is_ok_and(|st| st.mode & 0o7777 == 0o600 && st.mtime == 1_234_567),
    );
    check(
        "nothing of a file that is not there",
        vfs::set_attr(v, b"/tmp/dtest-no-such-file", vfs::ATTR_MODE, 0o600, 0, 0, 0, 0)
            == Err(vfs::ERR_NOT_FOUND),
    );

    // Removed while open: still a file to whoever holds it, and gone when the
    // last descriptor is.
    check("removed while open", vfs::unlink(v, path).is_ok());
    check(
        "still there for its descriptor",
        vfs::seek(v, fd, 0, vfs::SEEK_SET) == Ok(0)
            && syscall::sys_fd_read(fd, &mut buf[..3]) == 3
            && &buf[..3] == b"one",
    );
    check("closed", syscall::sys_fd_close(fd).is_ok());

    // Closing gives the handle back: a hundred and fifty opens, sixty at a
    // time, in a table that holds sixty-four.
    let mut opened = 0;
    for _ in 0..3 {
        let mut held = [0usize; 50];
        let mut n = 0;
        for slot in held.iter_mut() {
            match vfs::open_fd(v, b"/etc/passwd", vfs::OPEN_READ, 0) {
                Ok(d) => {
                    *slot = d;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        opened += n;
        for d in &held[..n] {
            let _ = syscall::sys_fd_close(*d);
        }
    }
    check("a hundred and fifty opens, each closed", opened == 150);

    // Where a program is goes with it. A forked child is another program as
    // far as the server can tell, and used to start at the root.
    let moved = vfs::chdir(v, b"/etc").is_ok();
    check("chdir to /etc", moved);
    match syscall::sys_fork() {
        Ok(0) => {
            let here = vfs::open(v, b"passwd").is_ok();
            let mut name = [0u8; 16];
            let said = vfs::getcwd(v, &mut name) == Ok(4) && &name[..4] == b"/etc";
            syscall::sys_exit_program(if here && said { 0 } else { 1 });
        }
        Ok(child) => check("a forked child is where its parent was", wait_for(child) == Some(0)),
        Err(()) => check("fork", false),
    }
    check("and back to /", vfs::chdir(v, b"/").is_ok());
}

/// As `dchild fdclient` knows them.
const ASK_OPEN: u64 = 0x51;
const ASK_HELD: u64 = 0x52;
const ASK_CHDIR: u64 = 0x53;
const COOKIE_FILE: u64 = 0x5151;
const COOKIE_DIR: u64 = 0x7700_0000_0077;

/// This program as a server of descriptors, and `dchild fdclient` as what it
/// serves: a file is this, with the file server in this program's place.
fn test_served() {
    use quark_rt::ipc::{self, Message, TID_ANY};
    println!("descriptors a server serves:");
    let me = syscall::sys_getpid() as usize;

    let mut tid_text = [0u8; 20];
    let mut n = 0;
    let mut v = me;
    let mut digits = [0u8; 20];
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        tid_text[i] = digits[n - 1 - i];
    }
    let Some(child) = load_child(&[b"dchild", b"fdclient", &tid_text[..n]]) else {
        check("loaded a client", false);
        return;
    };
    // It may call this task: an Endpoint to it, from its creator.
    let granted = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, me as u64, 0)
        .is_ok()
        && syscall::sys_cap_grant_any(child.tid, syscall::SLOT_SCRATCH).is_ok();
    let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
    check("let the client call us", granted);
    // Nobody is handed a descriptor unasked: the client is not calling yet.
    check(
        "a task that is not calling cannot be given one",
        syscall::sys_fd_serve(child.tid, COOKIE_FILE, syscall::ANY_FD).is_err(),
    );
    if child.start().is_err() {
        check("started the client", false);
        return;
    }
    let child = child.tid;

    let mut served = None;
    let mut wrote = false;
    let mut read = false;
    let mut cwd_seen = false;
    let mut notices = 0;
    let mut collected = [0u64; 4];
    let mut ncollected = 0;
    let mut died = false;
    // The client's fork is a second program holding the first cookie, so the
    // file is released twice over before it is released: only the last counts.
    for _ in 0..400 {
        if ncollected >= 2 && died {
            break;
        }
        let mut msg = Message::empty();
        if syscall::sys_recv_timeout(TID_ANY, &mut msg, 5).is_err() {
            // 3 is `sys_task_info`'s state for a task that has exited.
            died = !matches!(syscall::sys_task_info(child), Ok((state, _, _)) if state != 3);
            continue;
        }
        if ipc::fd_released_notice(&msg) {
            notices += 1;
            while let Some(cookie) = syscall::sys_fd_reap() {
                if ncollected < collected.len() {
                    collected[ncollected] = cookie;
                    ncollected += 1;
                }
            }
            continue;
        }
        let from = msg.sender;
        let mut reply = Message::empty();
        match msg.tag {
            ASK_OPEN => {
                served = syscall::sys_fd_serve(from, COOKIE_FILE, syscall::ANY_FD).ok();
                match served {
                    Some(fd) => reply.data[0] = fd as u64,
                    None => reply.tag = u64::MAX,
                }
            }
            ASK_HELD => reply.data[0] = syscall::sys_fd_holds(from, msg.data[0]) as u64,
            ASK_CHDIR => {
                if syscall::sys_fd_serve(from, COOKIE_DIR, syscall::FD_CWD).is_err() {
                    reply.tag = u64::MAX;
                }
                cwd_seen = syscall::sys_fd_cookie(from, syscall::FD_CWD) == Some(COOKIE_DIR)
                    && syscall::sys_fd_holds(from, COOKIE_DIR);
            }
            // The kernel, writing for the client: what it wrote is lent.
            ipc::TAG_FD_WRITE => {
                let mut got = [0u8; 8];
                wrote = msg.data[0] == COOKIE_FILE
                    && msg.data[1] == 5
                    && syscall::sys_fd_holds(from, COOKIE_FILE)
                    && syscall::sys_lent_read(from, 0, &mut got[..5]) == Ok(5)
                    && &got[..5] == b"hello";
                reply.data[0] = 5;
            }
            // And reading: its buffer is lent to fill.
            ipc::TAG_FD_READ => {
                read = msg.data[0] == COOKIE_FILE
                    && msg.data[1] == 8
                    && syscall::sys_lent_write(from, 0, b"world") == Ok(5);
                reply.data[0] = 5;
            }
            _ => reply.tag = u64::MAX,
        }
        let _ = syscall::sys_reply(from, &reply);
    }

    let status = wait_for(child).unwrap_or(-1);
    check("the client was given a descriptor", served.is_some_and(|fd| fd >= 3));
    check("and knows whose object it names", status & 1 != 0);
    check("a write through it is a call to its server", wrote && status & 2 != 0);
    check("and so is a read", read && status & 4 != 0);
    check("a forked child has one of its own", status & 8 != 0);
    check("a copy keeps the object when the first closes", status & 16 != 0);
    check("the last close ends it", status & 32 != 0);
    check("a working directory is a descriptor too", cwd_seen && status & 64 != 0);
    check("the server is told when one has no descriptors left", notices >= 1);
    check(
        "and collects each object once: the file, then the directory",
        ncollected == 2 && collected[0] == COOKIE_FILE && collected[1] == COOKIE_DIR,
    );
    check("then there is nothing to collect", syscall::sys_fd_reap().is_none());
    check(
        "nobody holds what was collected",
        !syscall::sys_fd_holds(me, COOKIE_FILE) && !syscall::sys_fd_holds(child, COOKIE_DIR),
    );
}

const SHM_AT: usize = 0x94_0000_0000;

fn test_big_region() {
    println!("shared memory:");
    // 2000 pages is two 1280x800 buffers: the case that could not be
    // expressed when a region was capped at 1024 pages.
    let handle = match syscall::sys_shmem_create(2000) {
        Ok(h) => h,
        Err(()) => {
            check("create a 2000-page region", false);
            return;
        }
    };
    check("create a 2000-page region", true);
    check("map it", syscall::sys_shmem_map(handle, SHM_AT).is_ok());

    // Write the page number into the first word of every page and read it
    // back. A run list that stitches its runs together wrongly shows up here
    // and nowhere else.
    let mut good = true;
    for p in 0..2000usize {
        let at = (SHM_AT + p * 4096) as *mut u64;
        unsafe { core::ptr::write_volatile(at, p as u64 ^ 0x5A5A_0000) };
    }
    for p in 0..2000usize {
        let at = (SHM_AT + p * 4096) as *const u64;
        if unsafe { core::ptr::read_volatile(at) } != p as u64 ^ 0x5A5A_0000 {
            good = false;
            break;
        }
    }
    check("every one of its 2000 pages is distinct and readable", good);

    check("unmap", syscall::sys_shmem_unmap(handle, SHM_AT).is_ok());
    check("destroy", syscall::sys_shmem_destroy(handle).is_ok());
}

const MEMFD_AT: usize = 0x95_0000_0000;

fn test_memfd() {
    println!("memory as a descriptor:");
    let fd = match syscall::sys_memfd_create(4) {
        Ok(f) => f,
        Err(()) => {
            check("create a four-page memory descriptor", false);
            return;
        }
    };
    check("create a four-page memory descriptor", fd >= 3);
    check("map it", syscall::sys_mmap_fd(fd, MEMFD_AT).is_ok());

    unsafe { core::ptr::write_volatile(MEMFD_AT as *mut u64, 0xFEED_FACE) };
    check(
        "what was written is there",
        unsafe { core::ptr::read_volatile(MEMFD_AT as *const u64) } == 0xFEED_FACE,
    );

    check("close it", syscall::sys_fd_close(fd).is_ok());
    check(
        "mapping a closed descriptor fails",
        syscall::sys_mmap_fd(fd, MEMFD_AT + 0x10000).is_err(),
    );
}

fn test_socketpair() {
    println!("socketpair:");
    let (a, b) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => {
            check("create a pair", false);
            return;
        }
    };
    check("create a pair", a >= 3 && b >= 3 && a != b);

    let mut buf = [0u8; 16];
    check("a writes", syscall::sys_fd_write(a, b"ping") == 4);
    check(
        "b reads what a wrote",
        syscall::sys_fd_read(b, &mut buf) == 4 && &buf[..4] == b"ping",
    );
    // The direction a pipe cannot do.
    check("b writes", syscall::sys_fd_write(b, b"pong") == 4);
    check(
        "a reads what b wrote",
        syscall::sys_fd_read(a, &mut buf) == 4 && &buf[..4] == b"pong",
    );

    check("close a", syscall::sys_fd_close(a).is_ok());
    check("b now reads EOF", syscall::sys_fd_read(b, &mut buf) == 0);
    check("close b", syscall::sys_fd_close(b).is_ok());
}

const PASSED_AT: usize = 0x96_0000_0000;

fn test_fd_passing() {
    println!("descriptor passing:");
    let (a, b) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => { check("a pair to pass over", false); return; }
    };
    let mem = match syscall::sys_memfd_create(2) {
        Ok(f) => f,
        Err(()) => { check("memory to pass", false); return; }
    };
    check("a pair and some memory", true);

    // Write a witness through the sender's own mapping first.
    check("map it here", syscall::sys_mmap_fd(mem, PASSED_AT).is_ok());
    unsafe { core::ptr::write_volatile(PASSED_AT as *mut u64, 0xC0FFEE) };

    check(
        "send the descriptor with a byte",
        syscall::sys_fd_send(a, b"m", Some(mem)) == Ok(1),
    );

    let mut buf = [0u8; 4];
    check(
        "receive says a descriptor came",
        syscall::sys_fd_recv(b, &mut buf, Some(20)) == Ok((1, Some(20))),
    );

    // The received descriptor is a different number naming the same memory.
    check(
        "map the received descriptor",
        syscall::sys_mmap_fd(20, PASSED_AT + 0x8000).is_ok(),
    );
    check(
        "it is the same memory",
        unsafe { core::ptr::read_volatile((PASSED_AT + 0x8000) as *const u64) } == 0xC0FFEE,
    );

    // Receiving when nothing was attached must not invent one.
    check("send with no descriptor", syscall::sys_fd_send(a, b"x", None) == Ok(1));
    check(
        "receive says none came",
        syscall::sys_fd_recv(b, &mut buf, Some(21)) == Ok((1, None)),
    );

    let _ = syscall::sys_fd_close(20);
    let _ = syscall::sys_fd_close(mem);

    // In flight, with the sender's own copy gone. The queue has to hold a
    // reference of its own, or the region is freed under the descriptor
    // travelling towards the peer and the receiver maps freed memory.
    let orphan = match syscall::sys_memfd_create(1) {
        Ok(f) => f,
        Err(()) => { check("memory to orphan", false); return; }
    };
    check("map the orphan here", syscall::sys_mmap_fd(orphan, PASSED_AT + 0x20000).is_ok());
    unsafe { core::ptr::write_volatile((PASSED_AT + 0x20000) as *mut u64, 0xBEEF) };
    check("send it", syscall::sys_fd_send(a, b"o", Some(orphan)) == Ok(1));
    check("close the only other copy", syscall::sys_fd_close(orphan).is_ok());
    check(
        "receive it anyway",
        syscall::sys_fd_recv(b, &mut buf, Some(22)) == Ok((1, Some(22))),
    );
    check("map what arrived", syscall::sys_mmap_fd(22, PASSED_AT + 0x28000).is_ok());
    check(
        "and it still holds what was written",
        unsafe { core::ptr::read_volatile((PASSED_AT + 0x28000) as *const u64) } == 0xBEEF,
    );

    let _ = syscall::sys_fd_close(22);

    // Asking for any slot rather than naming one. A caller translating
    // `recvmsg` has no way to name one: Linux chooses the number, and the
    // alternative -- probing -- means reading, which is the one thing a
    // receive must do exactly once.
    let any = match syscall::sys_memfd_create(1) {
        Ok(f) => f,
        Err(()) => { check("memory to send anywhere", false); return; }
    };
    check("send it", syscall::sys_fd_send(a, b"x", Some(any)) == Ok(1));
    let landed = syscall::sys_fd_recv(b, &mut buf, Some(syscall::ANY_FD));
    check("receive into a slot of the kernel's choosing", matches!(landed, Ok((1, Some(_)))));
    let slot = match landed { Ok((_, Some(s))) => s, _ => 0 };
    check("the slot it named is above the standard three", slot >= 3);
    check("and it holds memory", syscall::sys_mmap_fd(slot, PASSED_AT + 0x30000).is_ok());
    let _ = syscall::sys_fd_close(slot);
    let _ = syscall::sys_fd_close(any);

    // Sizing memory after making it. This is `ftruncate`, and every Wayland
    // client's buffer pool is made that way: memfd_create, then ftruncate,
    // then mmap.
    let grow = match syscall::sys_memfd_create(1) {
        Ok(f) => f,
        Err(()) => { check("memory to grow", false); return; }
    };
    check("one page to start with", syscall::sys_memfd_truncate(grow, 0).is_err());
    check("grow it to ten", syscall::sys_memfd_truncate(grow, 10 * 4096) == Ok(10 * 4096));
    check(
        "a size that is not a whole page rounds up",
        syscall::sys_memfd_truncate(grow, 4097) == Ok(2 * 4096),
    );
    check(
        "and mapping it says how big it became",
        syscall::sys_mmap_fd(grow, PASSED_AT + 0x40000) == Ok(2 * 4096),
    );
    unsafe { core::ptr::write_volatile((PASSED_AT + 0x40000 + 4096) as *mut u64, 0x1234) };
    check(
        "the second page is really there",
        unsafe { core::ptr::read_volatile((PASSED_AT + 0x40000 + 4096) as *const u64) } == 0x1234,
    );
    // Not while somebody holds a mapping: growing would change what is behind
    // it, and nothing would tell them.
    check("no resizing what is mapped", syscall::sys_memfd_truncate(grow, 4 * 4096).is_err());
    let _ = syscall::sys_munmap(PASSED_AT + 0x40000, 2);

    // A second name for one of your own descriptors, which needs no authority.
    let copy = match syscall::sys_fd_dup_self(grow, 3) {
        Ok(f) => f,
        Err(()) => { check("duplicate a descriptor", false); return; }
    };
    check("the duplicate is a different number", copy != grow);
    check("and it names the same memory", syscall::sys_mmap_fd(copy, PASSED_AT + 0x48000).is_ok());
    check(
        "a floor is respected",
        matches!(syscall::sys_fd_dup_self(grow, 20), Ok(f) if f >= 20),
    );
    let _ = syscall::sys_munmap(PASSED_AT + 0x48000, 2);
    let _ = syscall::sys_fd_close(copy);
    let _ = syscall::sys_fd_close(grow);

    let _ = syscall::sys_fd_close(a);
    let _ = syscall::sys_fd_close(b);
}

fn test_no_leak() {
    println!("abandoned descriptors are reclaimed:");
    // Send a descriptor and throw the connection away without receiving it,
    // forty times. There are thirty-two streams and the region table is
    // finite, so anything that fails to give back what it took runs out
    // before this loop does.
    let mut rounds = 0;
    for _ in 0..40 {
        let Ok((a, b)) = syscall::sys_socketpair() else { break };
        let Ok(mem) = syscall::sys_memfd_create(1) else {
            let _ = syscall::sys_fd_close(a);
            let _ = syscall::sys_fd_close(b);
            break;
        };
        if syscall::sys_fd_send(a, b"z", Some(mem)) != Ok(1) {
            break;
        }
        // Everybody drops it: the sender's copy, and both ends of the stream
        // that was carrying the one in flight.
        let _ = syscall::sys_fd_close(mem);
        let _ = syscall::sys_fd_close(a);
        let _ = syscall::sys_fd_close(b);
        rounds += 1;
    }
    check("forty rounds of send-and-abandon", rounds == 40);

    // And the tables still work afterwards.
    match syscall::sys_socketpair() {
        Ok((a, b)) => {
            check("a stream can still be made", true);
            let _ = syscall::sys_fd_close(a);
            let _ = syscall::sys_fd_close(b);
        }
        Err(()) => check("a stream can still be made", false),
    }
    match syscall::sys_memfd_create(1) {
        Ok(m) => {
            check("memory can still be made", true);
            let _ = syscall::sys_fd_close(m);
        }
        Err(()) => check("memory can still be made", false),
    }
}

fn test_pollset() {
    println!("waiting on a set:");
    let (a, b) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => { check("a pair to watch", false); return; }
    };
    let set = match syscall::sys_pollset_create() {
        Ok(s) => s,
        Err(()) => { check("create a set", false); return; }
    };
    check("create a set", set >= 3);
    check(
        "watch b for readable",
        syscall::sys_pollset_add(set, b, syscall::POLL_READABLE, 0xB).is_ok(),
    );
    check(
        "watch a for writable",
        syscall::sys_pollset_add(set, a, syscall::POLL_WRITABLE, 0xA).is_ok(),
    );

    let mut ready = [syscall::Ready::empty(); 4];

    // `a` is writable now and `b` is not readable, so exactly one fires.
    check("one is ready", syscall::sys_pollset_wait(set, &mut ready, 50) == Ok(1));
    check("and it is the writable one", ready[0].token == 0xA);

    // Stop watching `a`, then nothing is ready until something is written.
    check("stop watching a", syscall::sys_pollset_remove(set, a).is_ok());
    check(
        "nothing ready, and it timed out",
        syscall::sys_pollset_wait(set, &mut ready, 5) == Ok(0),
    );

    check("write to a", syscall::sys_fd_write(a, b"go") == 2);
    let n = syscall::sys_pollset_wait(set, &mut ready, 50);
    check("now b is ready", n == Ok(1) && ready[0].token == 0xB);
    check(
        "readable is what it reports",
        ready[0].events & syscall::POLL_READABLE != 0,
    );

    // A closed peer is a hangup rather than a silence.
    let mut buf = [0u8; 4];
    let _ = syscall::sys_fd_read(b, &mut buf);
    check("close a", syscall::sys_fd_close(a).is_ok());
    let n = syscall::sys_pollset_wait(set, &mut ready, 50);
    check(
        "b reports hangup",
        n == Ok(1) && ready[0].events & syscall::POLL_HANGUP != 0,
    );

    // A descriptor that can never become ready is refused, not accepted and
    // then silent. Note that stdout is *not* an example: init wires it to a
    // pipe, so watching it for writable is a reasonable thing to ask and the
    // kernel is right to allow it.
    check(
        "watching an empty descriptor is refused",
        syscall::sys_pollset_add(set, 30, syscall::POLL_READABLE, 0xC).is_err(),
    );
    // An IPC endpoint is one of those: nothing says when a server would
    // answer. Made here for the purpose — standard input is one only where
    // the console is not a terminal, and a terminal is a thing to wait on.
    let me = syscall::sys_getpid() as usize;
    let endpoint = syscall::sys_fd_set(me, 29, me, 1).is_ok();
    check(
        "watching an IPC endpoint is refused",
        endpoint && syscall::sys_pollset_add(set, 29, syscall::POLL_READABLE, 0xD).is_err(),
    );
    let _ = syscall::sys_fd_close(29);
    check(
        "watching stdout, which really is a pipe, is allowed",
        syscall::sys_pollset_add(set, 1, syscall::POLL_WRITABLE, 0xE).is_ok(),
    );

    let _ = syscall::sys_fd_close(set);
    let _ = syscall::sys_fd_close(b);
}

/// The waking thread's end of the pair, handed to it as descriptor 3.
const WAKER_FD: usize = 3;

/// Where the kernel says a signal has arrived for a handler.
static SIG_WORD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// The master of the terminal `typist` types at.
static TYPIST_MASTER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Wait a while, then press Ctrl-C at the terminal. On a thread, so that the
/// main task can be reading the terminal when it is pressed.
extern "C" fn typist() -> ! {
    syscall::sleep_ticks(20);
    let master = TYPIST_MASTER.load(core::sync::atomic::Ordering::SeqCst);
    let _ = syscall::sys_fd_write_nb(master, b"\x03");
    syscall::sys_exit_code(0);
}

/// Signals: what a program says about one, what the kernel does when it has
/// said nothing, and how it is told when it has a handler.
/// A thread that does nothing but end.
extern "C" fn leaver() -> ! {
    syscall::sys_exit_code(0)
}

fn test_signals() {
    use core::sync::atomic::Ordering::SeqCst;
    println!("signals:");
    let me = syscall::sys_getpid() as usize;
    const USR1: u64 = 10;
    const USR2: u64 = 12;
    let bit = |signo: u64| 1u64 << (signo - 1);

    check("a program starts having said nothing", syscall::sys_sig_action_get(USR1) == Ok(syscall::SIG_DEFAULT));
    check(
        "ignoring a signal answers with what it was",
        syscall::sys_sig_action(USR2, syscall::SIG_IGNORE) == Ok(syscall::SIG_DEFAULT)
            && syscall::sys_sig_action_get(USR2) == Ok(syscall::SIG_IGNORE),
    );
    check(
        "kill and stop cannot be ignored or handled",
        syscall::sys_sig_action(syscall::SIGKILL, syscall::SIG_IGNORE).is_err()
            && syscall::sys_sig_action(syscall::SIGKILL, syscall::SIG_HANDLE).is_err()
            && syscall::sys_sig_action(19, syscall::SIG_IGNORE).is_err(),
    );
    check(
        "0 and 65 are not signals",
        syscall::sys_sig_action_get(0).is_err() && syscall::sys_sig_action_get(65).is_err(),
    );
    check("an ignored signal does nothing", syscall::sys_sig_raise(me, USR2).is_ok());
    check(
        "signal 0 asks and raises nothing",
        syscall::sys_sig_raise(me, 0).is_ok() && syscall::sys_sig_raise(63, 0).is_err(),
    );

    // A handler. The kernel runs none: it says the signal is waiting, in a
    // word of this program's and in the answer to the next take.
    let _ = syscall::sys_sig_action(USR1, syscall::SIG_HANDLE);
    SIG_WORD.store(0, SeqCst);
    check("nothing is waiting to begin with", syscall::sys_sig_take(Some(&SIG_WORD)) == 0);
    check("a handled signal is raised", syscall::sys_sig_raise(me, USR1).is_ok());
    check("and the program's word says so", SIG_WORD.load(SeqCst) == 1);

    // One wait is ended by it: the first to look.
    let mut msg = quark_rt::ipc::Message::empty();
    let before = syscall::sys_ticks();
    let ended = syscall::sys_recv_timeout(me, &mut msg, 50);
    check(
        "a sleep ends at once, saying why",
        ended == Err(syscall::SLEEP_INTERRUPTED) && syscall::sys_ticks() - before < 10,
    );
    let before = syscall::sys_ticks();
    let ended = syscall::sys_recv_timeout(me, &mut msg, 10);
    check(
        "the next sleep is a sleep",
        ended == Err(1) && syscall::sys_ticks() - before >= 9,
    );
    check("taking it gives the signal", syscall::sys_sig_take(None) == bit(USR1));
    check("once", syscall::sys_sig_take(None) == 0);

    // A forked child is a copy of the program, what it said about signals
    // included, with nothing waiting.
    let _ = syscall::sys_sig_raise(me, USR1);
    match syscall::sys_fork() {
        Ok(0) => {
            let same = syscall::sys_sig_action_get(USR1) == Ok(syscall::SIG_HANDLE)
                && syscall::sys_sig_action_get(USR2) == Ok(syscall::SIG_IGNORE);
            let nothing = syscall::sys_sig_take(None) == 0;
            syscall::sys_exit_program(if same && nothing { 7 } else { 8 });
        }
        Ok(child) => check("a forked child says what its parent said", wait_for(child) == Some(7)),
        Err(()) => check("fork", false),
    }
    check("and what was waiting stayed with the parent", syscall::sys_sig_take(None) == bit(USR1));

    // A process id: what a program is called by something that will ask
    // about it later. Never a task id, never used twice, and what a wait and
    // a signal can each name a program by.
    let mine = syscall::sys_pid_self();
    check("a process id is not a task id", mine >= 64 && syscall::sys_pid(me) == Some(mine));
    let mut seen = [0u64; 3];
    let mut tids = [0usize; 3];
    for i in 0..3 {
        match syscall::sys_fork() {
            Ok(0) => syscall::sys_exit_program(40 + i as i32),
            Ok(child) => {
                tids[i] = child;
                seen[i] = syscall::sys_pid(child).unwrap_or(0);
                let waited = syscall::sys_wait_for_pid(seen[i]);
                if waited != Ok((seen[i], 40 + i as i32)) {
                    seen[i] = 0;
                }
            }
            Err(()) => {}
        }
    }
    check(
        "a child is waited for by its process id, and answered by it",
        seen.iter().all(|&p| p >= 64),
    );
    check(
        "the task id comes round again and the process id does not",
        (tids[0] == tids[1] || tids[1] == tids[2]) && seen[0] < seen[1] && seen[1] < seen[2],
    );
    check(
        "a process id that has gone names nothing",
        syscall::sys_sig_raise_pid(seen[0], 0).is_err()
            && syscall::sys_wait_for_pid(seen[0]).is_err(),
    );
    check("a signal can be raised by process id", {
        let _ = syscall::sys_sig_raise_pid(mine, USR1);
        syscall::sys_sig_take(None) == bit(USR1)
    });

    // An alarm: a signal the kernel raises itself, after a time.
    const ALRM: u64 = syscall::SIGALRM;
    const CHLD: u64 = syscall::SIGCHLD;
    let slept = |ticks: u64| {
        let mut msg = quark_rt::ipc::Message::empty();
        let before = syscall::sys_ticks();
        let ended = syscall::sys_recv_timeout(me, &mut msg, ticks);
        (ended, syscall::sys_ticks() - before)
    };
    let _ = syscall::sys_sig_action(ALRM, syscall::SIG_HANDLE);
    check("a program has no alarm until it sets one", syscall::sys_sig_alarm_left() == (0, 0));
    check("setting one answers that there was none", syscall::sys_sig_alarm(100, 0) == (0, 0));
    let (left, _) = syscall::sys_sig_alarm_left();
    check("asking says what is left of it", left > 90 && left <= 100);
    let (left, every) = syscall::sys_sig_alarm(3, 0);
    check("setting another answers with what was left of the first", left > 90 && left <= 100 && every == 0);
    let (ended, took) = slept(50);
    check(
        "an alarm ends a sleep when it is due, and not before",
        ended == Err(syscall::SLEEP_INTERRUPTED) && (2..10).contains(&took),
    );
    check("as SIGALRM", syscall::sys_sig_take(None) == bit(ALRM));
    check("and is over", syscall::sys_sig_alarm_left() == (0, 0));

    let _ = syscall::sys_sig_alarm(2, 3);
    let mut rings = 0;
    for _ in 0..3 {
        if slept(50).0 == Err(syscall::SLEEP_INTERRUPTED) && syscall::sys_sig_take(None) == bit(ALRM) {
            rings += 1;
        }
    }
    check("one that repeats is raised again and again", rings == 3);
    let (left, every) = syscall::sys_sig_alarm(0, 0);
    check("until it is cancelled, which says how it stood", (1..=3).contains(&left) && every == 3);
    let (ended, took) = slept(8);
    check("and then nothing more comes", ended == Err(1) && took >= 7 && syscall::sys_sig_take(None) == 0);

    // It is the program's own: a child made by fork starts with none.
    let _ = syscall::sys_sig_alarm(500, 0);
    match syscall::sys_fork() {
        Ok(0) => syscall::sys_exit_program(if syscall::sys_sig_alarm_left() == (0, 0) { 7 } else { 8 }),
        Ok(child) => check("a forked child has no alarm of its parent's", wait_for(child) == Some(7)),
        Err(()) => check("fork", false),
    }
    check("and the parent's is still running", syscall::sys_sig_alarm(0, 0).0 > 400);

    // A program that has said nothing about the signal is ended by it.
    let _ = syscall::sys_sig_action(ALRM, syscall::SIG_DEFAULT);
    match syscall::sys_fork() {
        Ok(0) => {
            let _ = syscall::sys_sig_alarm(2, 0);
            syscall::sleep_ticks(200);
            syscall::sys_exit_program(0);
        }
        Ok(child) => {
            let before = syscall::sys_ticks();
            check(
                "an alarm nobody handles ends the program, as signal 14",
                wait_for(child) == Some(-14) && syscall::sys_ticks() - before < 50,
            );
        }
        Err(()) => check("fork", false),
    }

    // A child ending is a signal too, to a program that has asked to hear.
    let _ = syscall::sys_sig_action(CHLD, syscall::SIG_HANDLE);
    match syscall::sys_fork() {
        Ok(0) => {
            syscall::sleep_ticks(3);
            syscall::sys_exit_program(5);
        }
        Ok(child) => {
            let (ended, took) = slept(100);
            check(
                "a child ending ends its parent's sleep",
                ended == Err(syscall::SLEEP_INTERRUPTED) && took < 50,
            );
            check("as SIGCHLD", syscall::sys_sig_take(None) == bit(CHLD));
            check("with the child there to collect", wait_for(child) == Some(5));
        }
        Err(()) => check("fork", false),
    }
    // A thread ending is not: it is not a child, it is this program.
    match thread::spawn_with_stack(leaver, 8) {
        Ok(t) => {
            let _ = t.join();
            check("a thread ending is not a child ending", syscall::sys_sig_take(None) == 0);
        }
        Err(()) => check("start a thread to end", false),
    }
    let _ = syscall::sys_sig_action(CHLD, syscall::SIG_DEFAULT);
    match syscall::sys_fork() {
        Ok(0) => syscall::sys_exit_program(0),
        Ok(child) => check(
            "and a program that has said nothing is not troubled by one",
            wait_for(child) == Some(0) && syscall::sys_sig_take(None) == 0,
        ),
        Err(()) => check("fork", false),
    }

    // A program a spawner makes is a new one, and has said nothing.
    let fresh = load_child(&[b"dchild", b"sigstate"]).and_then(|c| {
        let tid = c.tid;
        c.start().ok().map(|()| tid)
    });
    check("a spawned program has said nothing", fresh.and_then(wait_for) == Some(0));

    // Nothing said, and the signal does what it does: ends the program, with
    // its number as the status.
    let sleeper = load_child(&[b"dchild", b"sleep"]).and_then(|c| {
        let tid = c.tid;
        c.start().ok().map(|()| tid)
    });
    match sleeper {
        Some(tid) => {
            syscall::sleep_ticks(10);
            check("a signal is raised for another program", syscall::sys_sig_raise(tid, syscall::SIGTERM).is_ok());
            check("which ends with the signal's number", wait_for(tid) == Some(-15));
        }
        None => check("started a program to signal", false),
    }
    // Ignored, it does nothing; and 9 cannot be.
    match (syscall::sys_socketpair(), load_child(&[b"dchild", b"sigignore"])) {
        (Ok((mine, theirs)), Some(child)) => {
            let tid = child.tid;
            let _ = syscall::sys_fd_dup(tid, 3, theirs);
            let _ = syscall::sys_fd_close(theirs);
            let started = child.start().is_ok();
            let mut said = [0u8; 1];
            let ready = started && syscall::sys_fd_read(mine, &mut said) == 1;
            let _ = syscall::sys_sig_raise(tid, syscall::SIGTERM);
            syscall::sleep_ticks(10);
            let alive = matches!(syscall::sys_task_info(tid), Ok((state, _, _)) if state != 3);
            check("a program that ignores a signal is not ended by it", ready && alive);
            let _ = syscall::sys_sig_raise(tid, syscall::SIGKILL);
            check("and is by 9", wait_for(tid) == Some(-9));
            let _ = syscall::sys_fd_close(mine);
        }
        _ => check("started a program that ignores a signal", false),
    }

    // A terminal. Its interrupt character raises signal 2 for every program
    // that holds the slave, and this one does.
    let pair = syscall::sys_pty_create().ok().and_then(|master| {
        let number = syscall::sys_pty_number(master).ok()?;
        Some((master, syscall::sys_pty_open(number).ok()?))
    });
    let Some((master, slave)) = pair else {
        check("a terminal", false);
        return;
    };
    let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_HANDLE);
    SIG_WORD.store(0, SeqCst);
    let _ = syscall::sys_fd_write_nb(master, b"abc\x03");
    check(
        "Ctrl-C at a terminal raises a signal for whoever holds it",
        SIG_WORD.load(SeqCst) == 1 && syscall::sys_sig_take(None) == bit(syscall::SIGINT),
    );

    // Pressed while a read of the terminal is waiting, it ends the read.
    TYPIST_MASTER.store(master, SeqCst);
    match thread::spawn_with_stack(typist, 8) {
        Ok(t) => {
            let mut line = [0u8; 16];
            let before = syscall::sys_ticks();
            let got = syscall::sys_fd_read(slave, &mut line);
            let waited = syscall::sys_ticks() - before;
            check(
                "a read of the terminal is ended by it",
                got == syscall::INTERRUPTED && (10..200).contains(&waited),
            );
            check("and it is waiting to be taken", syscall::sys_sig_take(None) == bit(syscall::SIGINT));
            let _ = t.join();
        }
        Err(_) => check("a thread to press the key", false),
    }

    // A program that holds the terminal and has said nothing is ended.
    match load_child(&[b"dchild", b"sleep"]) {
        Some(child) => {
            let tid = child.tid;
            let _ = syscall::sys_fd_dup(tid, 0, slave);
            let started = child.start().is_ok();
            syscall::sleep_ticks(10);
            let _ = syscall::sys_fd_write_nb(master, b"\x03");
            check("Ctrl-C ends a program that said nothing", started && wait_for(tid) == Some(-2));
        }
        None => check("started a program on the terminal", false),
    }
    let _ = syscall::sys_sig_take(None);

    // What a descriptor is, and whether anybody is at the other end.
    check(
        "a terminal's two ends say which they are",
        syscall::sys_fd_kind(master) == Some((syscall::FD_KIND_PTY_MASTER, false))
            && syscall::sys_fd_kind(slave) == Some((syscall::FD_KIND_PTY_SLAVE, false)),
    );
    // What is typed is UTF-8, and erasing takes back a character of it and
    // not a byte: é is two bytes and 中 is three, and each goes whole.
    check(
        "a new terminal expects UTF-8",
        syscall::sys_pty_get_termios(master).is_ok_and(|t| t.c_iflag & 0o40000 != 0),
    );
    let _ = syscall::sys_fd_write_nb(master, "aé中".as_bytes());
    let _ = syscall::sys_fd_write_nb(master, b"\x7f\x7fz\n");
    let mut line = [0u8; 16];
    let got = syscall::sys_fd_read(slave, &mut line);
    check("erasing at a terminal takes back a whole character", got == 3 && &line[..3] == b"az\n");

    let _ = syscall::sys_fd_close(slave);
    check(
        "and a master whose slave has gone says so",
        syscall::sys_fd_kind(master) == Some((syscall::FD_KIND_PTY_MASTER, true)),
    );
    let _ = syscall::sys_fd_close(master);
    if let Ok((a, b)) = syscall::sys_socketpair() {
        check("a stream is a stream", syscall::sys_fd_kind(a) == Some((syscall::FD_KIND_STREAM, false)));
        let _ = syscall::sys_fd_close(b);
        check(
            "a write nobody can read fails, and the descriptor says why",
            syscall::sys_fd_write(a, b"x") == u64::MAX
                && syscall::sys_fd_kind(a) == Some((syscall::FD_KIND_STREAM, true)),
        );
        let _ = syscall::sys_fd_close(a);
        check("a number that names nothing has no kind", syscall::sys_fd_kind(a).is_none());
    } else {
        check("a stream", false);
    }

    // As it was found.
    for signo in [syscall::SIGINT, USR1, USR2] {
        let _ = syscall::sys_sig_action(signo, syscall::SIG_DEFAULT);
    }
    let _ = syscall::sys_sig_take(None);
}

/// Process groups, sessions, programs that stop, and whose a terminal is.
fn test_jobs() {
    use syscall::{ChildNews, Refused};
    println!("jobs:");
    let bit = |signo: u64| 1u64 << (signo - 1);
    let group = syscall::sys_getpgid(0);
    let session = syscall::sys_getsid(0);
    check("a program is in a process group and a session", group.is_some() && session.is_some());

    // A child that can be seen to be running: it writes a byte every
    // twentieth of a second.
    let started = match (syscall::sys_socketpair(), load_child(&[b"dchild", b"beat"])) {
        (Ok((mine, theirs)), Some(child)) => {
            let tid = child.tid;
            let _ = syscall::sys_fd_dup(tid, 3, theirs);
            let _ = syscall::sys_fd_close(theirs);
            child.start().ok().map(|()| (tid, mine))
        }
        _ => None,
    };
    let Some((tid, beats)) = started else {
        check("start a child", false);
        return;
    };
    let pid = syscall::sys_pid(tid).unwrap_or(0);
    let mut byte = [0u8; 64];
    check("a child runs", syscall::sys_fd_read(beats, &mut byte[..1]) == 1);
    check(
        "it begins in its parent's group and session",
        syscall::sys_getpgid(pid) == group && syscall::sys_getsid(pid) == session,
    );
    check(
        "its parent can put it in a group of its own",
        syscall::sys_setpgid(pid, 0).is_ok() && syscall::sys_getpgid(pid) == Some(pid),
    );
    check(
        "but not in a group that is not there",
        syscall::sys_setpgid(pid, 0x7FFF_0000) == Err(Refused::NotAllowed),
    );
    check(
        "and a process that is nobody's child is not this one's to move",
        syscall::sys_setpgid(syscall::sys_pid(1).unwrap_or(1), 0) == Err(Refused::NoSuch),
    );

    // Stopped, it does not run; and its parent can ask to be told.
    check("signal 19 is raised for it", syscall::sys_sig_raise_pid(pid, syscall::SIGSTOP).is_ok());
    check(
        "a wait that asked hears that it has stopped",
        syscall::sys_wait_job(pid, syscall::WAIT_STOPPED) == Ok(Some(ChildNews::Stopped(pid, 19))),
    );
    check(
        "once",
        syscall::sys_wait_job(pid, syscall::WAIT_STOPPED | syscall::WAIT_NOW) == Ok(None),
    );
    check("its task says it is stopped", matches!(syscall::sys_task_info(tid), Ok((4, _, _))));
    while (1..=byte.len() as u64).contains(&syscall::sys_fd_read_nb(beats, &mut byte)) {}
    syscall::sleep_ticks(30);
    check(
        "and it does not run while it is",
        syscall::sys_fd_read_nb(beats, &mut byte) == syscall::WOULD_BLOCK,
    );
    check("signal 18 is raised for it", syscall::sys_sig_raise_pid(pid, syscall::SIGCONT).is_ok());
    check(
        "a wait that asked hears that it was continued",
        syscall::sys_wait_job(pid, syscall::WAIT_CONTINUED) == Ok(Some(ChildNews::Continued(pid))),
    );
    check("and it runs again", syscall::sys_fd_read(beats, &mut byte[..1]) == 1);

    // A signal for a group is for everything in it, and a wait can be too.
    let second = load_child(&[b"dchild", b"sleep"]).and_then(|c| {
        let tid = c.tid;
        c.start().ok().map(|()| tid)
    });
    let second_pid = second.and_then(syscall::sys_pid).unwrap_or(0);
    let outsider = load_child(&[b"dchild", b"quit"]).and_then(|c| {
        let tid = c.tid;
        c.start().ok().map(|()| tid)
    });
    check(
        "a second child joins the first one's group",
        syscall::sys_setpgid(second_pid, pid).is_ok() && syscall::sys_getpgid(second_pid) == Some(pid),
    );
    check(
        "a signal raised for the group",
        syscall::sys_sig_raise_group(pid, syscall::SIGTERM).is_ok(),
    );
    let mut ended = [0u64; 2];
    for slot in ended.iter_mut() {
        if let Ok(Some(ChildNews::Ended(who, -15))) = syscall::sys_wait_job(pid, syscall::WAIT_GROUP) {
            *slot = who;
        }
    }
    check(
        "ends both, and a wait for the group collects them",
        ended.contains(&pid) && ended.contains(&second_pid) && pid != second_pid,
    );
    check(
        "and nothing else: a child outside it is still there to collect",
        syscall::sys_wait_job(pid, syscall::WAIT_GROUP).is_err()
            && outsider.and_then(wait_for) == Some(0),
    );
    check(
        "a group nobody is in cannot be signalled",
        syscall::sys_sig_raise_group(pid, 0) == Err(Refused::NoSuch),
    );
    let _ = syscall::sys_fd_close(beats);

    // A terminal with a session: a child begins one and takes the terminal.
    let pair = syscall::sys_pty_create().ok().and_then(|master| {
        let number = syscall::sys_pty_number(master).ok()?;
        Some((master, syscall::sys_pty_open(number).ok()?))
    });
    let Some((master, slave)) = pair else {
        check("a terminal", false);
        return;
    };
    let leader = match (syscall::sys_socketpair(), load_child(&[b"dchild", b"leader"])) {
        (Ok((mine, theirs)), Some(child)) => {
            let tid = child.tid;
            let _ = syscall::sys_fd_dup(tid, 0, slave);
            let _ = syscall::sys_fd_dup(tid, 3, theirs);
            let _ = syscall::sys_fd_close(theirs);
            child.start().ok().map(|()| (tid, mine))
        }
        _ => None,
    };
    let Some((leader, says)) = leader else {
        check("start a child on the terminal", false);
        return;
    };
    let leader_pid = syscall::sys_pid(leader).unwrap_or(0);
    // If it was stopped after all it says nothing, and this must not wait
    // for ever to find that out.
    let mut fds = [syscall::PollFd::new(says, syscall::POLL_READABLE)];
    let went = if syscall::sys_poll(&mut fds, 300) == Ok(1) && syscall::sys_fd_read(says, &mut byte[..1]) == 1 {
        byte[0]
    } else {
        0
    };
    check("a program begins a session, which it leads, once", went & 7 == 7);
    check("and takes a terminal as the session's, with itself in front", went & 24 == 24);
    check(
        "a group nobody would continue is not stopped by the signal Ctrl-Z raises",
        went & 32 != 0,
    );
    check(
        "the terminal is not another session's to ask about",
        syscall::sys_pty_front(slave).is_none() && syscall::sys_pty_session(slave).is_none(),
    );
    check(
        "or to take",
        syscall::sys_pty_set_session(slave) == Err(Refused::NotAllowed),
    );
    // What is typed is for the group in front, and for nobody else who
    // happens to hold the terminal — as this does.
    let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_HANDLE);
    let _ = syscall::sys_sig_take(None);
    syscall::sleep_ticks(10);
    let _ = syscall::sys_fd_write_nb(master, b"\x03");
    check(
        "Ctrl-C ends the program in front of the terminal",
        syscall::sys_wait_job(leader_pid, 0) == Ok(Some(ChildNews::Ended(leader_pid, -2))),
    );
    check("and is not for whoever else has it open", syscall::sys_sig_take(None) == 0);
    // Its leader gone, the terminal is nobody's, and is as it was before
    // anybody claimed it: what is typed is for whoever holds it.
    let _ = syscall::sys_fd_write_nb(master, b"\x03");
    check(
        "a terminal whose session has ended is nobody's again",
        syscall::sys_sig_take(None) == bit(syscall::SIGINT),
    );
    let _ = syscall::sys_sig_action(syscall::SIGTSTP, syscall::SIG_HANDLE);
    let _ = syscall::sys_fd_write_nb(master, b"\x1a");
    check("and Ctrl-Z raises signal 20 there", syscall::sys_sig_take(None) == bit(syscall::SIGTSTP));
    for signo in [syscall::SIGINT, syscall::SIGTSTP] {
        let _ = syscall::sys_sig_action(signo, syscall::SIG_DEFAULT);
    }
    let _ = syscall::sys_sig_take(None);
    for fd in [says, slave, master] {
        let _ = syscall::sys_fd_close(fd);
    }
}

/// Sleep a little, then write. Run on a thread so that something can become
/// ready while the main task is blocked in a wait — which is the whole of what
/// Task 8 adds, and cannot be tested from one task.
extern "C" fn waker() -> ! {
    syscall::sleep_ticks(5);
    let _ = syscall::sys_fd_write(WAKER_FD, b"wake");
    syscall::sys_exit_code(0);
}

fn test_wake_latency() {
    println!("waiting wakes promptly:");
    let (a, b) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => { check("a pair", false); return; }
    };
    let set = match syscall::sys_pollset_create() {
        Ok(s) => s,
        Err(()) => { check("a set", false); return; }
    };
    let _ = syscall::sys_pollset_add(set, b, syscall::POLL_READABLE, 1);

    // Data already waiting: a correct wait returns without sleeping at all.
    let _ = syscall::sys_fd_write(a, b"now");
    let before = syscall::sys_ticks();
    let mut ready = [syscall::Ready::empty(); 2];
    let n = syscall::sys_pollset_wait(set, &mut ready, 100);
    let elapsed = syscall::sys_ticks() - before;
    check("data already waiting returns at once", n == Ok(1) && elapsed <= 1);

    let mut buf = [0u8; 8];
    let _ = syscall::sys_fd_read(b, &mut buf);

    // Nothing to read: this must run its full timeout and not return early.
    let before = syscall::sys_ticks();
    let n = syscall::sys_pollset_wait(set, &mut ready, 20);
    let elapsed = syscall::sys_ticks() - before;
    check("an empty wait runs its full timeout", n == Ok(0) && elapsed >= 20);

    // And the one that matters: something becomes ready *while* we are
    // blocked. Without a wake path the wait sleeps its whole timeout and only
    // then notices, so the check is on the clock and not on the answer.
    let Ok(t) = thread::spawn_with_stack(waker, 8) else {
        check("start a thread to wake us", false);
        return;
    };
    check("start a thread to wake us", true);
    if syscall::sys_fd_dup(t.tid(), WAKER_FD, a).is_err() {
        check("give it the other end", false);
        return;
    }
    check("give it the other end", true);

    let before = syscall::sys_ticks();
    let n = syscall::sys_pollset_wait(set, &mut ready, 300);
    let elapsed = syscall::sys_ticks() - before;
    check("woken by the write, not by the deadline", n == Ok(1) && elapsed < 100);

    let _ = syscall::sys_fd_read(b, &mut buf);
    let _ = syscall::sys_fd_close(set);
    let _ = syscall::sys_fd_close(a);
    let _ = syscall::sys_fd_close(b);
}

fn test_poll() {
    println!("one-shot poll:");
    let (a, b) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => { check("a pair", false); return; }
    };

    // `a` is writable and `b` is not readable, so exactly one fires — and it
    // has to land in the right entry, which is what revents is for.
    let mut fds = [
        syscall::PollFd::new(b, syscall::POLL_READABLE),
        syscall::PollFd::new(a, syscall::POLL_WRITABLE),
    ];
    check("the writable end fires", syscall::sys_poll(&mut fds, 50) == Ok(1));
    check(
        "and it is the second entry",
        fds[1].revents & syscall::POLL_WRITABLE != 0,
    );
    check("the first reports nothing", fds[0].revents == 0);

    // Nothing ready: run the timeout rather than returning early.
    let mut fds = [syscall::PollFd::new(b, syscall::POLL_READABLE)];
    let before = syscall::sys_ticks();
    let n = syscall::sys_poll(&mut fds, 15);
    let elapsed = syscall::sys_ticks() - before;
    check("nothing ready times out", n == Ok(0) && elapsed >= 15);

    let _ = syscall::sys_fd_write(a, b"z");
    let mut fds = [syscall::PollFd::new(b, syscall::POLL_READABLE)];
    check("after a write it is readable", syscall::sys_poll(&mut fds, 50) == Ok(1));

    // A descriptor that cannot be waited on is reported as invalid rather than
    // failing the whole call, which is what poll(2) does.
    let mut fds = [
        syscall::PollFd::new(30, syscall::POLL_READABLE),
        syscall::PollFd::new(b, syscall::POLL_READABLE),
    ];
    let n = syscall::sys_poll(&mut fds, 50);
    check("an unwaitable descriptor is reported, not fatal", n == Ok(2));
    check(
        "and it is marked invalid",
        fds[0].revents & syscall::POLL_INVALID != 0,
    );
    check("while the good one still reports", fds[1].revents & syscall::POLL_READABLE != 0);

    let mut buf = [0u8; 4];
    let _ = syscall::sys_fd_read(b, &mut buf);
    let _ = syscall::sys_fd_close(a);
    let _ = syscall::sys_fd_close(b);
}

fn test_environment() {
    println!("environment:");
    // What the shell puts in every program's environment.
    check("HOME is set", quark_rt::args::getenv(b"HOME").is_some());
    check(
        "and it is a path",
        quark_rt::args::getenv(b"HOME").map(|v| v.starts_with(b"/")) == Some(true),
    );
    check("PATH is set", quark_rt::args::getenv(b"PATH").is_some());
    check("a name nobody set is absent", quark_rt::args::getenv(b"NOPE").is_none());
    // A prefix of a real name must not match it: without checking the `=`,
    // HOM matches HOME=/home/root and returns E=/home/root.
    check("HOM does not match HOME", quark_rt::args::getenv(b"HOM").is_none());
    // The environment sits after the arguments on the same page, so reading it
    // must not have disturbed them.
    check("arguments still readable", quark_rt::args::argv(0).is_some());
    check("and argv[0] is this program", quark_rt::args::argv(0) == Some(&b"dtest"[..]));
}

const CHILD_IMAGE: usize = 0x98_0000_0000;
const THEIR_MEM: usize = 0x99_0000_0000;
const WITNESS: u64 = 0x0D15_EA5E_D15C_0DE5;

static SPAWN_SCRATCH: spawn::Scratch = spawn::Scratch {
    elf: 0x9A_0000_0000,
    stack: 0x9B_0000_0000,
    args: 0x9C_0000_0000,
};

/// Read `/usr/bin/dchild` and load it. Modelled on how `wm` starts a session
/// program: the image comes through the VFS, `spawn::load` builds the address
/// space, and the manifest decides what it is granted.
fn load_child(args: &[&[u8]]) -> Option<spawn::Spawned> {
    load_program(b"/usr/bin/dchild", b"/usr/bin/DCHILD.ELF", args)
}

/// Load a program by either of the names it may have on the disk.
fn load_program(lower: &[u8], upper: &[u8], args: &[&[u8]]) -> Option<spawn::Spawned> {
    let vfs_tid = nameserver::lookup_retry(b"vfs", 20)?;
    // Lowercase for ext2, uppercase with .ELF for FAT32 — the two spellings
    // the shell already tries.
    let grant = |image: &[u8], tid: usize| {
        quark_rt::manifest::grant_image(tid, image, 12);
    };
    let info = spawn::load_path(vfs_tid, lower, CHILD_IMAGE, &SPAWN_SCRATCH, grant)
        .or_else(|()| spawn::load_path(vfs_tid, upper, CHILD_IMAGE, &SPAWN_SCRATCH, grant))
        .ok()?;
    // Every program is started with an argument page; reading one that was
    // never mapped faults.
    spawn::set_args(&info, args, &SPAWN_SCRATCH).ok()?;
    // It needs to be able to reach the nameserver, and somewhere to print.
    let _ = syscall::sys_cap_grant(info.tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT);
    let _ = syscall::sys_fd_dup(info.tid, 1, 1);
    let _ = syscall::sys_fd_dup(info.tid, 2, 2);
    Some(info)
}

static SPACE_VFS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static SPACE_HANDLE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
static SPACE_OF_THREAD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Open a file and leave it open. It belongs to the program, not to this
/// thread, so it has to outlive the thread.
extern "C" fn opener() -> ! {
    use core::sync::atomic::Ordering::SeqCst;
    let me = syscall::sys_getpid() as usize;
    SPACE_OF_THREAD.store(syscall::sys_task_space(me).unwrap_or(0), SeqCst);
    if let Ok((handle, _, _)) = vfs::open(SPACE_VFS.load(SeqCst), b"/etc/passwd") {
        SPACE_HANDLE.store(handle, SeqCst);
    }
    syscall::sys_exit_code(0);
}

/// A program is its address space: its threads are part of it, and what one
/// of them opens is the program's.
fn test_program_is_its_space() {
    use core::sync::atomic::Ordering::SeqCst;
    let me = syscall::sys_getpid() as usize;
    let space = syscall::sys_task_space(me);
    check("a task belongs to a program", space.is_ok_and(|s| s != 0));
    // Looked up first, so the thread starts holding the capability to call it.
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the VFS", false);
        return;
    };
    SPACE_VFS.store(vfs_tid, SeqCst);
    let Ok(t) = thread::spawn_with_stack(opener, 8) else {
        check("start a thread to open a file", false);
        return;
    };
    // Joined before any child is started, since a join reaps whatever exits.
    let _ = t.join();
    check("a thread is part of its program", space == Ok(SPACE_OF_THREAD.load(SeqCst)));
    let handle = SPACE_HANDLE.load(SeqCst);
    let mut got = [0u8; 4];
    check(
        "a file a thread opened outlives the thread",
        handle != usize::MAX && vfs::read(vfs_tid, handle, &mut got, 0) == Ok(4) && &got == b"root",
    );
    if handle != usize::MAX {
        let _ = vfs::close(vfs_tid, handle);
    }
}

fn test_across_address_spaces() {
    println!("across address spaces:");
    test_program_is_its_space();
    let (mine, theirs) = match syscall::sys_socketpair() {
        Ok(p) => p,
        Err(()) => { check("a pair", false); return; }
    };
    let Some(info) = load_child(&[b"dchild"]) else {
        check("load /usr/bin/dchild", false);
        return;
    };
    check("load /usr/bin/dchild", true);
    // The pages the program was read into are a copy the child no longer
    // needs. sys_mmap refuses to map over a page that is still mapped, which
    // makes "was it given back" something a test can ask.
    check(
        "loading a program gives its staging memory back",
        syscall::sys_mmap(CHILD_IMAGE, 1).is_ok(),
    );
    let _ = syscall::sys_munmap(CHILD_IMAGE, 1);

    // Hand the child its end, then drop ours. If an end were a flag rather
    // than a count, this would tell the child's peer the end had gone.
    check(
        "give the child descriptor 3",
        syscall::sys_fd_dup(info.tid, 3, theirs).is_ok(),
    );
    check("drop our copy of it", syscall::sys_fd_close(theirs).is_ok());
    check("the stream is still alive", syscall::sys_fd_write(mine, b"go!\n") == 4);

    if info.start().is_err() {
        check("the child runs", false);
        return;
    }
    check("the child runs", true);
    let me = syscall::sys_getpid() as usize;
    check(
        "a child is another program",
        matches!(
            (syscall::sys_task_space(me), syscall::sys_task_space(info.tid)),
            (Ok(a), Ok(b)) if a != b
        ),
    );

    // Wait for its answer with the set, which is what makes this the whole
    // phase rather than three quarters of it.
    let set = match syscall::sys_pollset_create() {
        Ok(s) => s,
        Err(()) => { check("a set to wait on", false); return; }
    };
    let _ = syscall::sys_pollset_add(set, mine, syscall::POLL_READABLE, 7);
    let mut ready = [syscall::Ready::empty(); 2];
    let n = syscall::sys_pollset_wait(set, &mut ready, 500);
    check(
        "the set wakes for the child's reply",
        n == Ok(1) && ready[0].token == 7,
    );

    let mut buf = [0u8; 8];
    check(
        "bytes and a descriptor arrived",
        syscall::sys_fd_recv(mine, &mut buf, Some(25)) == Ok((4, Some(25))),
    );
    check(
        "map memory the other task allocated",
        syscall::sys_mmap_fd(25, THEIR_MEM).is_ok(),
    );
    check(
        "and read what it wrote there",
        unsafe { core::ptr::read_volatile(THEIR_MEM as *const u64) } == WITNESS,
    );
    // The child has no TaskMgmt over anybody and nobody is calling it, so the
    // kernel must have refused to let it put a capability into this task's
    // CSpace. Filling sixteen slots is a denial of service even though a grant
    // can never raise the authority of the task it lands in.
    check(
        "a task cannot fill another's CSpace",
        unsafe { core::ptr::read_volatile((THEIR_MEM + 128) as *const u64) } == 1,
    );

    // A lock living in memory the two processes share. The child blocks on it
    // in its own address space and is woken from this one, which works only
    // because the kernel keys its futex queue on the physical address.
    let shared = unsafe { &*((THEIR_MEM + 64) as *const sync::Mutex<u64>) };
    let held = shared.lock();
    // The child is now blocked on this. Give it long enough to get there.
    syscall::sleep_ticks(10);
    drop(held);

    // Wait for the child to finish with it.
    let mut waited = 0;
    loop {
        {
            let v = shared.lock();
            if *v == 1 {
                break;
            }
        }
        syscall::sleep_ticks(1);
        waited += 1;
        if waited > 300 {
            break;
        }
    }
    check("a lock in shared memory works between processes", waited <= 300 && *shared.lock() == 1);

    let _ = syscall::sys_fd_close(25);
    let _ = syscall::sys_fd_close(set);
    let _ = syscall::sys_fd_close(mine);
}

/// Pages this task gives away, clear of everything else here.
const GIFT: usize = 0x9D_0000_0000;
/// Somewhere the child has nothing.
const CHILD_SPARE: usize = 0x90_0000_0000;

/// True if nothing is mapped at `at`. sys_mmap refuses to map over a page that
/// is present, which is what makes this a question a program can ask.
fn nothing_at(at: usize) -> bool {
    let free = syscall::sys_mmap(at, 1).is_ok();
    if free {
        let _ = syscall::sys_munmap(at, 1);
    }
    free
}

/// Wait for one particular child, collecting any other on the way.
fn wait_for(tid: usize) -> Option<i32> {
    loop {
        match syscall::sys_wait() {
            Ok((t, code)) if t == tid => return Some(code),
            Ok(_) => continue,
            Err(()) => return None,
        }
    }
}

fn test_spawned_memory() {
    println!("a program's memory is its own:");
    let Some(info) = load_child(&[b"dchild", b"quit"]) else {
        check("load /usr/bin/dchild", false);
        return;
    };
    // The loader builds a program in this task's memory and moves it across.
    // None of it may stay mapped here: a spawner still holding a page could
    // read whatever the frame held next, once the child was gone.
    check("its stack is not left mapped in the parent", nothing_at(SPAWN_SCRATCH.stack));
    check("nor its code", nothing_at(SPAWN_SCRATCH.elf));
    check("nor its arguments", nothing_at(SPAWN_SCRATCH.args));

    // Only memory a task owns can be given. A frame from sys_phys_alloc is
    // mapped without the mapping owning it, and whoever allocated it still
    // answers for it.
    let frame = syscall::sys_phys_alloc(1);
    let lent = frame.is_ok_and(|f| syscall::sys_map_phys(f, GIFT, 1).is_ok());
    check(
        "a frame mapped from elsewhere cannot be given",
        lent && syscall::sys_addrspace_give(info.cr3, CHILD_SPARE, GIFT, 1, 1).is_err(),
    );
    let _ = syscall::sys_munmap(GIFT, 1);
    if let Ok(f) = frame {
        let _ = syscall::sys_phys_free(f, 1);
    }

    let made = syscall::sys_mmap(GIFT, 1).is_ok();
    check(
        "a gift cannot replace a page the child has",
        made && syscall::sys_addrspace_give(
            info.cr3,
            spawn::STACK_TOP - spawn::PAGE_SIZE,
            GIFT,
            1,
            1,
        )
        .is_err(),
    );
    let me = syscall::sys_addrspace_self().unwrap_or(0);
    check(
        "nor go to the address space it came from",
        syscall::sys_addrspace_give(me, CHILD_SPARE, GIFT, 1, 1).is_err(),
    );
    check("a refused gift stays with the giver", syscall::sys_munmap(GIFT, 1) == Ok(1));

    let made = syscall::sys_mmap(GIFT, 1).is_ok();
    check(
        "memory of one's own can be given",
        made && syscall::sys_addrspace_give(info.cr3, CHILD_SPARE, GIFT, 1, 1).is_ok(),
    );
    check("and it leaves the giver", nothing_at(GIFT));

    check("the child runs with its gift", info.start().is_ok() && wait_for(info.tid) == Some(0));

    // A program whose thread exited before it did. Collecting the program
    // orphans the thread, and a dead orphan goes with it: left behind, it
    // named a parent that was gone, whatever took that slot next adopted it,
    // and the address space the two shared stayed allocated until then.
    let orphan = load_child(&[b"dchild", b"orphan"])
        .filter(|child| child.start().is_ok())
        .and_then(|child| wait_for(child.tid))
        .filter(|&tid| tid > 0);
    check("a program leaves a dead thread behind", orphan.is_some());
    check(
        "which is reaped when the program is collected",
        orphan.is_some_and(|tid| syscall::sys_task_info(tid as usize).is_err()),
    );

    // Each run costs a megabyte of stack and the program. A parent that went
    // on holding what it gave its children — or a machine that only reaped
    // them when it next went idle, which a parent doing this never lets it
    // do — is out of memory long before this loop is.
    let mut runs = 0;
    for _ in 0..160 {
        let Some(child) = load_child(&[b"dchild", b"quit"]) else { break };
        if child.start().is_err() || wait_for(child.tid) != Some(0) {
            break;
        }
        runs += 1;
    }
    check("run a program 160 times over", runs == 160);
}

static mut LEND_BUF: [u8; 64] = [0; 64];
static LEND_SERVER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static LEND_GO: sync::Semaphore = sync::Semaphore::new(0);
/// What the lending thread saw. Bit 0: its first call was answered. 1: its
/// second was. 2: an unwritable buffer could not be lent for writing. 3:
/// nothing was lent to it while nobody was calling it.
static LEND_RESULTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The client half of `test_lent_buffers`: lends this task's main thread a
/// buffer three ways.
extern "C" fn lender() -> ! {
    use quark_rt::ipc::Message;
    // Not until main has given this thread the right to call it.
    LEND_GO.acquire();
    let server = LEND_SERVER.load(core::sync::atomic::Ordering::SeqCst);
    let ask = |tag| Message { sender: 0, tag, data: [0; 6] };
    let mut reply = Message::empty();
    let mut results = 0;
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(LEND_BUF) };
    buf[..8].copy_from_slice(b"lent-buf");
    if syscall::sys_call_lend_rw(server, &ask(1), &mut reply, buf).is_ok() {
        results |= 1;
    }
    if syscall::sys_call_lend(server, &ask(2), &mut reply, &buf[..]).is_ok() {
        results |= 2;
    }
    // The argument page is mapped read-only.
    let args = unsafe {
        core::slice::from_raw_parts_mut(quark_rt::args::ARGS_PAGE_ADDR as *mut u8, 16)
    };
    if syscall::sys_call_lend_mut(server, &ask(3), &mut reply, args).is_err() {
        results |= 4;
    }
    let mut probe = [0u8; 1];
    if syscall::sys_lent_read(server, 0, &mut probe).is_err() {
        results |= 8;
    }
    LEND_RESULTS.store(results, core::sync::atomic::Ordering::SeqCst);
    syscall::sys_exit_code(0);
}

fn test_lent_buffers() {
    use quark_rt::ipc::Message;
    println!("lent buffers:");
    let me = syscall::sys_getpid() as usize;
    LEND_SERVER.store(me, core::sync::atomic::Ordering::SeqCst);
    let Ok(t) = thread::spawn_with_stack(lender, 8) else {
        check("start a thread to lend us a buffer", false);
        return;
    };
    let t = t.tid();
    // The thread may call this task: an Endpoint to it, from its creator.
    let granted = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, me as u64, 0)
        .is_ok()
        && syscall::sys_cap_grant_any(t, syscall::SLOT_SCRATCH).is_ok();
    let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
    check("let the thread call us", granted);
    LEND_GO.release();

    let mut msg = Message::empty();
    let mut got = [0u8; 8];
    check("the lending call arrives", syscall::sys_recv(t, &mut msg).is_ok() && msg.tag == 1);
    check(
        "read what was lent",
        syscall::sys_lent_read(t, 0, &mut got) == Ok(8) && &got == b"lent-buf",
    );
    check("write into what was lent", syscall::sys_lent_write(t, 4, b"XY") == Ok(2));
    check("not past its end", syscall::sys_lent_read(t, 60, &mut got).is_err());
    check(
        "not at an offset that wraps",
        syscall::sys_lent_read(t, usize::MAX, &mut got[..1]).is_err(),
    );
    let _ = syscall::sys_reply(t, &Message::empty());
    check(
        "the write landed where it was aimed",
        unsafe { (&*core::ptr::addr_of!(LEND_BUF))[..8] == *b"lentXYuf" },
    );
    // Whatever the thread does next, its second call cannot be further along
    // than waiting to be received.
    check(
        "nothing is lent once the call is answered",
        syscall::sys_lent_read(t, 0, &mut got).is_err(),
    );

    check("a read-only lend arrives", syscall::sys_recv(t, &mut msg).is_ok() && msg.tag == 2);
    check("it can be read", syscall::sys_lent_read(t, 0, &mut got) == Ok(8));
    check("but not written", syscall::sys_lent_write(t, 0, b"Z").is_err());
    let _ = syscall::sys_reply(t, &Message::empty());

    let _ = wait_for(t);
    let results = LEND_RESULTS.load(core::sync::atomic::Ordering::SeqCst);
    check("both lending calls were answered", results & 3 == 3);
    check("an unwritable buffer cannot be lent for writing", results & 4 != 0);
    check("nothing is lent to a task nobody is calling", results & 8 != 0);
}

/// Slots for the endpoint checks. In the range a capability given without a
/// slot lands in, clear of the fixed ones below 16.
const SELF_SLOT: usize = 40;
const STRANGER_SLOT: usize = 41;
const CHILD_SLOT: usize = 42;
const NEXT_CHILD_SLOT: usize = 43;
const THREAD_SLOT: usize = 44;
/// Never filled, so there is nothing in it to offer.
const EMPTY_SLOT: usize = 45;
/// For the child `test_call_storm` calls.
const STORM_SLOT: usize = 46;
/// A task nothing here made or holds a capability to. Not the nameserver:
/// every program is handed one to that.
const INIT_TID: usize = 1;
/// The capability type that named a set of TIDs, withdrawn at ABI 2.0.
const WITHDRAWN_ENDPOINT_SET: u64 = 7;
/// The offering thread's own slots.
const OFFER_SLOT: usize = 8;
const HOLDER_SLOT: usize = 9;
const FOREIGN_SLOT: usize = 10;

fn mint_endpoint(slot: usize, tid: usize) -> bool {
    syscall::sys_cap_mint(slot, syscall::CAP_TYPE_ENDPOINT, tid as u64, 0).is_ok()
}

/// Call `tid` and return the tag it answers with: None if the call cannot be
/// made, or nobody answers within half a second.
fn call_tag(tid: usize) -> Option<u64> {
    use quark_rt::ipc::Message;
    let mut reply = Message::empty();
    match syscall::sys_call_timeout(tid, &Message::empty(), &mut reply, 50) {
        syscall::CallOutcome::Replied => Some(reply.tag),
        // Which of the two it was matters and the check cannot say: a call
        // that was refused is a capability that is not there, and one that ran
        // out of time is a child that had not reached `sys_recv` half a second
        // after it was started. The second has been seen once, on a machine
        // doing something else at the time, and nothing recorded why.
        other => {
            println!(
                "[dtest] call to {} did not reply: {}",
                tid,
                match other {
                    syscall::CallOutcome::TimedOut => "timed out",
                    _ => "refused",
                }
            );
            None
        }
    }
}

static OFFER_TO: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static OFFER_GO: sync::Semaphore = sync::Semaphore::new(0);
/// What the offering thread saw. Bit 0: holding a capability to main, it could
/// mint another. 1: its offering call was answered. 2: it could not mint one
/// to init, which it neither is, made, nor holds one for.
static OFFER_RESULTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The client half of the offer checks: offers main a capability naming
/// itself, with a call.
extern "C" fn offerer() -> ! {
    use quark_rt::ipc::Message;
    // Not until main has given this thread the right to call it.
    OFFER_GO.acquire();
    let main = OFFER_TO.load(core::sync::atomic::Ordering::SeqCst);
    let me = syscall::sys_getpid() as usize;
    let mut results = 0;
    if mint_endpoint(HOLDER_SLOT, main) {
        results |= 1;
    }
    let ask = Message { sender: 0, tag: 1, data: [0; 6] };
    let mut reply = Message::empty();
    if mint_endpoint(OFFER_SLOT, me)
        && syscall::sys_call_offer(main, &ask, &mut reply, OFFER_SLOT).is_ok()
    {
        results |= 2;
    }
    if !mint_endpoint(FOREIGN_SLOT, INIT_TID) {
        results |= 4;
    }
    OFFER_RESULTS.store(results, core::sync::atomic::Ordering::SeqCst);
    syscall::sys_exit_code(0);
}

fn test_endpoint_objects() {
    use quark_rt::ipc::Message;
    println!("endpoints:");
    let me = syscall::sys_getpid() as usize;
    for slot in SELF_SLOT..=EMPTY_SLOT {
        let _ = syscall::sys_cap_delete(slot);
    }
    // Minting for yourself is ownership; minting for a stranger is not.
    check("a task may mint a capability to itself", mint_endpoint(SELF_SLOT, me));
    check(
        "but not to a task it did not make and cannot call",
        !mint_endpoint(STRANGER_SLOT, INIT_TID),
    );
    check(
        "though it may to one it can call",
        mint_endpoint(STRANGER_SLOT, nameserver::NAMESERVER_TID),
    );
    check(
        "and it records a number, not the task",
        syscall::sys_cap_read(me, SELF_SLOT).is_ok_and(|c| {
            c.cap_type == syscall::CAP_TYPE_ENDPOINT && c.param0 != me as u64 && c.valid
        }),
    );
    let _ = syscall::sys_cap_delete(STRANGER_SLOT);
    // The sets of TIDs these replaced are gone, even the one naming only the
    // caller that anybody could once mint.
    check(
        "a set of task IDs cannot be minted",
        syscall::sys_cap_mint(STRANGER_SLOT, WITHDRAWN_ENDPOINT_SET, 1u64 << me, 0).is_err(),
    );
    let _ = syscall::sys_cap_delete(STRANGER_SLOT);

    // A capability names a task, not the slot it ran in.
    let Some(a) = load_child(&[b"dchild", b"serve"]) else {
        check("start a child to call", false);
        return;
    };
    let _ = a.start();
    check("a creator may mint a capability to its child", mint_endpoint(CHILD_SLOT, a.tid));
    let ask = Message::empty();
    let mut reply = Message::empty();
    check(
        "offering an empty slot is refused",
        syscall::sys_call_offer(a.tid, &ask, &mut reply, EMPTY_SLOT).is_err(),
    );
    check("the capability reaches the child", call_tag(a.tid) == Some(42));
    check("the child answered and exited", wait_for(a.tid) == Some(0));
    check("nobody can mint one to a task that is gone", !mint_endpoint(STRANGER_SLOT, a.tid));
    let Some(b) = load_child(&[b"dchild", b"serve"]) else {
        check("start a second child", false);
        return;
    };
    let _ = b.start();
    check("the next child takes the same slot", b.tid == a.tid);
    check("and the old capability does not reach it", call_tag(b.tid).is_none());
    check("a fresh one is minted", mint_endpoint(NEXT_CHILD_SLOT, b.tid));
    // Giving a task an endpoint it already has costs nothing.
    let first = syscall::sys_cap_grant_any(b.tid, NEXT_CHILD_SLOT);
    let second = syscall::sys_cap_grant_any(b.tid, NEXT_CHILD_SLOT);
    check("a grant to any slot lands at 16 or above", first.is_ok_and(|s| s >= 16));
    check("and the same endpoint again lands in the same slot", first.is_ok() && first == second);
    check("the fresh one reaches the child", call_tag(b.tid) == Some(42));
    let _ = wait_for(b.tid);

    // Offers: a capability travels with a call, and the task called takes it.
    OFFER_TO.store(me, core::sync::atomic::Ordering::SeqCst);
    let Ok(t) = thread::spawn_with_stack(offerer, 8) else {
        check("start a thread to offer us a capability", false);
        return;
    };
    let t = t.tid();
    check(
        "let the thread call us",
        syscall::sys_cap_grant_any(t, SELF_SLOT).is_ok(),
    );
    OFFER_GO.release();
    let mut msg = Message::empty();
    let arrived = syscall::sys_recv_timeout(t, &mut msg, 100).is_ok() && msg.tag == 1;
    check("the offering call arrives", arrived);
    if arrived {
        let taken = syscall::sys_cap_take_any(t);
        check("take what was offered", taken.is_ok_and(|s| s >= 16));
        let taken = taken.unwrap_or(0);
        check("a creator may mint a capability to its thread", mint_endpoint(THREAD_SLOT, t));
        let number = |slot| syscall::sys_cap_read(me, slot).map(|c| (c.cap_type, c.param0));
        check(
            "and what was taken names the same task",
            number(taken).is_ok() && number(taken) == number(THREAD_SLOT),
        );
        check("an offer is taken once", syscall::sys_cap_take_any(t).is_err());
        check(
            "and not from a task that is not calling",
            syscall::sys_cap_take_any(nameserver::NAMESERVER_TID).is_err(),
        );
        let _ = syscall::sys_cap_delete(taken);
        let _ = syscall::sys_reply(t, &Message::empty());
    }
    let _ = wait_for(t);
    let results = OFFER_RESULTS.load(core::sync::atomic::Ordering::SeqCst);
    check("a holder may mint another", results & 1 != 0);
    check("the offering call was answered", results & 2 != 0);
    check("a thread cannot mint one to a stranger", results & 4 != 0);

    for slot in SELF_SLOT..=EMPTY_SLOT {
        let _ = syscall::sys_cap_delete(slot);
    }
}

fn test_runtime_service() {
    println!("a service started at run time:");
    let Some(server) = load_child(&[b"dchild", b"register", b"dchild-svc"]) else {
        check("start a service", false);
        return;
    };
    let _ = server.start();
    // Its registration is what makes it reachable, so wait for that.
    let registered = (0..100).any(|_| {
        nameserver::lookup(b"dchild-svc") == Some(server.tid) || {
            syscall::sleep_ticks(1);
            false
        }
    });
    check("it registers", registered);
    check(
        "a second task cannot take its name",
        nameserver::register(b"dchild-svc").is_err(),
    );
    let Some(client) = load_child(&[b"dchild", b"lookup", b"dchild-svc"]) else {
        check("start a client", false);
        return;
    };
    let _ = client.start();
    // Whichever finishes first: collecting one must not throw the other away.
    let (mut served, mut reached) = (None, None);
    while served.is_none() || reached.is_none() {
        match syscall::sys_wait() {
            Ok((t, code)) if t == server.tid => served = Some(code),
            Ok((t, code)) if t == client.tid => reached = Some(code),
            Ok(_) => {}
            Err(()) => break,
        }
    }
    check("a program it was never introduced to reaches it by name", reached == Some(42));
    check("and the service answered", served == Some(0));
    let gone = (0..100).any(|_| {
        nameserver::lookup(b"dchild-svc").is_none() || {
            syscall::sleep_ticks(1);
            false
        }
    });
    check("its name goes with it", gone);
    check("and can be taken again", nameserver::register(b"dchild-svc").is_ok());

    // Deaths are the kernel's to report. A call dressed as one is a request
    // like any other: answered with an error, and changing nothing.
    use quark_rt::ipc::{TAG_SPACE_DIED, TAG_TASK_DIED};
    let vfs_tid = nameserver::lookup(b"vfs").unwrap_or(0);
    let console = nameserver::lookup(b"console").unwrap_or(0);
    let refused = |tag: Option<u64>| tag == Some(u64::MAX);
    check(
        "the nameserver refuses a death notice from a program",
        refused(forged_death(nameserver::NAMESERVER_TID, TAG_TASK_DIED, vfs_tid as u64)),
    );
    check("and still knows the VFS", vfs_tid != 0 && nameserver::lookup(b"vfs") == Some(vfs_tid));
    if let Some(fb) = nameserver::lookup(b"fb") {
        check(
            "the display refuses one",
            refused(forged_death(fb, TAG_TASK_DIED, console as u64)),
        );
    }
    if let Some(input) = nameserver::lookup(b"input") {
        check(
            "the keyboard refuses one",
            refused(forged_death(input, TAG_TASK_DIED, console as u64)),
        );
    }
    check(
        "the VFS refuses a program's",
        vfs_tid != 0
            && refused(forged_death(vfs_tid, TAG_SPACE_DIED, own_space())),
    );
    // Under a compositor, one naming this program would end its session.
    if let Some(wm) = nameserver::lookup(b"wm") {
        let me = syscall::sys_getpid() as u64;
        check("the compositor refuses one", refused(forged_death(wm, TAG_TASK_DIED, me)));
    }
}

fn own_space() -> u64 {
    syscall::sys_task_space(syscall::sys_getpid() as usize).unwrap_or(0)
}

/// Call `tid` with what looks like the kernel's notice `tag` about `dead`, and
/// return the tag it answers with: None if nobody answers in half a second.
fn forged_death(tid: usize, tag: u64, dead: u64) -> Option<u64> {
    use quark_rt::ipc::Message;
    let forged = Message { sender: 0, tag, data: [dead, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    match syscall::sys_call_timeout(tid, &forged, &mut reply, 50) {
        syscall::CallOutcome::Replied => Some(reply.tag),
        _ => None,
    }
}

/// Start `dchild MODE PATH` with one end of a fresh pair as its descriptor 3,
/// and return it with this end.
fn lock_child(mode: &[u8], path: &[u8]) -> Option<(spawn::Spawned, usize)> {
    let (mine, theirs) = syscall::sys_socketpair().ok()?;
    let child = load_child(&[b"dchild", mode, path])?;
    let given = syscall::sys_fd_dup(child.tid, 3, theirs).is_ok();
    let _ = syscall::sys_fd_close(theirs);
    if !given || child.start().is_err() {
        let _ = syscall::sys_fd_close(mine);
        return None;
    }
    Some((child, mine))
}

/// One byte from `fd`, which a child writes when it has done something.
fn child_says(fd: usize) -> Option<u8> {
    let mut b = [0u8; 1];
    (syscall::sys_fd_read(fd, &mut b) == 1).then_some(b[0])
}

fn test_locks() {
    println!("locks across programs:");
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the VFS", false);
        return;
    };
    const FILE: &[u8] = b"/tmp/dtest-lock";
    if let Ok(o) = vfs::open_with(vfs_tid, FILE, vfs::OPEN_CREATE) {
        let _ = vfs::close(vfs_tid, o.handle);
    }
    let Ok((h, _, _)) = vfs::open(vfs_tid, FILE) else {
        check("open a file to lock", false);
        return;
    };
    let (ex, wait, query) = (vfs::LOCK_EXCLUSIVE, vfs::LOCK_WAIT, vfs::LOCK_QUERY);

    // Another program's lock keeps this one out, until that program has gone.
    match lock_child(b"lock", FILE) {
        Some((child, mine)) => {
            check("a child takes a lock", child_says(mine) == Some(b'L'));
            check(
                "which keeps this program out",
                vfs::lock(vfs_tid, h, ex, 0, 0, 0).err() == Some(vfs::ERR_WOULD_BLOCK),
            );
            let theirs = syscall::sys_task_space(child.tid).unwrap_or(0);
            check(
                "and a query names the child",
                vfs::lock(vfs_tid, h, ex, 0, 0, query).is_ok_and(|a| a[0] == ex && a[3] == theirs),
            );
            let _ = syscall::sys_fd_close(mine);
            check("the child lets go and exits", wait_for(child.tid) == Some(0));
            check("and its lock went with it", vfs::lock(vfs_tid, h, ex, 0, 0, 0).is_ok());
            let _ = vfs::lock(vfs_tid, h, vfs::LOCK_UNLOCK, 0, 0, 0);
        }
        None => check("a child takes a lock", false),
    }

    // Two programs each waiting for what the other holds.
    let _ = vfs::lock(vfs_tid, h, ex, 0, 1, 0);
    match lock_child(b"lock2", FILE) {
        Some((child, mine)) => {
            check("a child takes byte 1", child_says(mine) == Some(b'1'));
            // Its next call waits for byte 0, which this program holds.
            let blocked = (0..100).any(|_| {
                syscall::sys_task_info(child.tid).is_ok_and(|(state, _, _)| state == 2) || {
                    syscall::sleep_ticks(1);
                    false
                }
            });
            syscall::sleep_ticks(5);
            check("and waits for byte 0", blocked);
            check(
                "waiting for byte 1 would never end",
                vfs::lock(vfs_tid, h, ex, 1, 1, wait).err() == Some(vfs::ERR_DEADLOCK),
            );
            let _ = vfs::lock(vfs_tid, h, vfs::LOCK_UNLOCK, 0, 1, 0);
            check("letting go of byte 0 lets the child in", child_says(mine) == Some(b'2'));
            let _ = syscall::sys_fd_close(mine);
            let _ = wait_for(child.tid);
        }
        None => check("a child takes byte 1", false),
    }
    let _ = vfs::close(vfs_tid, h);
    let _ = vfs::unlink(vfs_tid, FILE);
}

/// Where the memory section reserves its gigabyte.
const LAZY: usize = 0xA0_0000_0000;
const LAZY_PAGES: usize = 262_144;

fn test_memory() {
    println!("memory on demand:");
    let (free0, charged0) = syscall::sys_mem_info();
    check("a gigabyte is reserved", syscall::sys_map_anon(LAZY, LAZY_PAGES, false).is_ok());
    let (free1, charged1) = syscall::sys_mem_info();
    check(
        "and costs a page table at most",
        charged1 == charged0 && free0.saturating_sub(free1) <= 2,
    );
    check("reserving it again is refused", syscall::sys_map_anon(LAZY, 1, false).is_err());
    check("and so is mapping over it", syscall::sys_mmap(LAZY + 4096, 1).is_err());
    // Sixteen pages, far apart, each in a reservation of its own until now.
    let page = |i: usize| LAZY + i * 16_000 * 4096;
    for i in 0..16 {
        unsafe { core::ptr::write_volatile(page(i) as *mut u8, i as u8 + 1) };
    }
    let (free2, charged2) = syscall::sys_mem_info();
    check("touching sixteen pages charges sixteen", charged2 == charged1 + 16);
    check("and takes at least sixteen frames", free1.saturating_sub(free2) >= 16);
    check(
        "each keeps what was written",
        (0..16).all(|i| unsafe { core::ptr::read_volatile(page(i) as *const u8) } == i as u8 + 1),
    );

    // The kernel copies out of a page nothing has touched.
    let untouched = unsafe { core::slice::from_raw_parts((LAZY + 1000 * 4096 + 7) as *const u8, 64) };
    let written = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs_tid| {
        let o = vfs::open_with(vfs_tid, b"/dev/null", 0).ok()?;
        let n = vfs::write(vfs_tid, o.handle, untouched, 0);
        let _ = vfs::close(vfs_tid, o.handle);
        n.ok()
    });
    check("an untouched page can be lent", written == Some(64));

    for chunk in (0..LAZY_PAGES).step_by(256) {
        let _ = syscall::sys_munmap(LAZY + chunk * 4096, 256);
    }
    check("unmapping gives the charge back", syscall::sys_mem_info().1 == charged0);
    check("and the range is free again", syscall::sys_mmap(LAZY, 1).is_ok());
    let _ = syscall::sys_munmap(LAZY, 1);

    // A file written through a shared mapping, by a program that then exits
    // without asking for it to be written back: it is written back anyway.
    const MAPPED: &[u8] = b"/tmp/dtest-map";
    let shared = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs_tid| {
        let o = vfs::open_with(vfs_tid, MAPPED, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE).ok()?;
        let _ = vfs::truncate(vfs_tid, o.handle, 4096);
        let _ = vfs::close(vfs_tid, o.handle);
        let child = load_child(&[b"dchild", b"mapwrite", MAPPED])?;
        let _ = vfs::give_cwd(vfs_tid, child.tid);
        let _ = child.start();
        let code = wait_for(child.tid);
        let (h, _, _) = vfs::open(vfs_tid, MAPPED).ok()?;
        let mut got = [0u8; 14];
        let n = vfs::read(vfs_tid, h, &mut got, 0);
        let _ = vfs::close(vfs_tid, h);
        let _ = vfs::unlink(vfs_tid, MAPPED);
        Some((code, n, got))
    });
    check(
        "a child writes a file through a shared mapping",
        matches!(shared, Some((Some(0), _, _))),
    );
    check(
        "and the file has it once the child has gone",
        matches!(shared, Some((_, Ok(14), got)) if &got == b"from the child"),
    );

    // A program that takes more than it may is stopped, and gives it all back.
    let hog = load_child(&[b"dchild", b"hog"]).map(|c| {
        let _ = syscall::sys_set_mem_limit(c.tid, 2048);
        let _ = c.start();
        wait_for(c.tid)
    });
    check("a program past its limit ends with SIGBUS", hog == Some(Some(-7)));
    let (free3, _) = syscall::sys_mem_info();
    check("and its memory comes back", free3 + 64 >= free0);
}

fn test_random() {
    println!("random numbers:");
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    check("the kernel fills a buffer", syscall::sys_getrandom(&mut a) == Ok(32));
    check("and another, differently", syscall::sys_getrandom(&mut b) == Ok(32) && a != b);
    check("with something other than zeroes", a.iter().any(|&x| x != 0));
    check("an empty request is answered", syscall::sys_getrandom(&mut []) == Ok(0));
    // A buffer the caller cannot write is refused, not written.
    let bad = unsafe { core::slice::from_raw_parts_mut(0x1000 as *mut u8, 16) };
    check("a buffer that is not the caller's is refused", syscall::sys_getrandom(bad).is_err());
    let mut big = [0u8; 5000];
    check(
        "the runtime fills more than a page",
        quark_rt::random::fill(&mut big).is_ok() && big[4096..].iter().any(|&x| x != 0),
    );
}

/// `dir`/entry-NN-nnn…, the name 100 bytes long. Returns the path's length.
fn listing_entry(buf: &mut [u8; 160], dir: &[u8], i: usize) -> usize {
    buf[..dir.len()].copy_from_slice(dir);
    let mut n = dir.len();
    buf[n..n + 7].copy_from_slice(b"/entry-");
    n += 7;
    buf[n] = b'0' + (i / 10) as u8;
    buf[n + 1] = b'0' + (i % 10) as u8;
    buf[n + 2] = b'-';
    n += 3;
    let end = dir.len() + 1 + 100;
    buf[n..end].fill(b'n');
    end
}

fn test_files() {
    println!("files:");
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the VFS", false);
        return;
    };
    // Paths are lent, so their length is the filesystem's business.
    let dir: &[u8] = b"/tmp/dtest-a-directory-whose-name-alone-is-past-the-old-limit";
    let file: &[u8] = b"/tmp/dtest-a-directory-whose-name-alone-is-past-the-old-limit/and-a-file";
    check(
        "make a directory with a long path",
        matches!(vfs::mkdir(vfs_tid, dir), Ok(()) | Err(vfs::ERR_EXISTS)),
    );
    if let Ok(o) = vfs::open_with(vfs_tid, file, vfs::OPEN_CREATE) {
        let _ = vfs::write(vfs_tid, o.handle, b"long paths", 0);
        let _ = vfs::close(vfs_tid, o.handle);
    }
    let again = vfs::open_with(vfs_tid, file, vfs::OPEN_CREATE);
    check("creating a file again opens it", again.as_ref().is_ok_and(|o| o.size == 10 && !o.is_dir));
    let other = vfs::open(vfs_tid, b"/etc/passwd");
    if let (Ok(a), Ok((b, _, _))) = (&again, &other) {
        let ids = (vfs::stat_full(vfs_tid, a.handle), vfs::stat_full(vfs_tid, *b));
        check(
            "stat names the inode, not the handle",
            matches!(ids, (Ok(x), Ok(y)) if x.id == a.id && x.id != y.id && x.links >= 1),
        );
    }
    for h in [again.map(|o| o.handle), other.map(|o| o.0)].into_iter().flatten() {
        let _ = vfs::close(vfs_tid, h);
    }
    check(
        "creating it exclusively fails",
        vfs::open_with(vfs_tid, file, vfs::OPEN_CREATE | vfs::OPEN_EXCLUSIVE).err() == Some(vfs::ERR_EXISTS),
    );
    check(
        "a file is not a directory",
        vfs::open_with(vfs_tid, file, vfs::OPEN_DIRECTORY).err() == Some(vfs::ERR_NOT_DIR),
    );
    let mut long = [b'y'; 300];
    long[..5].copy_from_slice(b"/tmp/");
    check(
        "a name past 255 bytes is refused",
        vfs::open_with(vfs_tid, &long, vfs::OPEN_CREATE).err() == Some(vfs::ERR_NAME_TOO_LONG),
    );
    // Removing, renaming and shortening, and the directory made above goes.
    let moved: &[u8] = b"/tmp/dtest-a-directory-whose-name-alone-is-past-the-old-limit/renamed";
    let _ = vfs::unlink(vfs_tid, moved);
    check("rename a file", vfs::rename(vfs_tid, file, moved).is_ok());
    check("the old name is gone", vfs::open(vfs_tid, file).err() == Some(vfs::ERR_NOT_FOUND));
    if let Ok(o) = vfs::open_with(vfs_tid, moved, 0) {
        check(
            "shorten it through a handle",
            vfs::truncate(vfs_tid, o.handle, 4).is_ok()
                && vfs::stat_full(vfs_tid, o.handle).is_ok_and(|s| s.size == 4),
        );
        let _ = vfs::close(vfs_tid, o.handle);
    }
    if let Ok(o) = vfs::open_with(vfs_tid, file, vfs::OPEN_CREATE) {
        let _ = vfs::close(vfs_tid, o.handle);
    }
    let replaced = vfs::rename(vfs_tid, moved, file).is_ok()
        && vfs::open_with(vfs_tid, file, 0).is_ok_and(|o| {
            let _ = vfs::close(vfs_tid, o.handle);
            o.size == 4
        });
    check("rename onto a name replaces what had it", replaced);
    // A second name is the same file.
    let _ = vfs::unlink(vfs_tid, moved);
    check("link a second name", vfs::link(vfs_tid, file, moved).is_ok());
    let names = (vfs::open_with(vfs_tid, file, 0), vfs::open_with(vfs_tid, moved, 0));
    if let (Ok(a), Ok(b)) = &names {
        let stats = (vfs::stat_full(vfs_tid, a.handle), vfs::stat_full(vfs_tid, b.handle));
        check(
            "both names are one file with two links",
            matches!(stats, (Ok(x), Ok(y)) if x.id == y.id && x.links == 2 && y.links == 2),
        );
    } else {
        check("both names are one file with two links", false);
    }
    for o in [names.0, names.1].into_iter().flatten() {
        let _ = vfs::close(vfs_tid, o.handle);
    }
    check(
        "a directory has one name",
        vfs::link(vfs_tid, dir, b"/tmp/dtest-dir-link").err() == Some(vfs::ERR_IS_DIR),
    );
    check("and the second name goes", vfs::unlink(vfs_tid, moved).is_ok());
    check(
        "a directory with something in it stays",
        vfs::rmdir(vfs_tid, dir).err() == Some(vfs::ERR_NOT_EMPTY),
    );
    check("unlink a file", vfs::unlink(vfs_tid, file).is_ok());
    check(
        "then the directory can go",
        vfs::rmdir(vfs_tid, dir).is_ok() && vfs::open(vfs_tid, dir).err() == Some(vfs::ERR_NOT_FOUND),
    );

    // A working directory: relative names start there, and a child is given
    // it, or starts at the root.
    check("chdir to /etc", vfs::chdir(vfs_tid, b"/etc").is_ok());
    check(
        "a relative name opens from there",
        vfs::open(vfs_tid, b"passwd").map(|(h, _, _)| vfs::close(vfs_tid, h)).is_ok(),
    );
    let mut here = [0u8; 64];
    check(
        "getcwd says /etc",
        vfs::getcwd(vfs_tid, &mut here).is_ok_and(|n| &here[..n] == b"/etc"),
    );
    let given = load_child(&[b"dchild", b"cwd"]).map(|c| {
        let _ = vfs::give_cwd(vfs_tid, c.tid);
        let _ = c.start();
        wait_for(c.tid)
    });
    check("a child given the directory starts there", given == Some(Some(0)));
    let not_given = load_child(&[b"dchild", b"cwd"]).map(|c| {
        let _ = c.start();
        wait_for(c.tid)
    });
    check("one not given it starts at the root", not_given == Some(Some(1)));
    check(
        "nobody else's child can be given it",
        vfs::give_cwd(vfs_tid, nameserver::NAMESERVER_TID).err() == Some(vfs::ERR_PERMISSION),
    );
    check(
        "a file is not a directory to be in",
        vfs::chdir(vfs_tid, b"/etc/passwd").err() == Some(vfs::ERR_NOT_DIR),
    );
    check("and back to /", vfs::chdir(vfs_tid, b"/").is_ok());

    // A directory read a page at a time, with names longer than a page's
    // fixed entries used to hold.
    const LISTING: &[u8] = b"/tmp/dtest-listing";
    let _ = vfs::mkdir(vfs_tid, LISTING);
    let mut path = [0u8; 160];
    let mut made = 0;
    for i in 0..80 {
        let n = listing_entry(&mut path, LISTING, i);
        if let Ok(o) = vfs::open_with(vfs_tid, &path[..n], vfs::OPEN_CREATE) {
            let _ = vfs::close(vfs_tid, o.handle);
            made += 1;
        }
    }
    check("make 80 files with 100-byte names", made == 80);
    let mut seen = [false; 80];
    let mut listed = 0;
    if let Ok((h, _, _)) = vfs::open(vfs_tid, LISTING) {
        let mut out = [vfs::DirEntry::empty(); 16];
        let mut next = 0u64;
        loop {
            let Ok(page) = vfs::readdir_bulk(vfs_tid, h, next, &mut out) else {
                break;
            };
            for e in &out[..page.count] {
                let name = e.name_bytes();
                if name.len() == 100 && name.starts_with(b"entry-") {
                    let i = ((name[6] - b'0') * 10 + (name[7] - b'0')) as usize;
                    if i < 80 && !seen[i] {
                        seen[i] = true;
                        listed += 1;
                    }
                }
            }
            next = page.next;
            if page.end || page.count == 0 {
                break;
            }
        }
        let _ = vfs::close(vfs_tid, h);
    }
    check("list all of them, sixteen at a time", listed == 80);

    // Space: a 64 KiB file takes it, and gives it back.
    let big: &[u8] = b"/tmp/dtest-listing/big";
    let before = vfs::statfs(vfs_tid).map(|s| s.free_blocks);
    if let Ok(o) = vfs::open_with(vfs_tid, big, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE) {
        let chunk = [0x5Au8; 4096];
        for i in 0..16u32 {
            let _ = vfs::write(vfs_tid, o.handle, &chunk, i * 4096);
        }
        let _ = vfs::close(vfs_tid, o.handle);
    }
    let during = vfs::statfs(vfs_tid).map(|s| s.free_blocks);
    let _ = vfs::unlink(vfs_tid, big);
    let after = vfs::statfs(vfs_tid).map(|s| s.free_blocks);
    let block = vfs::statfs(vfs_tid).map_or(1024, |s| s.block_size);
    check(
        "a 64 KiB file takes 64 KiB",
        matches!((before, during), (Ok(b), Ok(d)) if b >= d + 65536 / block),
    );
    check("and gives it back when it goes", before.is_ok() && before == after);
    for i in 0..80 {
        let n = listing_entry(&mut path, LISTING, i);
        let _ = vfs::unlink(vfs_tid, &path[..n]);
    }
    check("and the directory empties and goes", vfs::rmdir(vfs_tid, LISTING).is_ok());

    // A program that exits holding files gives them back. Two of these hold
    // more handles between them than the table has room for.
    for _ in 0..2 {
        let Some(child) = load_child(&[b"dchild", b"hold", b"100"]) else {
            check("start a program that holds files", false);
            return;
        };
        let _ = child.start();
        check("it opened a hundred files", wait_for(child.tid) == Some(100));
    }
    let mut held = [0usize; 60];
    let mut n = 0;
    for slot in held.iter_mut() {
        if let Ok((h, _, _)) = vfs::open(vfs_tid, b"/etc/passwd") {
            *slot = h;
            n += 1;
        }
    }
    check("and their handles went when they did", n == 60);
    for &h in &held[..n] {
        let _ = vfs::close(vfs_tid, h);
    }
}

static LOCK: sync::Mutex<u32> = sync::Mutex::new(0);
static COND: sync::Condvar = sync::Condvar::new();
static ONCE: sync::Once = sync::Once::new();
static ONCE_RAN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static SEM: sync::Semaphore = sync::Semaphore::new(0);
static RW: sync::RwLock<u32> = sync::RwLock::new(7);

/// Take the lock, change the value, and say so — after a delay, so the main
/// task is genuinely blocked rather than arriving second.
extern "C" fn sync_worker() -> ! {
    syscall::sleep_ticks(5);
    {
        let mut held = LOCK.lock();
        *held = 99;
        COND.notify_one();
    }
    SEM.release();
    syscall::sys_exit_code(0);
}

/// A disk driver's volumes, and who may have one.
///
/// Asked of the first disk as an image built on another machine lays it
/// out: an EFI partition, then the root, which the file server has. On a
/// system running from memory there is no such disk, and nothing to ask.
fn test_disks() {
    use quark_rt::block;
    println!("disks:");
    let disk = nameserver::lookup(b"disk0");
    let whole = disk.and_then(|d| block::info(d, 0).ok());
    let (Some(disk), Some(whole)) = (disk, whole) else {
        println!("  (no disk0 here; nothing to ask)");
        return;
    };
    if whole.volumes < 3 {
        println!("  (disk0 is not laid out as a root after an EFI partition; nothing to ask)");
        return;
    }
    check("a disk is a volume, and has more", whole.sectors > 0 && whole.kind == block::KIND_WHOLE);
    let efi = block::info(disk, 1);
    let root = block::info(disk, 2);
    check(
        "its first partition is the EFI one, and nobody has it",
        efi.is_ok_and(|v| v.kind == block::KIND_EFI && v.claimant == 0 && v.start > 0),
    );
    let vfs_pid = nameserver::lookup(b"vfs").and_then(syscall::sys_pid).unwrap_or(0);
    check(
        "its second is the file server's",
        root.is_ok_and(|v| v.kind == block::KIND_DATA && v.claimant == vfs_pid && vfs_pid != 0),
    );
    check(
        "each lies inside the disk, one after the other",
        matches!((efi, root), (Ok(a), Ok(b)) if a.start + a.sectors <= b.start && b.start + b.sectors <= whole.sectors),
    );
    check("a volume that is not there is not there", block::info(disk, 16) == Err(block::ERR_NO_VOLUME));

    let mut sector = [0u8; 512];
    let last = efi.map_or(0, |v| v.sectors);
    check(
        "root reads a volume it has not claimed",
        block::read(disk, 1, last - 1, &mut sector).is_ok(),
    );
    // What was just read, written back: if this were answered, nothing
    // would have changed.
    check(
        "and does not write one",
        block::write(disk, 1, last - 1, &sector) == Err(block::ERR_NOT_CLAIMANT)
            && block::write(disk, 2, 0, &sector) == Err(block::ERR_NOT_CLAIMANT),
    );
    check("the file server's is not anybody else's to claim", block::claim(disk, 2) == Err(block::ERR_BUSY));
    check(
        "nor is the whole disk, which is the same sectors",
        block::claim(disk, 0) == Err(block::ERR_BUSY),
    );
    check("a partition nobody has can be claimed", block::claim(disk, 1).is_ok());
    check(
        "and read: the EFI partition begins as a FAT filesystem does",
        block::read(disk, 1, 0, &mut sector).is_ok() && sector[510] == 0x55 && sector[511] == 0xAA,
    );
    check(
        "to its last sector and not past it",
        block::read(disk, 1, last - 1, &mut sector).is_ok()
            && block::read(disk, 1, last, &mut sector) == Err(block::ERR_RANGE),
    );
    check(
        "the partition table is read again only for whoever has the whole disk",
        block::rescan(disk) == Err(block::ERR_NOT_CLAIMANT),
    );
    check("it is let go", block::release(disk, 1).is_ok());
    check("and is then nobody's again", block::info(disk, 1).is_ok_and(|v| v.claimant == 0));
}

/// The RAM disk `ramdisk 4` has just made: the one of `ram0`..`ram7` that is
/// four megabytes and nobody's.
fn new_ram_disk() -> Option<usize> {
    ram_disk_of(8192).map(|(tid, _)| tid)
}

/// The RAM disk of `sectors` sectors that nobody has, and what it is called.
fn ram_disk_of(sectors: u64) -> Option<(usize, [u8; 4])> {
    use quark_rt::block;
    let mut name = *b"ram0";
    (b'0'..=b'7').find_map(|digit| {
        name[3] = digit;
        let tid = nameserver::lookup(&name)?;
        matches!(block::info(tid, 0), Ok(i) if i.sectors == sectors && i.claimant == 0)
            .then_some((tid, name))
    })
}

/// The CRC a GPT is checked with.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Run `/usr/bin/parts` with `args` and say how it ended.
fn parts(args: &[&[u8]]) -> Option<i32> {
    let mut argv: [&[u8]; 6] = [b"parts"; 6];
    argv[1..1 + args.len()].copy_from_slice(args);
    let child = load_program(b"/usr/bin/parts", b"/usr/bin/PARTS.ELF", &argv[..1 + args.len()])?;
    let tid = child.tid;
    child.start().ok()?;
    wait_for(tid)
}

/// A partition table made by `parts`, read back off the disk it was made on
/// and checked the way firmware would check it.
fn test_parts() {
    use quark_rt::block;
    println!("a partition table:");
    const SECTORS: u64 = 16 * 2048;
    let server = load_program(b"/usr/bin/ramdisk", b"/usr/bin/RAMDISK.ELF", &[b"ramdisk", b"16"])
        .and_then(|c| {
            let tid = c.tid;
            c.start().ok().map(|()| tid)
        });
    let Some(server) = server else {
        check("start a RAM disk", false);
        return;
    };
    let mut found = None;
    for _ in 0..50 {
        found = ram_disk_of(SECTORS);
        if found.is_some() {
            break;
        }
        syscall::sleep_ticks(2);
    }
    let Some((disk, name)) = found else {
        check("a RAM disk of sixteen megabytes appears", false);
        let _ = syscall::sys_task_kill(server);
        let _ = wait_for(server);
        return;
    };

    check("a partition is refused a disk with no table", parts(&[&name, b"new", b"root"]) == Some(1));
    check("a table is made", parts(&[&name, b"init"]) == Some(0));
    check("an EFI partition of four megabytes", parts(&[&name, b"new", b"efi", b"4M"]) == Some(0));
    check("and a root in what is left", parts(&[&name, b"new", b"root"]) == Some(0));
    check("after which there is no room for another", parts(&[&name, b"new", b"data"]) == Some(1));
    check("and no such type as that", parts(&[&name, b"new", b"swap"]) == Some(2));

    // The driver has been told to look, and has.
    let (efi, root) = (block::info(disk, 1), block::info(disk, 2));
    check(
        "the disk now has two partitions",
        block::info(disk, 0).is_ok_and(|v| v.volumes == 3),
    );
    check(
        "the first is the EFI one, on the first megabyte, four long",
        efi.is_ok_and(|v| v.kind == block::KIND_EFI && v.start == 2048 && v.sectors == 4 * 2048),
    );
    // What is left after the tables at each end, in whole megabytes.
    let rest = (SECTORS - 34 - (2048 + 4 * 2048) + 1) / 2048 * 2048;
    check(
        "the second follows it and takes the rest, in whole megabytes",
        root.is_ok_and(|v| v.kind == block::KIND_DATA && v.start == 5 * 2048 && v.sectors == rest),
    );

    // The table itself, as it is on the disk.
    let mut header = [0u8; 512];
    let mut backup = [0u8; 512];
    let mut sector = [0u8; 512];
    let word = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let long = |b: &[u8], at: usize| word(b, at) as u64 | (word(b, at + 4) as u64) << 32;
    let sound = |h: &[u8; 512]| {
        let mut copy = *h;
        copy[16..20].fill(0);
        &h[..8] == b"EFI PART" && word(h, 12) == 92 && crc32(&copy[..92]) == word(h, 16)
    };
    let read = block::read(disk, 0, 1, &mut header).is_ok()
        && block::read(disk, 0, SECTORS - 1, &mut backup).is_ok()
        && block::read(disk, 0, 0, &mut sector).is_ok();
    check("the header is a GPT's, and its checksum is right", read && sound(&header));
    check("so is the copy at the end of the disk", read && sound(&backup));
    check(
        "each says where the other is",
        long(&header, 24) == 1
            && long(&header, 32) == SECTORS - 1
            && long(&backup, 24) == SECTORS - 1
            && long(&backup, 32) == 1,
    );
    check(
        "and they are the same disk's",
        header[56..72] == backup[56..72] && header[56..72].iter().any(|&b| b != 0),
    );
    check(
        "an MBR in front says the disk is taken",
        sector[446 + 4] == 0xEE && sector[510] == 0x55 && sector[511] == 0xAA,
    );
    // The entries: thirty-two sectors of them, checked as one.
    let entries_ok = |at: u64, want: u32| {
        let mut crc = !0u32;
        let mut piece = [0u8; 4096];
        for i in 0..4 {
            if block::read(disk, 0, at + i * 8, &mut piece).is_err() {
                return false;
            }
            for &byte in piece.iter() {
                crc ^= byte as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
                }
            }
        }
        !crc == want
    };
    check(
        "the entries are where each header says, with the checksum it says",
        long(&header, 72) == 2
            && long(&backup, 72) == SECTORS - 33
            && entries_ok(2, word(&header, 88))
            && entries_ok(SECTORS - 33, word(&backup, 88)),
    );

    // A partition keeps its number when another goes.
    check("the first partition is deleted", parts(&[&name, b"delete", b"1"]) == Some(0));
    check(
        "and the second is still the second",
        block::info(disk, 1) == Err(block::ERR_NO_VOLUME)
            && block::info(disk, 2).is_ok_and(|v| v.start == 5 * 2048),
    );
    // A disk with a partition in use is not repartitioned under it.
    check("a partition is claimed", block::claim(disk, 2).is_ok());
    check("and the table is then not anybody's to change", parts(&[&name, b"init"]) == Some(1));
    let _ = block::release(disk, 2);
    let _ = syscall::sys_task_kill(server);
    let _ = wait_for(server);
}

/// A disk made of memory: the same protocol, and somewhere to write that
/// nothing depends on.
fn test_ram_disk() {
    use quark_rt::block;
    println!("a disk of memory:");
    let started = load_program(b"/usr/bin/ramdisk", b"/usr/bin/RAMDISK.ELF", &[b"ramdisk", b"4"])
        .and_then(|c| {
            let tid = c.tid;
            c.start().ok().map(|()| tid)
        });
    let Some(server) = started else {
        check("start a RAM disk", false);
        return;
    };
    // It registers when it has its memory; give it a moment to.
    let mut disk = None;
    for _ in 0..50 {
        disk = new_ram_disk();
        if disk.is_some() {
            break;
        }
        syscall::sleep_ticks(2);
    }
    let Some(disk) = disk else {
        check("a RAM disk of four megabytes appears", false);
        let _ = syscall::sys_task_kill(server);
        let _ = wait_for(server);
        return;
    };
    check("a RAM disk of four megabytes appears", true);
    check(
        "it is one volume: nothing has written a partition table",
        block::info(disk, 0).is_ok_and(|i| i.volumes == 1 && i.kind == block::KIND_WHOLE),
    );
    let mut sector = [0u8; 512];
    let mut eight = [0u8; 4096];
    check("it is claimed", block::claim(disk, 0).is_ok());
    check(
        "it begins empty",
        block::read(disk, 0, 100, &mut sector).is_ok() && sector.iter().all(|&b| b == 0),
    );
    for (i, b) in eight.iter_mut().enumerate() {
        *b = (i / 512) as u8 + 1;
    }
    check("eight sectors are written at once", block::write(disk, 0, 96, &eight).is_ok());
    check(
        "and each reads back as it was written",
        block::read(disk, 0, 100, &mut sector).is_ok() && sector.iter().all(|&b| b == 5),
    );
    check(
        "the last sector is there and the one after is not",
        block::read(disk, 0, 8191, &mut sector).is_ok()
            && block::read(disk, 0, 8192, &mut sector) == Err(block::ERR_RANGE)
            && block::write(disk, 0, 8190, &eight) == Err(block::ERR_RANGE),
    );
    // A partition table of one's own making: a protective MBR is enough to
    // have a partition, and the driver finds it when asked to look.
    sector = [0u8; 512];
    sector[446 + 4] = 0x83;
    sector[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
    sector[446 + 12..446 + 16].copy_from_slice(&4096u32.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;
    check("a partition table is written", block::write(disk, 0, 0, &sector).is_ok());
    check("the driver reads it when asked", block::rescan(disk) == Ok(2));
    check(
        "and there is a partition where the table says",
        block::info(disk, 1).is_ok_and(|v| v.start == 2048 && v.sectors == 4096 && v.kind == block::KIND_DATA),
    );
    check("which is claimed too", block::claim(disk, 1).is_ok());
    check(
        "its sector 0 is the disk's 2048",
        block::write(disk, 1, 0, &[7u8; 512]).is_ok()
            && block::read(disk, 0, 2048, &mut sector).is_ok()
            && sector.iter().all(|&b| b == 7),
    );
    check(
        "and it ends where it ends",
        block::read(disk, 1, 4096, &mut sector) == Err(block::ERR_RANGE),
    );
    check(
        "the table is not read again while a partition is in use",
        block::rescan(disk) == Err(block::ERR_BUSY),
    );
    let _ = block::release(disk, 1);
    let _ = block::release(disk, 0);
    let _ = syscall::sys_task_kill(server);
    check("the disk goes with its server", wait_for(server) == Some(-9) && new_ram_disk().is_none());
}

static FIFO_VFS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const FIFO: &[u8] = b"/tmp/dtest.fifo";

/// Open the pipe to write a fifth of a second after the test began waiting
/// for somebody to, say something, and go.
extern "C" fn fifo_writer() -> ! {
    use core::sync::atomic::Ordering::SeqCst;
    syscall::sleep_ticks(20);
    if let Ok(w) = vfs::open_fd(FIFO_VFS.load(SeqCst), FIFO, vfs::OPEN_WRITE, 0) {
        let _ = syscall::sys_fd_write(w, b"by name");
        let _ = syscall::sys_fd_close(w);
    }
    syscall::sys_exit_code(0);
}

/// A named pipe: the file server's name for a pipe the kernel keeps.
fn test_named_pipes() {
    use core::sync::atomic::Ordering::SeqCst;
    println!("named pipes:");
    let Some(vfs_tid) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the VFS", false);
        return;
    };
    let nowait = |how: u64| vfs::open_fd(vfs_tid, FIFO, how | vfs::OPEN_NOWAIT, 0);
    let mut buf = [0u8; 16];

    let _ = vfs::unlink(vfs_tid, FIFO);
    check("make one", vfs::mkfifo(vfs_tid, FIFO, 0o600).is_ok());
    check(
        "a second by the same name is refused",
        vfs::mkfifo(vfs_tid, FIFO, 0o600) == Err(vfs::ERR_EXISTS),
    );
    let seen = vfs::open_with(vfs_tid, FIFO, 0);
    check(
        "it is there, and says it is a pipe",
        seen.as_ref().is_ok_and(|o| o.mode & vfs::S_IFMT == vfs::S_IFIFO && o.mode & 0o777 == 0o600),
    );
    if let Ok(o) = seen {
        let _ = vfs::close(vfs_tid, o.handle);
    }

    // The ends, taken by somebody who will not wait for the other.
    check(
        "a writer that will not wait is refused while nobody reads",
        nowait(vfs::OPEN_WRITE) == Err(vfs::ERR_NO_PEER),
    );
    let (Ok(r), w) = (nowait(vfs::OPEN_READ), nowait(vfs::OPEN_WRITE)) else {
        check("a reader that will not wait is given its end", false);
        return;
    };
    check("a reader that will not wait is given its end, and then a writer is", w.is_ok());
    let w = w.unwrap_or(usize::MAX);
    check(
        "they are the two ends of one pipe",
        syscall::sys_fd_write(w, b"same") == 4
            && syscall::sys_fd_read(r, &mut buf) == 4
            && &buf[..4] == b"same",
    );
    check(
        "a second reader is given the same pipe",
        nowait(vfs::OPEN_READ).is_ok_and(|r2| {
            let got = syscall::sys_fd_write(w, b"2") == 1 && syscall::sys_fd_read(r2, &mut buf) == 1;
            let _ = syscall::sys_fd_close(r2);
            got
        }),
    );
    check(
        "opening it for both at once is refused",
        nowait(vfs::OPEN_READ | vfs::OPEN_WRITE) == Err(vfs::ERR_NOT_SUPPORTED),
    );
    // What is in it goes with its last end: the name is somewhere to meet.
    let _ = syscall::sys_fd_write(w, b"left");
    let _ = syscall::sys_fd_close(w);
    let _ = syscall::sys_fd_close(r);
    let (Ok(r), Ok(w)) = (nowait(vfs::OPEN_READ), nowait(vfs::OPEN_WRITE)) else {
        check("both ends again", false);
        return;
    };
    check(
        "what was left in it went with its last end",
        syscall::sys_fd_read_nb(r, &mut buf) == syscall::WOULD_BLOCK,
    );
    let _ = syscall::sys_fd_close(w);
    let mut fds = [syscall::PollFd::new(r, syscall::POLL_READABLE)];
    check(
        "a writer that has gone is the end of it, and a poll says so",
        syscall::sys_poll(&mut fds, 5) == Ok(1) && fds[0].revents & syscall::POLL_HANGUP != 0,
    );
    let _ = syscall::sys_fd_close(r);

    // A reader with no writer *yet* has not been hung up on.
    let Ok((r, wait)) = vfs::open_end(vfs_tid, FIFO, vfs::OPEN_READ, 0) else {
        check("a reader's end, and what to wait on", false);
        return;
    };
    check("a reader's end comes with something to wait on", wait != 0);
    let mut fds = [syscall::PollFd::new(r, syscall::POLL_READABLE)];
    check(
        "before any writer, a poll finds nothing: it has not ended",
        syscall::sys_poll(&mut fds, 5) == Ok(0),
    );
    check(
        "though a read answers as a pipe with no writer does",
        syscall::sys_fd_read_nb(r, &mut buf) == 0,
    );
    // A writer comes and goes before the reader gets round to waiting. It
    // has still been: the wait is for an opening, not for an end to be held.
    if let Ok(w) = nowait(vfs::OPEN_WRITE) {
        let _ = syscall::sys_fd_write(w, b"gone");
        let _ = syscall::sys_fd_close(w);
    }
    check(
        "a writer that came and went before the reader waited has still been",
        syscall::sys_pipe_peer(r, wait) == Ok(()),
    );
    check(
        "and what it left is read, and then the end",
        syscall::sys_fd_read(r, &mut buf) == 4
            && &buf[..4] == b"gone"
            && syscall::sys_fd_read(r, &mut buf) == 0,
    );
    let _ = syscall::sys_fd_close(r);

    // An open that waits.
    FIFO_VFS.store(vfs_tid, SeqCst);
    match thread::spawn_with_stack(fifo_writer, 8) {
        Ok(t) => {
            let before = syscall::sys_ticks();
            let r = vfs::open_fd(vfs_tid, FIFO, vfs::OPEN_READ, 0);
            let waited = syscall::sys_ticks() - before;
            check("opening to read waits for somebody to open it to write", r.is_ok() && waited >= 15);
            if let Ok(r) = r {
                check(
                    "and what the writer says arrives",
                    syscall::sys_fd_read(r, &mut buf) == 7 && &buf[..7] == b"by name",
                );
                let _ = syscall::sys_fd_close(r);
            }
            let _ = t.join();
        }
        Err(_) => check("start a thread to write", false),
    }

    // The kernel's own rules, asked directly.
    check(
        "an end is given only to a task that is calling",
        matches!(syscall::sys_fd_serve_pipe(vfs_tid, 1, false, false), syscall::PipeEnd::Failed),
    );
    check("a wait for the other end of nothing fails", syscall::sys_pipe_peer(63, 1) == Err(false));
    check(
        "unlinking takes the name away",
        vfs::unlink(vfs_tid, FIFO).is_ok() && vfs::open(vfs_tid, FIFO).err() == Some(vfs::ERR_NOT_FOUND),
    );
    // One is left where it is, for whatever checks the disk afterwards to
    // find: an inode that is a pipe has to be one e2fsck agrees with.
    check(
        "and one stays behind for the filesystem check",
        matches!(vfs::mkfifo(vfs_tid, b"/tmp/dtest.kept-fifo", 0o644), Ok(()) | Err(vfs::ERR_EXISTS)),
    );
}

fn test_sync() {
    println!("locks:");
    // Uncontended, which is the path that must cost no system call at all.
    {
        let mut held = LOCK.lock();
        *held = 1;
        check("lock and write through it", *held == 1);
        check("try_lock fails while it is held", LOCK.try_lock().is_none());
    }
    check("try_lock succeeds once it is free", LOCK.try_lock().is_some());
    {
        let mut held = LOCK.lock();
        *held = 0;
    }

    ONCE.call_once(|| {
        ONCE_RAN.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    });
    ONCE.call_once(|| {
        ONCE_RAN.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    });
    check(
        "once runs exactly once",
        ONCE_RAN.load(core::sync::atomic::Ordering::Relaxed) == 1 && ONCE.is_completed(),
    );

    {
        let a = RW.read();
        let b = RW.read();
        check("two readers at once", *a == 7 && *b == 7);
    }
    {
        let mut w = RW.write();
        *w = 8;
    }
    check("a writer changed it", *RW.read() == 8);

    // Contention, which needs a second task: one task cannot both hold a lock
    // and wait for it.
    let Ok(_t) = thread::spawn_with_stack(sync_worker, 8) else {
        check("start a thread to contend with", false);
        return;
    };
    check("start a thread to contend with", true);

    let before = syscall::sys_ticks();
    let mut held = LOCK.lock();
    while *held != 99 {
        held = COND.wait(held);
    }
    let elapsed = syscall::sys_ticks() - before;
    check("condvar woke with the value the other task set", *held == 99);
    // It has to have waited — arriving after the worker had already finished
    // would prove nothing about waiting — but not spun for a whole timeout.
    check("and it waited rather than spun", elapsed >= 3 && elapsed < 200);
    drop(held);

    SEM.acquire();
    check("semaphore permit arrived", true);
    check("and there is not a second one", !SEM.try_acquire());
}

// --- floating-point state across a context switch ---

/// The go-ahead for the worker, and its report that it ran.
static FPU_GO: sync::Semaphore = sync::Semaphore::new(0);
static FPU_RAN: sync::Semaphore = sync::Semaphore::new(0);

/// A different value in each of the sixteen SSE registers, derived from a seed
/// so that two tasks' patterns can never agree by accident.
fn fpu_pattern(seed: u64, reg: u64) -> u64 {
    seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (reg << 56) ^ reg
}

/// Load every SSE register and MXCSR with this task's pattern.
///
/// Assembly because this program, like everything built for
/// x86_64-unknown-none, is compiled soft-float: no Rust statement here touches
/// an SSE register, which is exactly what makes the test deterministic. The only
/// SSE state in the system is what this function and its twin in the worker put
/// there — so if one task sees the other's, the kernel handed it over.
unsafe fn fpu_load(seed: u64, mxcsr: u32) {
    let mut vals = [0u64; 16];
    for (i, v) in vals.iter_mut().enumerate() {
        *v = fpu_pattern(seed, i as u64);
    }
    let m = mxcsr;
    unsafe {
        core::arch::asm!(
            "movq xmm0,  [{v} + 0*8]",
            "movq xmm1,  [{v} + 1*8]",
            "movq xmm2,  [{v} + 2*8]",
            "movq xmm3,  [{v} + 3*8]",
            "movq xmm4,  [{v} + 4*8]",
            "movq xmm5,  [{v} + 5*8]",
            "movq xmm6,  [{v} + 6*8]",
            "movq xmm7,  [{v} + 7*8]",
            "movq xmm8,  [{v} + 8*8]",
            "movq xmm9,  [{v} + 9*8]",
            "movq xmm10, [{v} + 10*8]",
            "movq xmm11, [{v} + 11*8]",
            "movq xmm12, [{v} + 12*8]",
            "movq xmm13, [{v} + 13*8]",
            "movq xmm14, [{v} + 14*8]",
            "movq xmm15, [{v} + 15*8]",
            "ldmxcsr [{m}]",
            v = in(reg) vals.as_ptr(),
            m = in(reg) &m as *const u32,
            options(nostack, preserves_flags),
        );
    }
}

/// Read every SSE register and MXCSR back.
unsafe fn fpu_read() -> ([u64; 16], u32) {
    let mut vals = [0u64; 16];
    let mut m: u32 = 0;
    unsafe {
        core::arch::asm!(
            "movq [{v} + 0*8],  xmm0",
            "movq [{v} + 1*8],  xmm1",
            "movq [{v} + 2*8],  xmm2",
            "movq [{v} + 3*8],  xmm3",
            "movq [{v} + 4*8],  xmm4",
            "movq [{v} + 5*8],  xmm5",
            "movq [{v} + 6*8],  xmm6",
            "movq [{v} + 7*8],  xmm7",
            "movq [{v} + 8*8],  xmm8",
            "movq [{v} + 9*8],  xmm9",
            "movq [{v} + 10*8], xmm10",
            "movq [{v} + 11*8], xmm11",
            "movq [{v} + 12*8], xmm12",
            "movq [{v} + 13*8], xmm13",
            "movq [{v} + 14*8], xmm14",
            "movq [{v} + 15*8], xmm15",
            "stmxcsr [{m}]",
            v = in(reg) vals.as_mut_ptr(),
            m = in(reg) &mut m as *mut u32,
            options(nostack, preserves_flags),
        );
    }
    (vals, m)
}

/// Round toward zero, all exceptions masked — distinct from the default
/// (round to nearest) so that a lost MXCSR is visible too.
const FPU_MXCSR_MAIN: u32 = 0x7F80;
/// Round down, all exceptions masked.
const FPU_MXCSR_WORKER: u32 = 0x3F80;

extern "C" fn fpu_worker() -> ! {
    FPU_GO.acquire();
    // The main task's pattern is loaded by now and it is asleep. Overwrite
    // every register with a different one; without a per-task save area, this
    // is what the main task will find when it wakes.
    unsafe { fpu_load(0xB0B, FPU_MXCSR_WORKER) };
    FPU_RAN.release();
    syscall::sys_exit_code(0);
}

fn test_fpu() {
    println!("floating-point state:");
    // A new task starts from a clean state rather than whatever the last task
    // left: MXCSR is the power-on default. Anything else is one task reading
    // another's.
    let (_, fresh) = unsafe { fpu_read() };
    check("a task starts with the default MXCSR", fresh & 0xFFC0 == 0x1F80);

    let Ok(_t) = thread::spawn_with_stack(fpu_worker, 8) else {
        check("start a task to share the SSE registers with", false);
        return;
    };
    unsafe { fpu_load(0xA11CE, FPU_MXCSR_MAIN) };
    FPU_GO.release();
    // Blocks, so the worker runs and loads its own pattern.
    FPU_RAN.acquire();
    let (vals, m) = unsafe { fpu_read() };

    let mut intact = true;
    for (i, v) in vals.iter().enumerate() {
        if *v != fpu_pattern(0xA11CE, i as u64) {
            intact = false;
        }
    }
    check("every SSE register survives another task using them", intact);
    check("and so does MXCSR", m == FPU_MXCSR_MAIN);
    check(
        "none of them is the other task's",
        vals[0] != fpu_pattern(0xB0B, 0),
    );
}

/// Where the pages first touched with the direction flag set go.
const BACKWARDS_AT: usize = 0xA8_0000_0000;
const BACKWARDS_PAGES: usize = 16;

fn rdtsc() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

fn test_flags() {
    println!("the flags a program leaves set:");
    // The direction flag says which way a string instruction runs, and a
    // program may have it set when the kernel is entered: a C library sets it
    // for as long as a copy that must run backwards takes (musl's `memmove`),
    // and an interrupt or a page fault arrives where it arrives. The kernel's
    // own code is compiled to find the flag clear. Entered with it set, the
    // kernel cleared a new page's frame backwards from its first word — the
    // page before it, whoever's that was — and a tick's first `memset` ran
    // down the stack over its own return address.
    //
    // Everything done with the flag set is done inside one block of assembly:
    // this program's own code is compiled to find it clear too.

    // A page first touched while it is set. The frames come from pages this
    // has just filled and given back, so that one handed over uncleared shows.
    let len = BACKWARDS_PAGES * 4096;
    let filled = syscall::sys_map_anon(BACKWARDS_AT, BACKWARDS_PAGES, false).is_ok();
    if filled {
        unsafe { core::ptr::write_bytes(BACKWARDS_AT as *mut u8, 0xAA, len) };
        let _ = syscall::sys_munmap(BACKWARDS_AT, BACKWARDS_PAGES);
    }
    let again = filled && syscall::sys_map_anon(BACKWARDS_AT, BACKWARDS_PAGES, false).is_ok();
    let mut clear = again;
    if again {
        for i in 0..BACKWARDS_PAGES {
            let page = BACKWARDS_AT + i * 4096;
            unsafe {
                core::arch::asm!(
                    "std",
                    "mov byte ptr [{at}], 1",
                    "cld",
                    at = in(reg) page + 2048,
                    options(nostack),
                );
            }
            let bytes = unsafe { core::slice::from_raw_parts(page as *const u8, 4096) };
            if bytes.iter().enumerate().any(|(j, &b)| b != (j == 2048) as u8) {
                clear = false;
            }
        }
        let _ = syscall::sys_munmap(BACKWARDS_AT, BACKWARDS_PAGES);
    }
    check("a page first touched with the direction flag set is given clear", clear);

    // A system call made with it set: answered, and the flag is still the
    // program's when it comes back.
    let before = syscall::sys_ticks();
    let (answer, flags): (u64, u64);
    unsafe {
        core::arch::asm!(
            "std",
            "syscall",
            "pushfq",
            "pop {flags}",
            "cld",
            flags = out(reg) flags,
            inlateout("rax") syscall::SYS_TICKS => answer,
            out("rcx") _, out("rdx") _, out("r8") _, out("r9") _, out("r10") _, out("r11") _,
        );
    }
    check("a system call made with it set is answered", answer >= before && answer < before + 100);
    check("and comes back with it set", flags & 0x400 != 0);

    // And the timer, which is the kernel entered at no instruction of the
    // program's choosing. Long enough with the flag set for several ticks to
    // land on it; the processor's own count says how long that is, because
    // asking the kernel would be a system call, and that clears the flag for
    // as long as the kernel runs.
    let t0 = rdtsc();
    syscall::sleep_ticks(3);
    let per_tick = (rdtsc() - t0) / 3;
    let until = rdtsc() + per_tick * 6;
    let ticks0 = syscall::sys_ticks();
    let flags: u64;
    unsafe {
        core::arch::asm!(
            "std",
            "2:",
            "rdtsc",
            "shl rdx, 32",
            "or rax, rdx",
            "cmp rax, {until}",
            "jb 2b",
            "pushfq",
            "pop {flags}",
            "cld",
            until = in(reg) until,
            flags = out(reg) flags,
            out("rax") _, out("rdx") _,
        );
    }
    let landed = syscall::sys_ticks() - ticks0;
    check("timer ticks land on a program with it set, and the machine goes on", landed >= 2);
    check("and it is still set afterwards", flags & 0x400 != 0);
}

fn test_wire() {
    println!("wayland wire format:");
    // wl_display.get_registry as libwayland actually sent it down a Quark
    // socketpair: object 1, opcode 1, size 12, one new_id argument of 2.
    // Twelve real bytes rather than twelve invented ones.
    let msg: [u8; 12] = [
        0x01, 0x00, 0x00, 0x00, // object 1
        0x01, 0x00, 0x0C, 0x00, // opcode 1, size 12
        0x02, 0x00, 0x00, 0x00, // new_id 2
    ];
    let h = wire::parse_header(&msg);
    check("a header parses", h.is_some());
    let Some(h) = h else { return };
    check("object is 1", h.object == 1);
    check("opcode is 1", h.opcode == 1);
    check("size is 12", h.size == 12);
    check("the argument is 2", wire::get_u32(&msg, 8) == Some(2));

    let mut out = [0u8; 12];
    wire::put_header(&mut out, wire::Header { object: 1, opcode: 1, size: 12 });
    wire::put_u32(&mut out, 8, 2);
    check("a header we write is the one libwayland wrote", out == msg);

    check("a truncated header is refused", wire::parse_header(&msg[..7]).is_none());
    // A size that does not cover its own header would advance a read cursor
    // by less than nothing.
    let bad: [u8; 8] = [1, 0, 0, 0, 1, 0, 4, 0];
    check("a size smaller than a header is refused", wire::parse_header(&bad).is_none());

    let mut sb = [0u8; 16];
    let n = wire::put_str(&mut sb, 0, b"wl_shm");
    check("a string is length, bytes, NUL, padding", n == 12);
    check("its length includes the NUL", wire::get_u32(&sb, 0) == Some(7));
    check(
        "and it reads back",
        wire::get_str(&sb, 0).map(|(s, _)| s) == Some(&b"wl_shm"[..]),
    );
    check("padding rounds up to four", wire::pad4(7) == 8 && wire::pad4(8) == 8);
}

/// Call after call for three seconds, each one handing the processor straight
/// to the task called.
///
/// The hand-over marks the callee runnable without queueing it, since it is
/// about to run, and then switches to it. Interrupts were back on in between,
/// and a tick there that preempted the caller -- already blocked on the callee
/// -- switched to something else and left the callee in no queue, runnable and
/// never run, with its caller waiting on it for ever. Any task busy with calls
/// could hit it; fontconfig scanning fonts did, once a minute or so. Every call
/// here has a deadline, so that shows up as a failure rather than a hang.
fn test_call_storm() {
    use quark_rt::ipc::Message;
    println!("calls:");
    let Some(child) = load_child(&[b"dchild", b"echo"]) else {
        check("start a child to call", false);
        return;
    };
    if child.start().is_err() || !mint_endpoint(STORM_SLOT, child.tid) {
        check("start a child to call", false);
        let _ = syscall::sys_task_kill(child.tid);
        let _ = wait_for(child.tid);
        return;
    }
    let start = syscall::sys_ticks();
    let mut calls = 0u64;
    let mut answered = true;
    let mut reply = Message::empty();
    while syscall::sys_ticks() - start < 300 {
        calls += 1;
        let ask = Message { sender: 0, tag: calls, data: [0; 6] };
        let outcome = syscall::sys_call_timeout(child.tid, &ask, &mut reply, 100);
        if !matches!(outcome, syscall::CallOutcome::Replied) || reply.tag != calls + 1 {
            answered = false;
            break;
        }
    }
    println!("        {} calls in {} ticks", calls, syscall::sys_ticks() - start);
    check("every call is answered, however many", answered && calls >= 1000);
    let stop = Message::empty();
    let stopped = matches!(
        syscall::sys_call_timeout(child.tid, &stop, &mut reply, 100),
        syscall::CallOutcome::Replied
    );
    if !stopped {
        let _ = syscall::sys_task_kill(child.tid);
    }
    check("and the child is still there to stop", stopped && wait_for(child.tid) == Some(0));
    let _ = syscall::sys_cap_delete(STORM_SLOT);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    // Every section by default; `dtest NAME` runs just that one, whose output
    // then fits on a screen.
    const SECTIONS: &[(&str, fn())] = &[
        ("physical", test_physical_authority),
        ("close", test_close),
        ("fds", test_fd_table),
        ("program", test_program_table),
        ("served", test_served),
        ("fdfiles", test_file_descriptors),
        ("signals", test_signals),
        ("jobs", test_jobs),
        ("region", test_big_region),
        ("memfd", test_memfd),
        ("socketpair", test_socketpair),
        ("passing", test_fd_passing),
        ("leak", test_no_leak),
        ("pollset", test_pollset),
        ("wake", test_wake_latency),
        ("poll", test_poll),
        ("environment", test_environment),
        ("spaces", test_across_address_spaces),
        ("spawn", test_spawned_memory),
        ("lend", test_lent_buffers),
        ("endpoints", test_endpoint_objects),
        ("calls", test_call_storm),
        ("service", test_runtime_service),
        ("random", test_random),
        ("locks", test_locks),
        ("memory", test_memory),
        ("disks", test_disks),
        ("ramdisk", test_ram_disk),
        ("parts", test_parts),
        ("files", test_files),
        ("fifo", test_named_pipes),
        ("sync", test_sync),
        ("fpu", test_fpu),
        ("flags", test_flags),
        ("wire", test_wire),
    ];
    let only = quark_rt::args::argv(1);
    let known = only.is_none_or(|o| SECTIONS.iter().any(|(name, _)| o == name.as_bytes()));
    if !known || quark_rt::args::argv(2).is_some() {
        println!("usage: dtest [SECTION]");
        print!("sections:");
        for (name, _) in SECTIONS {
            print!(" {}", name);
        }
        println!();
        syscall::sys_exit_code(2);
    }
    println!("[dtest] kernel and runtime checks");
    for (name, section) in SECTIONS {
        if only.is_none_or(|o| o == name.as_bytes()) {
            section();
        }
    }

    let (passed, failed) = unsafe { (PASSED, FAILED) };
    // The names again, at the end, where they are still on the screen.
    if failed > 0 {
        let names = unsafe { &*core::ptr::addr_of!(FAILURES) };
        for name in names.iter().take((failed as usize).min(RECAP)) {
            println!("  FAILED: {}", name);
        }
        if failed as usize > RECAP {
            println!("  ... and {} more", failed as usize - RECAP);
        }
    }
    println!("[dtest] {} passed, {} failed", passed, failed);
    // The program, not the task: some sections leave a thread waiting.
    syscall::sys_exit_program(if failed == 0 { 0 } else { 1 });
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[dtest] PANIC: {}", info);
    syscall::sys_exit_code(255);
}
