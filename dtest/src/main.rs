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

// The right to say who a task is, is what `identity` checks the use of. A
// shell that does not hold it cannot give it, and the section says so.
quark_rt::manifest!([CapReq::task_mgmt(0), CapReq::phys_alloc(64), CapReq::set_uid(), CapReq::clock()]);

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

    // A negative status is how the kernel says a program was killed, and a
    // program cannot say it of itself: what it ends with is kept as eight
    // bits, whichever call it ends with. Ending the *program* kept all of
    // it, and a C program that returned -1 from `main` had been "hung up".
    check(
        "a program cannot claim to have been killed",
        run(b"dchild", &[b"claim"]) == Some(245),
    );
}

/// What a client asks a server that may say who it is: see `dchild whoami`.
const ASK_WHO: u64 = 0x52;

/// A number as its digits.
fn decimal(mut n: usize, out: &mut [u8; 20]) -> &[u8] {
    let mut at = out.len();
    loop {
        at -= 1;
        out[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &out[at..]
}

/// Who a task is: its user, its group, the groups it is in besides, and
/// the one way a server may say so about somebody else.
fn test_identity() {
    use quark_rt::ipc::{Message, TID_ANY};
    println!("identity:");
    let me = syscall::sys_getpid() as usize;
    let mut was = [0u32; syscall::MAX_GROUPS];
    let Ok(had) = syscall::sys_groups(0, &mut was) else {
        check("a task says which groups it is in", false);
        return;
    };
    check("a task says which groups it is in", had <= syscall::MAX_GROUPS);
    let mut got = [0u32; syscall::MAX_GROUPS];
    check("and anybody's can be read", syscall::sys_groups(INIT_TID, &mut got).is_ok());

    // Setting them is saying who a task is, and that takes the right to.
    let may = (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(me, slot), Ok(c) if c.cap_type == syscall::CAP_TYPE_SET_UID && c.valid)
    });
    if !may {
        check(
            "without the right to say who a task is, its groups cannot be set",
            syscall::sys_set_groups(0, &[7]).is_err(),
        );
        println!("  (the rest needs SetUid, which this was not started with)");
        return;
    }
    check("a holder of SetUid sets its own", syscall::sys_set_groups(0, &[7, 9, 11]).is_ok());
    check("and they read back", syscall::sys_groups(0, &mut got) == Ok(3) && got[..3] == [7, 9, 11]);
    check("asking with no room says how many", syscall::sys_groups(0, &mut []) == Ok(3));
    let mut one = [0u32; 1];
    check("with room for one, one is written", syscall::sys_groups(0, &mut one) == Ok(3) && one[0] == 7);
    check("seventeen is one too many", syscall::sys_set_groups(0, &[1; 17]).is_err());

    // A task is who its creator is.
    check(
        "a program started here is in them too",
        run(b"dchild", &[b"groups", b"7", b"9", b"11"]) == Some(0),
    );
    match syscall::sys_fork() {
        Ok(0) => {
            let mut mine = [0u32; syscall::MAX_GROUPS];
            let same = syscall::sys_groups(0, &mut mine) == Ok(3) && mine[..3] == [7, 9, 11];
            syscall::sys_exit_program(if same { 7 } else { 8 });
        }
        Ok(child) => check("and so is a forked copy", wait_for(child) == Some(7)),
        Err(()) => check("fork", false),
    }

    // A child still being made is its maker's to say this of; one that has
    // started is not.
    match load_child(&[b"dchild", b"groups", b"21"]) {
        Some(child) => {
            let tid = child.tid;
            check("a child being prepared can be given its own", syscall::sys_set_groups(tid, &[21]).is_ok());
            check("and starts in them", child.start().is_ok() && wait_for(tid) == Some(0));
        }
        None => check("loaded a child to prepare", false),
    }
    match load_child(&[b"dchild", b"sleep"]) {
        Some(child) => {
            let tid = child.tid;
            let started = child.start().is_ok();
            check(
                "a task that has started cannot have them set from outside",
                started && syscall::sys_set_groups(tid, &[1]).is_err(),
            );
            let _ = syscall::sys_task_kill(tid);
            let _ = wait_for(tid);
        }
        None => check("loaded a child to start", false),
    }

    // A server says who a client is, while the client is asking it to.
    let mut text = [0u8; 20];
    let Some(child) = load_child(&[b"dchild", b"whoami", decimal(me, &mut text)]) else {
        check("loaded a client", false);
        return;
    };
    let granted = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, me as u64, 0)
        .is_ok()
        && syscall::sys_cap_grant_any(child.tid, syscall::SLOT_SCRATCH).is_ok();
    let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
    check("let the client call us", granted);
    check(
        "a task that is not calling cannot be told who it is",
        syscall::sys_identify(child.tid, child.tid, 1, 1, &[]).is_err(),
    );
    let client = child.tid;
    if child.start().is_err() {
        check("started the client", false);
        return;
    }
    let (mut asked, mut stranger, mut too_many, mut itself, mut its_child) = (false, false, false, false, false);
    let mut theirs = 0;
    for _ in 0..200 {
        let mut msg = Message::empty();
        if syscall::sys_recv_timeout(TID_ANY, &mut msg, 5).is_err() {
            // 3 is `sys_task_info`'s state for a task that has exited.
            if !matches!(syscall::sys_task_info(client), Ok((state, _, _)) if state != 3) {
                break;
            }
            continue;
        }
        if msg.sender != client || msg.tag != ASK_WHO {
            continue;
        }
        asked = true;
        theirs = msg.data[0] as usize;
        // Somebody else's task, named by a client: not the client's to
        // have anything said of.
        stranger = syscall::sys_identify(client, INIT_TID, 9, 9, &[]).is_err();
        too_many = syscall::sys_identify(client, client, 9, 9, &[1; 17]).is_err();
        itself = syscall::sys_identify(client, client, 1234, 5678, &[42, 43]).is_ok();
        its_child = syscall::sys_identify(client, theirs, 4321, 8765, &[44]).is_ok();
        let _ = syscall::sys_reply(client, &Message { sender: 0, tag: 0, data: [0; 6] });
    }
    check("the client asked who it is", asked);
    check("a stranger's task is not a client's to have named", stranger);
    check("nor may it be put in seventeen groups", too_many);
    check("a client that is calling is told who it is", itself);
    check("and so is a child it has made and not started", its_child);
    check("which is what they both then are", wait_for(client) == Some(31));
    // The child the client made was never started and its maker has gone:
    // nobody else will end it.
    if theirs != 0 {
        let _ = syscall::sys_task_kill(theirs);
    }
    check(
        "a client that has stopped calling is not",
        syscall::sys_identify(client, client, 0, 0, &[]).is_err(),
    );

    check("this task's groups are put back", syscall::sys_set_groups(0, &was[..had]).is_ok());
}

/// Passwords and the files accounts are kept in: the runtime's own code, run
/// where it will be used. The hashes are Drepper's test vectors, which every
/// C library's `crypt` also has to produce.
fn test_passwords() {
    use quark_rt::accounts::{self, Rights};
    use quark_rt::crypt;
    println!("passwords:");
    let hash = |password: &[u8], setting: &[u8], want: &[u8]| {
        let mut out = [0u8; crypt::MAX_HASH];
        crypt::sha512_crypt(password, setting, &mut out).is_some_and(|n| &out[..n] == want)
    };
    check(
        "SHA-512 of three letters is what it is everywhere",
        crypt::sha512(b"abc")[..8] == [0xdd, 0xaf, 0x35, 0xa1, 0x93, 0x61, 0x7a, 0xba],
    );
    const HELLO: &[u8] =
        b"$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";
    check("a password hashes as it does on any Unix", hash(b"Hello world!", b"$6$saltstring", HELLO));
    check(
        "with the rounds it is told, and sixteen characters of salt",
        hash(
            b"Hello world!",
            b"$6$rounds=10000$saltstringsaltstring",
            b"$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v.",
        ),
    );
    check(
        "a password longer than a block of the hash",
        hash(
            b"a very much longer text to encrypt.  This one even stretches over morethan one line.",
            b"$6$rounds=1400$anotherlongsaltstring",
            b"$6$rounds=1400$anotherlongsalts$POfYwTEok97VWcjxIiSOjiykti.o/pQs.wPvMxQ6Fm7I6IoYN3CmLs66x9t0oSwbtEW7o7UmJEiDwGqd8p4ur1",
        ),
    );
    check("the right password opens it", crypt::verify(b"Hello world!", HELLO));
    check("a wrong one does not", !crypt::verify(b"Hello world?", HELLO));
    check("nor does anything open a locked account", !crypt::verify(b"", b"!") && !crypt::verify(b"x", b"*"));
    let mut one = [0u8; crypt::MAX_HASH];
    let mut two = [0u8; crypt::MAX_HASH];
    let a = crypt::make(b"secret", &[1; 12], &mut one);
    let b = crypt::make(b"secret", &[2; 12], &mut two);
    check(
        "a new hash is one that verifies",
        a.is_some_and(|n| crypt::verify(b"secret", &one[..n]) && !crypt::verify(b"Secret", &one[..n])),
    );
    check("and is salted: the same password twice is two hashes", a.is_some() && b.is_some() && one != two);

    let passwd = b"root:x:0:0:root:/root:/usr/bin/bash\n# a note\nnate:x:1000:1000:Nate:/home/nate:/usr/bin/qsh\nold:5:5:/home/old:/usr/bin/QSH.ELF\n";
    check(
        "an account is read as Unix writes it",
        accounts::user_named(passwd, b"nate").is_some_and(|u| u.uid == 1000 && u.home == b"/home/nate"),
    );
    check(
        "and as this system first wrote it",
        accounts::user_named(passwd, b"old").is_some_and(|u| u.uid == 5 && u.shell == b"/usr/bin/QSH.ELF"),
    );
    let group = b"root:x:0:\nwheel:x:10:root,nate\nnate:x:1000:\nvideo:x:39:nate\n";
    let mut groups = [0u32; accounts::MAX_GROUPS];
    check(
        "the groups somebody is in are the ones that list them",
        accounts::groups_of(group, b"nate", 1000, &mut groups) == 2 && groups[..2] == [10, 39],
    );
    check(
        "with no rights file, root has every right and nobody else has any",
        accounts::rights_of(None, b"root", 0).has(Rights::ALL) && accounts::rights_of(None, b"nate", 1000) == Rights::NONE,
    );
    check(
        "with one, an account has what its line says",
        accounts::rights_of(Some(b"nate power become\n"), b"nate", 1000).has(Rights::POWER | Rights::BECOME)
            && !accounts::rights_of(Some(b"nate power become\n"), b"nate", 1000).has(Rights::TASKS),
    );
    let mut out = [0u8; 256];
    check(
        "a line is replaced where it is and the rest left alone",
        accounts::with_record(b"a:1\nb:2\nc:3\n", b"b", b':', Some(b"b:9"), &mut out)
            .is_some_and(|n| &out[..n] == b"a:1\nb:9\nc:3\n"),
    );
    check(
        "a name that would break a file is not a name",
        !accounts::name_ok(b"a:b") && !accounts::name_ok(b"") && !accounts::name_ok(b"Root") && accounts::name_ok(b"amy_2"),
    );
}

/// A call to a task that ends without answering. The caller is answered by
/// the ending — a failure — and not left until somebody collects the task:
/// the somebody is its parent, and here the parent is the caller.
fn test_callee_gone() {
    use quark_rt::ipc::Message;
    println!("a call to a task that goes:");
    let Some(child) = load_child(&[b"dchild", b"late"]) else {
        check("loaded a child that will go", false);
        return;
    };
    let tid = child.tid;
    let may = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, tid as u64, 0).is_ok();
    if !may || child.start().is_err() {
        let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
        check("started a child that will go", false);
        return;
    }
    let before = syscall::sys_ticks();
    let mut reply = Message::empty();
    let ask = Message { sender: 0, tag: 1, data: [0; 6] };
    // Five seconds, which is how long this waits if nothing ends the call.
    let outcome = syscall::sys_call_timeout(tid, &ask, &mut reply, 500);
    let took = syscall::sys_ticks() - before;
    let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
    // The answer is the kernel's: a message from the task that went, with
    // the tag every refusal has.
    let failed = match outcome {
        syscall::CallOutcome::Failed => true,
        syscall::CallOutcome::Replied => reply.tag == u64::MAX,
        syscall::CallOutcome::TimedOut => false,
    };
    check("its parent, in a call to it, is answered when it ends", failed && took < 200);
    check("and can then collect it", wait_for(tid) == Some(3));
}

/// A child that is built and then not wanted: what `login` has when the
/// password was wrong. It has to go back whole — the task, and the address
/// space with the image in it — or sixty-four wrong passwords are the last
/// anybody types.
fn test_discard() {
    println!("a child that is not started:");
    // This program's own children, and nobody else's: what else is running
    // on the machine comes and goes as it likes, and a program that ended
    // while these were counted made one fewer of "all the tasks there are".
    let me = syscall::sys_getpid() as usize;
    let mine = || (2..64).filter(|&t| matches!(syscall::sys_task_info(t), Ok((_, parent, _)) if parent == me)).count();
    let before = mine();
    let mut all = true;
    for _ in 0..80 {
        match load_child(&[b"dchild", b"quit"]) {
            Some(child) => child.discard(),
            None => {
                all = false;
                break;
            }
        }
    }
    check("eighty are built and taken back, in a table of sixty-four", all);
    check("and no task is left of them", mine() == before);
    check("the next one still runs", run(b"dchild", &[b"quit"]) == Some(0));
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

static PRINTER_SLAVE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
/// 1 once the newline has been printed, 2 if it could not be.
static PRINTED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Print a newline at a terminal, which waits if there is no room for it.
extern "C" fn printer() -> ! {
    let slave = PRINTER_SLAVE.load(core::sync::atomic::Ordering::SeqCst);
    let wrote = syscall::sys_fd_write(slave, b"\n") == 1;
    PRINTED.store(if wrote { 1 } else { 2 }, core::sync::atomic::Ordering::SeqCst);
    syscall::sys_exit_code(0);
}

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

    // What a program prints waits for room, and for as much as it needs: a
    // newline goes out as a return and a newline. With one byte left it
    // waits, and is let through when the terminal is read. It asked only
    // whether there was any room, found some, and tried again for ever —
    // in the kernel, which on several processors nobody else could then
    // get into to read the terminal.
    let mut drain = [0u8; 256];
    while (1..=256).contains(&syscall::sys_fd_read_nb(master, &mut drain)) {}
    const NEARLY: usize = 4095;
    let filled = syscall::sys_fd_write_nb(slave, &[b'x'; NEARLY]) == NEARLY as u64;
    PRINTER_SLAVE.store(slave, SeqCst);
    PRINTED.store(0, SeqCst);
    match thread::spawn_with_stack(printer, 8) {
        Ok(t) => {
            syscall::sleep_ticks(10);
            check(
                "a newline printed to a terminal with one byte of room waits for two",
                filled && PRINTED.load(SeqCst) == 0,
            );
            let mut seen = 0;
            let mut last = [0u8; 2];
            while seen < NEARLY + 2 {
                let n = syscall::sys_fd_read(master, &mut drain);
                if !(1..=256).contains(&n) {
                    break;
                }
                let n = n as usize;
                if n >= 2 {
                    last = [drain[n - 2], drain[n - 1]];
                } else {
                    last = [last[1], drain[0]];
                }
                seen += n;
            }
            let _ = t.join();
            check(
                "and is printed when the terminal has been read",
                PRINTED.load(SeqCst) == 1 && seen == NEARLY + 2 && &last == b"\r\n",
            );
        }
        Err(_) => check("a thread to print a newline", false),
    }

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
    // On a terminal, a session is one login's: begun when somebody logged
    // in, over when they log out, and what they left running is then in a
    // session the terminal is no longer the terminal of. For a long time the
    // terminal's keeper led one session for as long as the machine was up,
    // and everybody who ever logged in was in it. The keeper is what `init`
    // starts; a login is what the keeper starts.
    let on_terminal = syscall::sys_pty_number(0).is_ok() && syscall::sys_pty_session(0) == session;
    let leader = (2..64).find(|&t| session.is_some() && syscall::sys_pid(t) == session);
    let a_logins = leader.is_some_and(|t| matches!(syscall::sys_task_info(t), Ok((_, parent, _)) if parent != INIT_TID));
    check("a session on a terminal is one login's, and not the terminal's keeper's", !on_terminal || a_logins);

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
    // Nor to use, for a user. A program outside the session is refused the
    // terminal by its number and through a descriptor it was left holding:
    // what somebody's program is, once they have logged out and somebody
    // else is typing.
    if syscall::sys_get_uid().0 == 0 && holds_set_uid() {
        let mut text = [0u8; 20];
        let number = decimal(syscall::sys_pty_number(slave).unwrap_or(usize::MAX), &mut text);
        let spied = load_child(&[b"dchild", b"ptyspy", number]).and_then(|child| {
            let tid = child.tid;
            let ready = syscall::sys_fd_dup(tid, 3, slave).is_ok()
                && syscall::sys_set_gid(tid, USER_GROUP).is_ok()
                && syscall::sys_set_uid(tid, USER).is_ok();
            if !ready {
                child.discard();
                return None;
            }
            child.start().ok()?;
            wait_for(tid)
        });
        let bits = spied.filter(|s| (0..128).contains(s)).unwrap_or(0);
        check("a user outside the session is not given its terminal by number", bits & 1 != 0);
        check("nor reads it through a descriptor left over from another", bits & 2 != 0);
        check("nor writes to it, nor changes how it behaves", bits & 12 == 12);
        check("a terminal a user makes is that user's to open", bits & 16 != 0);
    }
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

/// Who the checks of what a user may do are made as: nobody the system has
/// an account for. `dchild` knows the same three numbers.
const USER: u32 = 4000;
const USER_GROUP: u32 = 4000;
/// A group that user is in besides its own.
const ALSO_IN: u32 = 4001;
/// Room for the account files to be read and rewritten in.
const ACCOUNTS_AT: usize = 0xA9_0000_0000;

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

    // A pager gives a program a capability to map an object with, and the
    // program maps it: two steps, and the object can go idle between them.
    // It is kept for the program that was promised it. It was not, and a
    // program that asked to map a file as another unmapped the last of it
    // was told there was no memory.
    const PAGED_SLOT: usize = 48;
    let kept = syscall::sys_object_create(0x0B1EC7, 4096, PAGED_SLOT).ok().and_then(|id| {
        let child = load_child(&[b"dchild", b"sleep"])?;
        let tid = child.tid;
        let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
        let granted = syscall::sys_cap_mint(
            syscall::SLOT_SCRATCH,
            syscall::CAP_TYPE_MEMOBJECT,
            id,
            syscall::OBJECT_ACCESS_READ,
        )
        .is_ok()
            && syscall::sys_cap_grant_any(tid, syscall::SLOT_SCRATCH).is_ok();
        let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
        child.start().ok()?;
        let held = syscall::sys_object_ctl(id, syscall::OBJECT_RELEASE, 0, 0);
        let _ = syscall::sys_task_kill(tid);
        let _ = wait_for(tid);
        let freed = syscall::sys_object_ctl(id, syscall::OBJECT_RELEASE, 0, 0);
        Some((granted, held, freed))
    });
    let _ = syscall::sys_cap_delete(PAGED_SLOT);
    check(
        "an object nothing maps is kept while a program holds a capability to map it with",
        matches!(kept, Some((true, syscall::OBJECT_RELEASE_LATER, _))),
    );
    check("and is released once that program has gone", matches!(kept, Some((true, _, 0))));
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

/// Start `ramdisk MEGABYTES` and wait for the disk it makes: its server, its
/// driver and its name.
fn start_ram_disk(megabytes: &[u8], sectors: u64) -> Option<(usize, usize, [u8; 4])> {
    let server = load_program(b"/usr/bin/ramdisk", b"/usr/bin/RAMDISK.ELF", &[b"ramdisk", megabytes])
        .and_then(|c| {
            let tid = c.tid;
            c.start().ok().map(|()| tid)
        })?;
    for _ in 0..50 {
        if let Some((disk, name)) = ram_disk_of(sectors) {
            return Some((server, disk, name));
        }
        syscall::sleep_ticks(2);
    }
    let _ = syscall::sys_task_kill(server);
    let _ = wait_for(server);
    None
}

/// `/dev/` and a disk's name, and a partition's number if there is one.
fn dev_path(name: &[u8], volume: u64, out: &mut [u8; 16]) -> usize {
    out[..5].copy_from_slice(b"/dev/");
    out[5..5 + name.len()].copy_from_slice(name);
    let mut len = 5 + name.len();
    if volume > 0 {
        out[len] = b'p';
        out[len + 1] = b'0' + volume as u8;
        len += 2;
    }
    len
}

/// A disk as a file under `/dev`: who holds it while it is open, and what a
/// handle may do. (The reading and writing is `blktest`'s, in C.)
fn test_disk_files() {
    use quark_rt::block;
    use quark_rt::vfs::{self as files, OPEN_WRITE};
    println!("disks as files:");
    let Some(vfs) = nameserver::lookup(b"vfs") else {
        check("find the file server", false);
        return;
    };
    let vfs_pid = syscall::sys_pid(vfs).unwrap_or(0);
    let Some((server, disk, name)) = start_ram_disk(b"2", 4096) else {
        check("start a RAM disk", false);
        return;
    };
    let mut text = [0u8; 16];
    let len = dev_path(&name, 0, &mut text);
    let path = &text[..len];
    let holder = |disk: usize| block::info(disk, 0).map_or(u64::MAX, |i| i.claimant);
    let mut sector = [0u8; 512];

    // Looking takes nothing.
    let looking = files::open_with(vfs, path, 0);
    check(
        "a disk opens to read, and says how long it is",
        looking.as_ref().is_ok_and(|o| o.size == 2 << 20 && o.mode & 0o170000 == 0o060000),
    );
    let looking = looking.map_or(usize::MAX, |o| o.handle);
    check("which claims nothing", holder(disk) == 0);
    check("it is read", files::read(vfs, looking, &mut sector, 0) == Ok(512));
    check(
        "and not written through a handle that did not ask to",
        files::write(vfs, looking, &sector, 0) == Err(files::ERR_READ_ONLY),
    );
    check(
        "nor told to read its partition table again",
        files::devctl(vfs, looking, files::DEVCTL_RESCAN) == Err(files::ERR_PERMISSION),
    );

    // Writing takes the volume, for as long as anything writes.
    let first = files::open_with(vfs, path, OPEN_WRITE).map(|o| o.handle);
    check("opened to write, it is the file server's", first.is_ok() && holder(disk) == vfs_pid && vfs_pid != 0);
    check(
        "and nobody else's to claim",
        block::claim(disk, 0) == Err(block::ERR_BUSY),
    );
    sector[..4].copy_from_slice(b"disk");
    check(
        "what is written through the file is on the disk",
        first.is_ok_and(|h| files::write(vfs, h, &sector, 512) == Ok(512))
            && block::read(disk, 0, 1, &mut sector).is_ok()
            && &sector[..4] == b"disk",
    );
    let second = files::open_with(vfs, path, OPEN_WRITE).map(|o| o.handle);
    check("a second handle that writes shares the claim", second.is_ok() && holder(disk) == vfs_pid);
    check(
        "which outlasts the first of them",
        first.is_ok_and(|h| files::close(vfs, h).is_ok()) && holder(disk) == vfs_pid,
    );
    check(
        "and goes with the last",
        second.is_ok_and(|h| files::close(vfs, h).is_ok()) && holder(disk) == 0,
    );

    // Somebody else's: a mounted filesystem's server holds its volume so.
    check("the disk is claimed by somebody else", block::claim(disk, 0).is_ok());
    check(
        "and then does not open to write",
        files::open_with(vfs, path, OPEN_WRITE).err() == Some(files::ERR_BUSY),
    );
    sector.fill(0);
    check(
        "but is still read, through the handle that was looking",
        files::read(vfs, looking, &mut sector, 512) == Ok(512) && &sector[..4] == b"disk",
    );
    check("and still opens to read", files::open_with(vfs, path, 0).is_ok_and(|o| files::close(vfs, o.handle).is_ok()));
    let _ = block::release(disk, 0);

    // A partition is a file of its own, and a table is read through the
    // whole disk's.
    let mut mbr = [0u8; 512];
    mbr[446 + 4] = 0x83;
    mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    let whole = files::open_with(vfs, path, OPEN_WRITE).map(|o| o.handle);
    check(
        "a partition table is written, and the driver told to read it",
        whole.is_ok_and(|h| {
            files::write(vfs, h, &mbr, 0) == Ok(512) && files::devctl(vfs, h, files::DEVCTL_RESCAN) == Ok(2)
        }),
    );
    let mut part_text = [0u8; 16];
    let part_len = dev_path(&name, 1, &mut part_text);
    let part = files::open_with(vfs, &part_text[..part_len], OPEN_WRITE);
    check("the partition is a file, as long as the table says", part.as_ref().is_ok_and(|o| o.size == 1024 * 512));
    let part = part.map_or(usize::MAX, |o| o.handle);
    check(
        "which has no table of its own to read",
        files::devctl(vfs, part, files::DEVCTL_RESCAN) == Err(files::ERR_INVALID_PATH),
    );
    check(
        "and the table is not read again while it is in use",
        whole.is_ok_and(|h| files::devctl(vfs, h, files::DEVCTL_RESCAN) == Err(files::ERR_BUSY)),
    );
    let _ = files::close(vfs, part);
    if let Ok(h) = whole {
        let _ = files::close(vfs, h);
    }
    check(
        "both are nobody's when they are closed",
        holder(disk) == 0 && block::info(disk, 1).is_ok_and(|i| i.claimant == 0),
    );

    // A handle that outlives its driver. The next disk of memory takes the
    // name, and may take the task's number.
    let stale = files::open_with(vfs, path, OPEN_WRITE).map_or(usize::MAX, |o| o.handle);
    let _ = syscall::sys_task_kill(server);
    let _ = wait_for(server);
    let Some((server, disk, again)) = start_ram_disk(b"2", 4096) else {
        check("start a second RAM disk", false);
        return;
    };
    check("a second disk takes the first one's name", again == name);
    check(
        "a handle on the first reads nothing and writes nothing",
        files::read(vfs, stale, &mut sector, 0) == Err(files::ERR_IO)
            && files::write(vfs, stale, &sector, 0) == Err(files::ERR_IO)
            && files::read(vfs, looking, &mut sector, 0) == Err(files::ERR_IO),
    );
    check("and its claim is not on the second", holder(disk) == 0);
    let fresh = files::open_with(vfs, path, OPEN_WRITE).map(|o| o.handle);
    check("which opens to write", fresh.is_ok() && holder(disk) == vfs_pid);
    check(
        "and is not let go when the stale handle closes",
        files::close(vfs, stale).is_ok() && holder(disk) == vfs_pid,
    );
    check("but when its own does", fresh.is_ok_and(|h| files::close(vfs, h).is_ok()) && holder(disk) == 0);
    let _ = files::close(vfs, looking);

    // The disk the system is running from: the one the file server holds.
    let mut root = None;
    'find: for driver in [&b"disk0"[..], b"disk1", b"ram0", b"ram1", b"ram2", b"ram3"] {
        let Some(tid) = nameserver::lookup(driver) else { continue };
        let volumes = block::info(tid, 0).map_or(0, |i| i.volumes);
        for volume in 0..volumes {
            if block::info(tid, volume).is_ok_and(|i| i.claimant == vfs_pid) {
                root = Some((tid, driver, volume));
                break 'find;
            }
        }
    }
    let Some((root_disk, root_name, root_volume)) = root else {
        check("the file server holds a volume", false);
        return;
    };
    let whole_len = dev_path(root_name, 0, &mut text);
    let before = holder(root_disk);
    check(
        "the disk the system runs from does not open to write",
        files::open_with(vfs, &text[..whole_len], OPEN_WRITE).err() == Some(files::ERR_BUSY),
    );
    let part_len = dev_path(root_name, root_volume, &mut part_text);
    check(
        "nor does the volume its root is on",
        files::open_with(vfs, &part_text[..part_len], OPEN_WRITE).err() == Some(files::ERR_BUSY),
    );
    let look = files::open_with(vfs, &text[..whole_len], 0).map(|o| o.handle);
    check(
        "it opens to read, and is read",
        look.is_ok_and(|h| files::read(vfs, h, &mut sector, 0) == Ok(512) && files::close(vfs, h).is_ok()),
    );
    check("and is whose it was afterwards", holder(root_disk) == before);

    let _ = syscall::sys_task_kill(server);
    let _ = wait_for(server);
}

/// Run a program in `/usr/bin` with `args` and say how it ended. `None` if
/// it is not there.
fn run(name: &[u8], args: &[&[u8]]) -> Option<i32> {
    let mut lower = [0u8; 40];
    let mut upper = [0u8; 44];
    let at = b"/usr/bin/".len();
    lower[..at].copy_from_slice(b"/usr/bin/");
    lower[at..at + name.len()].copy_from_slice(name);
    upper[..at + name.len()].copy_from_slice(&lower[..at + name.len()]);
    upper[at..at + name.len()].make_ascii_uppercase();
    upper[at + name.len()..at + name.len() + 4].copy_from_slice(b".ELF");
    let mut argv: [&[u8]; 8] = [name; 8];
    argv[1..1 + args.len()].copy_from_slice(args);
    let child = load_program(&lower[..at + name.len()], &upper[..at + name.len() + 4], &argv[..1 + args.len()])?;
    let tid = child.tid;
    child.start().ok()?;
    wait_for(tid)
}

/// The whole of a file, by path; how much of it there was.
fn slurp(vfs_tid: usize, path: &[u8], into: &mut [u8]) -> Result<usize, u64> {
    let (handle, _, _) = vfs::open(vfs_tid, path)?;
    let mut got = 0;
    let result = loop {
        match vfs::read(vfs_tid, handle, &mut into[got..], got as u32) {
            Ok(0) => break Ok(got),
            Ok(n) => got += n as usize,
            Err(code) => break Err(code),
        }
        if got == into.len() {
            break Ok(got);
        }
    };
    let _ = vfs::close(vfs_tid, handle);
    result
}

/// Make a file hold exactly `bytes`.
fn spill(vfs_tid: usize, path: &[u8], bytes: &[u8]) -> Result<(), u64> {
    let handle = vfs::open_with(vfs_tid, path, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE)?.handle;
    let mut at = 0;
    let result = loop {
        if at == bytes.len() {
            break Ok(());
        }
        match vfs::write(vfs_tid, handle, &bytes[at..], at as u32) {
            Ok(n) if n > 0 => at += n as usize,
            Ok(_) => break Err(vfs::ERR_IO),
            Err(code) => break Err(code),
        }
    };
    let _ = vfs::close(vfs_tid, handle);
    result
}

/// A path in `dir`.
fn under<'a>(dir: &[u8], name: &[u8], out: &'a mut [u8; 160]) -> &'a [u8] {
    let n = dir.len().min(120);
    out[..n].copy_from_slice(&dir[..n]);
    out[n] = b'/';
    out[n + 1..n + 1 + name.len()].copy_from_slice(name);
    &out[..n + 1 + name.len()]
}

/// Whether this task may say who another is.
fn holds_set_uid() -> bool {
    let me = syscall::sys_getpid() as usize;
    (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(me, slot), Ok(c) if c.cap_type == syscall::CAP_TYPE_SET_UID && c.valid)
    })
}

/// How many capabilities of one kind a task holds.
fn holds(tid: usize, cap_type: u64) -> usize {
    (0..64).filter(|&slot| matches!(syscall::sys_cap_read(tid, slot), Ok(c) if c.cap_type == cap_type && c.valid)).count()
}

/// What `dchild user` looks at, laid out in `dir` as root.
fn lay_out_for_user(v: usize, dir: &[u8]) -> bool {
    let mut path = [0u8; 160];
    let mode = |path: &[u8], mode: u32| vfs::set_attr(v, path, vfs::ATTR_MODE, mode, 0, 0, 0, 0).is_ok();
    let mut ok = vfs::mkdir(v, dir).is_ok() && mode(dir, 0o755);
    let secret = under(dir, b"secret", &mut path);
    ok &= spill(v, secret, b"secret").is_ok() && mode(secret, 0o600);
    let shared = under(dir, b"shared", &mut path);
    ok &= spill(v, shared, b"shared").is_ok()
        && vfs::set_attr(v, shared, vfs::ATTR_MODE | vfs::ATTR_GID, 0o640, 0, ALSO_IN, 0, 0).is_ok();
    let open = under(dir, b"open", &mut path);
    ok &= spill(v, open, b"open").is_ok() && mode(open, 0o644);
    let sticky = under(dir, b"sticky", &mut path);
    ok &= vfs::mkdir(v, sticky).is_ok() && mode(sticky, 0o1777);
    ok &= spill(v, under(dir, b"sticky/roots", &mut path), b"roots").is_ok();
    ok &= vfs::mkdir(v, under(dir, b"sticky/rootsdir", &mut path)).is_ok();
    let private = under(dir, b"private", &mut path);
    ok &= vfs::mkdir(v, private).is_ok() && mode(private, 0o700);
    ok &= spill(v, under(dir, b"private/inside", &mut path), b"inside").is_ok();
    ok
}

/// Take that away again, and whatever a run that stopped half way left.
fn clear_for_user(v: usize, dir: &[u8]) {
    let mut path = [0u8; 160];
    for name in [
        &b"secret"[..],
        b"shared",
        b"open",
        b"new",
        b"sticky/roots",
        b"sticky/mine",
        b"sticky/moved",
        b"sticky/taken",
        b"sticky/kept",
        b"private/inside",
    ] {
        let _ = vfs::unlink(v, under(dir, name, &mut path));
    }
    for name in [&b"sticky/rootsdir"[..], b"newdir", b"sticky", b"private"] {
        let _ = vfs::rmdir(v, under(dir, name, &mut path));
    }
    let _ = vfs::rmdir(v, dir);
}

/// Run `dchild` as a user who is not root, and say how it ended.
fn run_as_user(args: &[&[u8]]) -> Option<i32> {
    let child = load_child(args)?;
    let tid = child.tid;
    // The user last: saying who a task is takes being allowed to, and the
    // order here is the order a program that gives its own rights up uses.
    let said = syscall::sys_set_gid(tid, USER_GROUP).is_ok()
        && syscall::sys_set_groups(tid, &[ALSO_IN]).is_ok()
        && syscall::sys_set_uid(tid, USER).is_ok();
    if !said {
        child.discard();
        return None;
    }
    child.start().ok()?;
    wait_for(tid)
}

/// The same checks twice: on the filesystem the system runs from, and on
/// one mounted in it, where a second server is told who is asking.
macro_rules! user_checks {
    ($($what:literal),* $(,)?) => {
        const AS_USER: &[&str] = &[$($what),*];
        const AS_USER_MOUNTED: &[&str] = &[$(concat!("through a mount: ", $what)),*];
    };
}
user_checks![
    "root's own file does not open for a user",
    "a file of a group the user is in besides its own is read, and not written",
    "a file anybody may read is read, and not written, removed or joined by another",
    "where anybody may make files, another's is not removed, moved or replaced",
    "and the user's own is",
    "a directory of root's own is not looked in",
    "a file's mode is its owner's to change, and whose it is, is root's",
];

/// Lay `dir` out, run `dchild user` there as a user, and check each thing
/// it reports.
fn as_a_user(v: usize, dir: &[u8], names: &[&'static str]) {
    clear_for_user(v, dir);
    let laid = lay_out_for_user(v, dir);
    let status = if laid { run_as_user(&[b"dchild", b"user", dir]) } else { None };
    // A status of 128 or more is a program that fell over.
    let bits = status.filter(|s| (0..128).contains(s)).unwrap_or(0);
    for (i, &name) in names.iter().enumerate() {
        check(name, bits & (1 << i) != 0);
    }
    clear_for_user(v, dir);
}

/// What somebody who is not root may do, and may not: the file server's
/// rules and the kernel's, asked of them by a program that is that user.
fn test_users() {
    println!("users:");
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the file server", false);
        return;
    };
    if syscall::sys_get_uid().0 != 0 || !holds_set_uid() {
        println!("  (needs root, and the right to say who a task is)");
        return;
    }
    as_a_user(v, b"/tmp/dtest-users", AS_USER);

    // The rest of what a user is not: a task of root's to try to end.
    let Some(victim) = load_child(&[b"dchild", b"sleep"]) else {
        check("loaded a program of root's", false);
        return;
    };
    let victim_tid = victim.tid;
    if victim.start().is_err() {
        check("started a program of root's", false);
        return;
    }
    let mut text = [0u8; 20];
    let status = run_as_user(&[b"dchild", b"usersys", decimal(victim_tid, &mut text)]);
    let bits = status.filter(|s| (0..128).contains(s)).unwrap_or(0);
    check("a program started as a user is that user, in the groups it was put in", bits & 1 != 0);
    check("the passwords do not open for it", bits & 2 != 0);
    check("who it is, is not its own to say", bits & 4 != 0);
    check("root's program is not its to end", bits & 8 != 0);
    check("a file it makes is its own", bits & 16 != 0);
    check("and the accounts are not its to change", bits & 32 != 0);
    check(
        "root's program is still there, and root ends it",
        syscall::sys_task_kill(victim_tid).is_ok() && wait_for(victim_tid).is_some(),
    );
}

/// The one line of `/etc/rights` for `name`: `rights` as its words, or no
/// line at all. Whether it was written.
fn set_rights(v: usize, name: &[u8], rights: Option<&[u8]>, work: &mut [u8]) -> bool {
    use quark_rt::accounts;
    let (old_buf, out) = work.split_at_mut(accounts::WORK / 5);
    let old = accounts::read(v, b"", b"rights", old_buf);
    let mut line = [0u8; 96];
    let line = rights.map(|r| {
        let n = name.len();
        line[..n].copy_from_slice(name);
        line[n] = b' ';
        line[n + 1..n + 1 + r.len()].copy_from_slice(r);
        &line[..n + 1 + r.len()]
    });
    let Some(len) = accounts::with_record(old.unwrap_or(b""), name, b' ', line, out) else {
        return false;
    };
    // A file this made, with nothing left in it, goes: a system that had no
    // such file is one where root is root, and is left that way.
    if len == 0 || out[..len].iter().all(|b| b.is_ascii_whitespace()) {
        let _ = vfs::unlink(v, b"/etc/rights");
        return true;
    }
    accounts::write(v, b"", b"rights", &out[..len], 0o644).is_ok()
}

/// Load `dchild`, have the server make it `user` — asked as root, who is
/// asked for no password — run it, and say how it ended.
fn run_blessed(user: &[u8], args: &[&[u8]]) -> Option<i32> {
    let child = load_child(args)?;
    let tid = child.tid;
    if quark_rt::auth::bless(tid, user, b"", 0).is_err() {
        child.discard();
        return None;
    }
    child.start().ok()?;
    wait_for(tid)
}

/// The server that says who somebody is, asked the way `login` and `su`
/// ask it: an account is made, given a password, become, and taken away.
fn test_auth() {
    use quark_rt::accounts::{self, NewUser};
    use quark_rt::auth;
    println!("auth:");
    if nameserver::lookup_retry(auth::NAME, 5).is_none() {
        println!("  (nothing here says who anybody is)");
        return;
    }
    let Some(v) = nameserver::lookup_retry(b"vfs", 20) else {
        check("find the file server", false);
        return;
    };
    check(
        "a name nobody has is asked for a password like any other",
        auth::needs(b"dtest-nobody-at-all") == Ok(true),
    );
    if syscall::sys_get_uid().0 != 0 {
        println!("  (the rest makes an account, and needs root)");
        return;
    }
    if syscall::sys_mmap(ACCOUNTS_AT, accounts::WORK / 4096).is_err() {
        check("memory to rewrite the accounts in", false);
        return;
    }
    let work = unsafe { core::slice::from_raw_parts_mut(ACCOUNTS_AT as *mut u8, accounts::WORK) };
    const NAME: &[u8] = b"dtestuser";
    const OTHER: &[u8] = b"dtestother";
    const FIRST: &[u8] = b"the first password";
    const SECOND: &[u8] = b"a second one, longer than the first and with a comma";
    const THIRD: &[u8] = b"3rd";
    // Whatever a run that stopped half way left.
    let _ = accounts::remove_user(v, b"", NAME, work);
    let _ = accounts::remove_user(v, b"", OTHER, work);
    let _ = set_rights(v, NAME, None, work);

    let new = |name| NewUser { name, uid: None, group: None, about: b"dtest", home: None, shell: None, make_home: false };
    let made = accounts::add_user(v, b"", &new(NAME), work);
    let other = accounts::add_user(v, b"", &new(OTHER), work);
    check("two accounts are made", made.is_ok() && other.is_ok());
    let (Ok((uid, gid)), Ok((other_uid, _))) = (made, other) else {
        let _ = accounts::remove_user(v, b"", NAME, work);
        let _ = accounts::remove_user(v, b"", OTHER, work);
        let _ = syscall::sys_munmap(ACCOUNTS_AT, accounts::WORK / 4096);
        return;
    };

    // One child, made and never started, to be said things about.
    let Some(child) = load_child(&[b"dchild", b"quit"]) else {
        check("loaded a child to bless", false);
        return;
    };
    let tid = child.tid;
    let who = || syscall::sys_get_tuid(tid);
    check(
        "a new account is locked: no password makes anybody it, and none is told why",
        auth::bless(tid, NAME, b"", auth::CHECK) == Err(auth::ERR_WRONG) && who() == Ok((0, 0)),
    );
    check(
        "root is asked for none, and its child is made that user",
        auth::bless(tid, NAME, b"", 0) == Ok((uid, gid)) && who() == Ok((uid, gid)),
    );
    check(
        "holding nothing an account with no rights is not given",
        holds(tid, syscall::CAP_TYPE_TASK_MGMT) == 0
            && holds(tid, syscall::CAP_TYPE_IOPORT) == 0
            && holds(tid, syscall::CAP_TYPE_SET_UID) == 0,
    );
    check("root gives the account a password", auth::passwd(NAME, b"", FIRST).is_ok());
    check("which it is then asked for", auth::needs(NAME) == Ok(true));
    check("the password opens it", auth::bless(tid, NAME, FIRST, auth::CHECK) == Ok((uid, gid)));
    check(
        "nobody is made a user nobody is, and is told no more than that it was wrong",
        auth::bless(tid, b"dtest-nobody-at-all", FIRST, auth::CHECK) == Err(auth::ERR_WRONG),
    );
    // Three wrong ones, and then the right one has to wait.
    let wrong = (0..3).all(|_| auth::bless(tid, NAME, b"not the password", auth::CHECK) == Err(auth::ERR_WRONG));
    check("a wrong password does not", wrong);
    check(
        "after three of them even the right one waits",
        auth::bless(tid, NAME, FIRST, auth::CHECK) == Err(auth::ERR_WAIT),
    );
    syscall::sleep_ticks(130);
    check("and a moment later it opens again", auth::bless(tid, NAME, FIRST, auth::CHECK) == Ok((uid, gid)));

    // Whose task it is.
    check(
        "a task that is not the asker's child is not the asker's to have named",
        auth::bless(INIT_TID, NAME, FIRST, auth::CHECK) == Err(auth::ERR_NOT_YOURS),
    );
    match load_child(&[b"dchild", b"sleep"]) {
        Some(running) => {
            let running_tid = running.tid;
            let started = running.start().is_ok();
            check(
                "nor is a child that has been started",
                started
                    && auth::bless(running_tid, NAME, FIRST, auth::CHECK) == Err(auth::ERR_NOT_YOURS)
                    && syscall::sys_get_tuid(running_tid) == Ok((0, 0)),
            );
            let _ = syscall::sys_task_kill(running_tid);
            let _ = wait_for(running_tid);
        }
        None => check("loaded a child to start", false),
    }

    // A password changed is the old one gone.
    check("root changes the password", auth::passwd(NAME, b"", SECOND).is_ok());
    check(
        "the old one no longer opens it, and the new one does",
        auth::bless(tid, NAME, FIRST, auth::CHECK) == Err(auth::ERR_WRONG)
            && auth::bless(tid, NAME, SECOND, auth::CHECK) == Ok((uid, gid)),
    );
    child.discard();

    // As the user.
    check(
        "the user changes its own with the old one, and nobody else's",
        run_blessed(NAME, &[b"dchild", b"authuser", NAME, OTHER, SECOND, THIRD]) == Some(7),
    );
    let mut text = [0u8; 20];
    let other_id = decimal(other_uid as usize, &mut text);
    check(
        "an account that may not become another is not made one by its own password",
        run_blessed(NAME, &[b"dchild", b"become", OTHER, other_id, THIRD])
            == Some(100 + auth::ERR_NOT_ALLOWED as i32),
    );

    // What an account may do is what its sessions are handed.
    check("the account is given the right to become another", set_rights(v, NAME, Some(b"become"), work));
    check(
        "and then its own password makes it one",
        run_blessed(NAME, &[b"dchild", b"become", OTHER, other_id, THIRD]) == Some(0),
    );
    check(
        "though not a wrong one",
        run_blessed(NAME, &[b"dchild", b"become", OTHER, other_id, b"not it"]) == Some(100 + auth::ERR_WRONG as i32),
    );
    let handed = |rights: &[u8], work: &mut [u8]| {
        if !set_rights(v, NAME, Some(rights), work) {
            return None;
        }
        let child = load_child(&[b"dchild", b"quit"])?;
        let blessed = auth::bless(child.tid, NAME, b"", 0).is_ok();
        let has = (
            holds(child.tid, syscall::CAP_TYPE_TASK_MGMT),
            holds(child.tid, syscall::CAP_TYPE_IOPORT),
            holds(child.tid, syscall::CAP_TYPE_SET_UID),
        );
        child.discard();
        blessed.then_some(has)
    };
    check(
        "an account that may end programs is handed the right to, and no more",
        handed(b"tasks", work) == Some((1, 0, 0)),
    );
    check(
        "one that may turn the machine off, the ports that do it",
        handed(b"power", work) == Some((1, 3, 0)),
    );
    check(
        "and neither the right to say who anybody is",
        handed(b"power tasks become", work) == Some((1, 3, 0)),
    );

    check(
        "the accounts are taken away again",
        set_rights(v, NAME, None, work)
            && accounts::remove_user(v, b"", NAME, work).is_ok()
            && accounts::remove_user(v, b"", OTHER, work).is_ok(),
    );
    check(
        "and nobody is made a user that has gone",
        load_child(&[b"dchild", b"quit"]).is_some_and(|c| {
            let refused = auth::bless(c.tid, NAME, THIRD, auth::CHECK) == Err(auth::ERR_WRONG);
            c.discard();
            refused
        }),
    );
    let _ = syscall::sys_munmap(ACCOUNTS_AT, accounts::WORK / 4096);
}

/// A filesystem mounted in another: a server of its own, reached through the
/// one its directory is in.
fn test_mounts() {
    use quark_rt::block;
    use quark_rt::ipc::Message;
    println!("mounts:");
    let Some(vfs_tid) = nameserver::lookup(b"vfs") else {
        check("find the file server", false);
        return;
    };
    // What a mounted filesystem's server takes from the server above it —
    // "you are mine", "this is for user 0", "stop" — the root takes from
    // nobody. A server that believed the first would believe the second from
    // any program that said it, and end when that program did.
    let refused = |tag: u64| {
        let mut reply = Message::empty();
        let said = Message { sender: 0, tag, data: [0; 6] };
        syscall::sys_call(vfs_tid, &said, &mut reply).is_ok() && reply.tag == u64::MAX
    };
    check(
        "the root's file server is nobody's to adopt, to speak through or to stop",
        refused(31) && refused(32) && refused(33),
    );
    check(
        "and it is still there",
        vfs::open(vfs_tid, b"/etc/passwd").is_ok_and(|(h, _, _)| vfs::close(vfs_tid, h).is_ok()),
    );
    // The programs that make filesystems are a distribution's to bring.
    if vfs::open(vfs_tid, b"/usr/bin/mkfs.ext4").map(|(h, _, _)| vfs::close(vfs_tid, h)).is_err() {
        println!("  (no mkfs.ext4 here; nothing to mount)");
        return;
    }
    // Big enough for a FAT32 filesystem to be one: it wants 65525 clusters.
    let Some((server, disk, name)) = start_ram_disk(b"64", 64 * 2048) else {
        check("start a RAM disk", false);
        return;
    };
    let mut dev_text = [0u8; 16];
    let dev_len = dev_path(&name, 0, &mut dev_text);
    let dev = &dev_text[..dev_len];
    let vfs_pid = syscall::sys_pid(vfs_tid).unwrap_or(0);
    let holder = || block::info(disk, 0).map_or(u64::MAX, |i| i.claimant);
    let at: &[u8] = b"/tmp/dtest-mnt";

    // Whatever an earlier run left.
    let _ = run(b"umount", &[at]);
    let _ = vfs::unlink(vfs_tid, b"/tmp/dtest-mnt/under");
    let _ = vfs::mkdir(vfs_tid, at);
    check("a filesystem is made on a disk of memory", run(b"mkfs.ext4", &[b"-q", b"-F", dev]) == Some(0));
    check("a file is left in the directory it will be mounted on", spill(vfs_tid, b"/tmp/dtest-mnt/under", b"under").is_ok());
    let root_before = vfs::lstat(vfs_tid, at).map(|s| s.id);

    check("it is mounted there", run(b"mount", &[dev, at]) == Some(0));
    let server_pid = holder();
    check(
        "by a server of its own, which holds the disk",
        server_pid != 0 && server_pid != u64::MAX && server_pid != vfs_pid,
    );
    let mut record = [0u8; 512];
    let listed = (0..8).find_map(|i| match vfs::mounted(vfs_tid, i, &mut record) {
        Ok(Some(m)) if vfs::mount_record(&record[..m.len]).1 == at => Some(m),
        _ => None,
    });
    check(
        "and is listed: what, where, of what kind and served by whom",
        listed.is_some_and(|m| {
            vfs::mount_record(&record[..m.len]).0 == dev && m.kind == vfs::KIND_EXT4 && m.pid == server_pid
        }),
    );
    let mut text = [0u8; 256];
    check(
        "/etc/mtab says so too, for programs that look there",
        slurp(vfs_tid, b"/etc/mtab", &mut text).is_ok_and(|n| {
            text[..n].split(|&b| b == b'\n').any(|line| {
                let mut words = line.split(|&b| b == b' ');
                words.next() == Some(dev) && words.next() == Some(at) && words.next() == Some(b"ext4")
            })
        }),
    );
    check(
        "what the directory held is out of sight",
        vfs::open(vfs_tid, b"/tmp/dtest-mnt/under").err() == Some(vfs::ERR_NOT_FOUND),
    );
    check(
        "and the filesystem's own root is in its place",
        vfs::open(vfs_tid, b"/tmp/dtest-mnt/lost+found").is_ok_and(|(h, _, dir)| dir && vfs::close(vfs_tid, h).is_ok()),
    );
    let root_now = vfs::lstat(vfs_tid, at).map(|s| s.id);
    check(
        "whose id is not the directory's, nor any file's of the filesystem around it",
        matches!((root_before, root_now), (Ok(a), Ok(b)) if a != b && b >> 40 != 0 && a >> 40 == 0),
    );

    // Files.
    let mut pattern = [0u8; 10000];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut back = [0u8; 10016];
    check(
        "a file is written there, pages of it, and read back",
        spill(vfs_tid, b"/tmp/dtest-mnt/a", &pattern).is_ok()
            && slurp(vfs_tid, b"/tmp/dtest-mnt/a", &mut back) == Ok(10000)
            && back[..10000] == pattern[..],
    );
    let a = vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/a");
    check("it is as long as what was written, and root's", a.is_ok_and(|s| s.size == 10000 && s.uid == 0));
    check("a directory is made there", vfs::mkdir(vfs_tid, b"/tmp/dtest-mnt/d").is_ok());
    check("and a file in it", spill(vfs_tid, b"/tmp/dtest-mnt/d/b", b"in a directory\n").is_ok());
    let b_id = vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/b").map_or(0, |s| s.id);
    let mut entries = [vfs::DirEntry::empty(); 8];
    let listing = vfs::open_with(vfs_tid, b"/tmp/dtest-mnt/d", vfs::OPEN_DIRECTORY).and_then(|o| {
        let page = vfs::readdir_bulk(vfs_tid, o.handle, 0, &mut entries);
        let _ = vfs::close(vfs_tid, o.handle);
        page
    });
    check(
        "the directory lists it, by the id stat gives it",
        listing.is_ok_and(|p| entries[..p.count].iter().any(|e| e.name_bytes() == b"b" && e.id == b_id && b_id != 0)),
    );
    check(
        "a rename inside the filesystem",
        vfs::rename(vfs_tid, b"/tmp/dtest-mnt/a", b"/tmp/dtest-mnt/d/a2").is_ok()
            && vfs::open(vfs_tid, b"/tmp/dtest-mnt/a").err() == Some(vfs::ERR_NOT_FOUND)
            && vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/a2").is_ok_and(|s| s.size == 10000),
    );
    check(
        "but not out of it: that is two filesystems",
        vfs::rename(vfs_tid, b"/tmp/dtest-mnt/d/a2", b"/tmp/dtest-out") == Err(vfs::ERR_CROSS_DEVICE)
            && vfs::rename(vfs_tid, b"/etc/passwd", b"/tmp/dtest-mnt/passwd") == Err(vfs::ERR_CROSS_DEVICE),
    );
    check(
        "nor a second name for a file in one made in the other",
        vfs::link(vfs_tid, b"/tmp/dtest-mnt/d/a2", b"/tmp/dtest-out") == Err(vfs::ERR_CROSS_DEVICE),
    );
    check(
        "a second name inside it is the same file",
        vfs::link(vfs_tid, b"/tmp/dtest-mnt/d/a2", b"/tmp/dtest-mnt/a3").is_ok()
            && vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/a3").is_ok_and(|s| s.links == 2 && Ok(s.id) == vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/a2").map(|t| t.id)),
    );
    let mut target = [0u8; 16];
    check(
        "a symbolic link there says what it was given",
        vfs::symlink(vfs_tid, b"d/b", b"/tmp/dtest-mnt/l").is_ok()
            && vfs::readlink(vfs_tid, b"/tmp/dtest-mnt/l", &mut target) == Ok(3)
            && &target[..3] == b"d/b",
    );
    check(
        "and is followed there",
        slurp(vfs_tid, b"/tmp/dtest-mnt/l", &mut text).is_ok_and(|n| &text[..n] == b"in a directory\n"),
    );
    check(
        "a file's mode is changed there",
        vfs::set_attr(vfs_tid, b"/tmp/dtest-mnt/d/b", vfs::ATTR_MODE, 0o600, 0, 0, 0, 0).is_ok()
            && vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/b").is_ok_and(|s| s.mode & 0o7777 == 0o600),
    );
    let shorter = vfs::open(vfs_tid, b"/tmp/dtest-mnt/a3").and_then(|(h, _, _)| {
        let cut = vfs::truncate(vfs_tid, h, 100);
        let size = vfs::stat(vfs_tid, h).map(|(size, _)| size);
        let _ = vfs::close(vfs_tid, h);
        cut.and(size)
    });
    check("a file there is cut short", shorter == Ok(100));
    check(
        "the filesystem says how big it is, and it is not the root",
        vfs::open(vfs_tid, b"/tmp/dtest-mnt/d").is_ok_and(|(h, _, _)| {
            let there = vfs::statfs_of(vfs_tid, h);
            let _ = vfs::close(vfs_tid, h);
            // Sixty-four megabytes in blocks of a kilobyte.
            matches!((there, vfs::statfs(vfs_tid)), (Ok(a), Ok(b)) if a.blocks == 64 * 1024 && a.blocks != b.blocks)
        }),
    );
    check(
        "names go, and the directory after them",
        vfs::unlink(vfs_tid, b"/tmp/dtest-mnt/a3").is_ok()
            && vfs::unlink(vfs_tid, b"/tmp/dtest-mnt/l").is_ok()
            && vfs::rmdir(vfs_tid, b"/tmp/dtest-mnt/d") == Err(vfs::ERR_NOT_EMPTY),
    );
    check(
        "the mount's own directory is not removed, renamed or made again",
        vfs::rmdir(vfs_tid, at) == Err(vfs::ERR_BUSY)
            && vfs::rename(vfs_tid, at, b"/tmp/dtest-elsewhere").is_err()
            && vfs::mkdir(vfs_tid, at) == Err(vfs::ERR_EXISTS),
    );

    // Somebody who is not root, asking through the server above: this one
    // is told who is asking, and in which groups.
    if syscall::sys_get_uid().0 == 0 && holds_set_uid() {
        as_a_user(vfs_tid, b"/tmp/dtest-mnt/users", AS_USER_MOUNTED);
    }

    // Being in it.
    let mut cwd = [0u8; 64];
    check(
        "a program moves into a directory there, and is told where it is",
        vfs::chdir(vfs_tid, b"/tmp/dtest-mnt/d").is_ok()
            && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == b"/tmp/dtest-mnt/d"),
    );
    check(
        "a path from there is looked up there",
        slurp(vfs_tid, b"b", &mut text).is_ok_and(|n| &text[..n] == b"in a directory\n")
            && slurp(vfs_tid, b"../d/b", &mut text).is_ok(),
    );
    check(
        "`..` goes up inside it",
        vfs::chdir(vfs_tid, b"..").is_ok() && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == at),
    );
    check(
        "and out of it from its root",
        slurp(vfs_tid, b"../dtest-mnt/d/b", &mut text).is_ok()
            && vfs::chdir(vfs_tid, b"..").is_ok()
            && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == b"/tmp"),
    );
    check(
        "as it does written after the mount's name",
        matches!(
            (vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/.."), vfs::lstat(vfs_tid, b"/tmp")),
            (Ok(a), Ok(b)) if a.id == b.id
        ),
    );
    let _ = vfs::chdir(vfs_tid, b"/");

    // In use.
    let open = vfs::open(vfs_tid, b"/tmp/dtest-mnt/d/b").map(|(h, _, _)| h);
    check("it is not unmounted with a file in it open", open.is_ok() && run(b"umount", &[at]) == Some(1));
    if let Ok(h) = open {
        let _ = vfs::close(vfs_tid, h);
    }
    check(
        "or with a program in it",
        vfs::chdir(vfs_tid, at).is_ok() && run(b"umount", &[at]) == Some(1) && vfs::chdir(vfs_tid, b"/").is_ok(),
    );
    check("it is unmounted when nothing is using it", run(b"umount", &[at]) == Some(0));
    // Its server ends on its own time.
    for _ in 0..50 {
        if holder() == 0 {
            break;
        }
        syscall::sleep_ticks(2);
    }
    check("and its server lets the disk go", holder() == 0);
    check(
        "the directory is what it was",
        slurp(vfs_tid, b"/tmp/dtest-mnt/under", &mut text).is_ok_and(|n| &text[..n] == b"under")
            && vfs::lstat(vfs_tid, at).map(|s| s.id) == root_before,
    );
    check("nothing is unmounted twice", run(b"umount", &[at]) == Some(1));

    // What was written is on the disk.
    check(
        "mounted again, what was written is there",
        run(b"mount", &[dev, at]) == Some(0)
            && slurp(vfs_tid, b"/tmp/dtest-mnt/d/a2", &mut back) == Ok(100)
            && back[..100] == pattern[..100]
            && vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/b").is_ok_and(|s| s.mode & 0o7777 == 0o600),
    );
    // A filesystem mounted in a mounted filesystem: two servers deep.
    if let Some((inner_server, _, inner_name)) = start_ram_disk(b"24", 24 * 2048) {
        let mut inner_text = [0u8; 16];
        let inner_len = dev_path(&inner_name, 0, &mut inner_text);
        let inner = &inner_text[..inner_len];
        let inside: &[u8] = b"/tmp/dtest-mnt/in";
        check(
            "a second filesystem is mounted on a directory inside the first",
            run(b"mkfs.ext2", &[b"-q", b"-F", inner]) == Some(0)
                && run(b"mount", &[b"--mkdir", inner, inside]) == Some(0),
        );
        check(
            "a file in it is written and read through both servers",
            spill(vfs_tid, b"/tmp/dtest-mnt/in/deep", &pattern[..6000]).is_ok()
                && slurp(vfs_tid, b"/tmp/dtest-mnt/in/deep", &mut back) == Ok(6000)
                && back[..6000] == pattern[..6000],
        );
        let deep = vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/in/deep").map(|s| s.id);
        let shallow = vfs::lstat(vfs_tid, b"/tmp/dtest-mnt/d/b").map(|s| s.id);
        check(
            "its id says both mounts it is under",
            matches!((deep, shallow), (Ok(a), Ok(b)) if a >> 44 != 0 && b >> 44 == 0 && b >> 40 != 0),
        );
        check(
            "a file does not move from the one to the other",
            vfs::rename(vfs_tid, b"/tmp/dtest-mnt/in/deep", b"/tmp/dtest-mnt/deep") == Err(vfs::ERR_CROSS_DEVICE),
        );
        let moved = vfs::chdir(vfs_tid, inside).is_ok()
            && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == inside)
            && vfs::chdir(vfs_tid, b"..").is_ok()
            && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == at);
        let _ = vfs::chdir(vfs_tid, b"/");
        check("a program is in it, is told where, and goes up into the first", moved);
        let mut seen = [0usize; 2];
        for index in 0..8 {
            if let Ok(Some(m)) = vfs::mounted(vfs_tid, index, &mut record) {
                let target = vfs::mount_record(&record[..m.len]).1;
                if target == at {
                    seen[0] = index as usize + 1;
                } else if target == inside {
                    seen[1] = index as usize + 1;
                }
            }
        }
        check("both are listed, the one inside after the one it is in", seen[0] != 0 && seen[1] == seen[0] + 1);
        check("the outer is not unmounted with another inside it", run(b"umount", &[at]) == Some(1));
        check("the inner is", run(b"umount", &[inside]) == Some(0));
        let _ = syscall::sys_task_kill(inner_server);
        let _ = wait_for(inner_server);
    } else {
        check("start a second RAM disk", false);
    }
    check("a disk with a mounted filesystem is not opened to write", {
        let busy = vfs::open_with(vfs_tid, dev, vfs::OPEN_WRITE).err() == Some(vfs::ERR_BUSY);
        busy && run(b"mkfs.ext4", &[b"-q", b"-F", dev]).is_some_and(|code| code != 0)
    });
    check("and it is unmounted", run(b"umount", &[at]) == Some(0));
    check("the filesystem's own checker finds nothing wrong", run(b"e2fsck", &[b"-fn", dev]) == Some(0));

    // FAT, as an EFI system partition is.
    check("a FAT filesystem is made and mounted", {
        run(b"mkfs.fat", &[b"-F", b"32", dev]) == Some(0) && run(b"mount", &[dev, at]) == Some(0)
    });
    check(
        "a directory and a file are made in it and read back",
        vfs::mkdir(vfs_tid, b"/tmp/dtest-mnt/EFI").is_ok()
            && spill(vfs_tid, b"/tmp/dtest-mnt/EFI/BOOT.BIN", &pattern[..5000]).is_ok()
            && slurp(vfs_tid, b"/tmp/dtest-mnt/EFI/BOOT.BIN", &mut back) == Ok(5000)
            && back[..5000] == pattern[..5000],
    );
    let inside = vfs::chdir(vfs_tid, b"/tmp/dtest-mnt/EFI").is_ok()
        && vfs::getcwd(vfs_tid, &mut cwd).is_ok_and(|n| &cwd[..n] == b"/tmp/dtest-mnt/EFI")
        && slurp(vfs_tid, b"BOOT.BIN", &mut back) == Ok(5000);
    let _ = vfs::chdir(vfs_tid, b"/");
    check("a program in a directory of it reads by a name from there", inside);
    check(
        "a file is written over, shorter, and is then that short",
        spill(vfs_tid, b"/tmp/dtest-mnt/EFI/BOOT.BIN", &pattern[..700]).is_ok()
            && slurp(vfs_tid, b"/tmp/dtest-mnt/EFI/BOOT.BIN", &mut back) == Ok(700)
            && back[..700] == pattern[..700],
    );
    check(
        "an empty file is made, and a name is removed",
        spill(vfs_tid, b"/tmp/dtest-mnt/EMPTY", b"").is_ok()
            && spill(vfs_tid, b"/tmp/dtest-mnt/GONE.TXT", &pattern[..3000]).is_ok()
            && vfs::unlink(vfs_tid, b"/tmp/dtest-mnt/GONE.TXT").is_ok()
            && vfs::open(vfs_tid, b"/tmp/dtest-mnt/GONE.TXT").err() == Some(vfs::ERR_NOT_FOUND)
            && vfs::rmdir(vfs_tid, b"/tmp/dtest-mnt/EFI") == Err(vfs::ERR_NOT_EMPTY),
    );
    // Nothing in it is anybody's, so there is nothing to ask of a user but
    // whether it is root.
    if syscall::sys_get_uid().0 == 0 && holds_set_uid() {
        check(
            "it is anybody's to read, and root's alone to change",
            run_as_user(&[b"dchild", b"userfat", b"/tmp/dtest-mnt/EFI", b"BOOT.BIN"]) == Some(3),
        );
    }
    check("it is unmounted", run(b"umount", &[at]) == Some(0));
    check("and its checker finds nothing wrong", run(b"fsck.fat", &[b"-n", dev]) == Some(0));

    // A server that goes.
    check("a filesystem is mounted", {
        run(b"mkfs.ext2", &[b"-q", b"-F", dev]) == Some(0) && run(b"mount", &[dev, at]) == Some(0)
    });
    let doomed = holder();
    check("its server is ended", syscall::sys_sig_raise_pid(doomed, syscall::SIGKILL).is_ok());
    for _ in 0..50 {
        if holder() == 0 {
            break;
        }
        syscall::sleep_ticks(2);
    }
    check(
        "and what was mounted cannot be reached, though the root can",
        vfs::open(vfs_tid, b"/tmp/dtest-mnt/lost+found").err() == Some(vfs::ERR_IO)
            && vfs::open(vfs_tid, b"/etc/passwd").is_ok_and(|(h, _, _)| vfs::close(vfs_tid, h).is_ok()),
    );
    check(
        "it is unmounted, and the directory is a directory again",
        run(b"umount", &[at]) == Some(0)
            && slurp(vfs_tid, b"/tmp/dtest-mnt/under", &mut text).is_ok_and(|n| &text[..n] == b"under"),
    );

    let _ = vfs::unlink(vfs_tid, b"/tmp/dtest-mnt/under");
    let _ = vfs::rmdir(vfs_tid, at);
    let _ = syscall::sys_task_kill(server);
    let _ = wait_for(server);
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

/// The wider registers: sixteen of 256 bits (AVX), and where the processor
/// has them thirty-two of 512 and seven mask registers (AVX-512). A word of
/// each task's pattern in every sixty-four bits of every one.
fn wide_pattern(seed: u64, words: &mut [u64]) {
    for (i, word) in words.iter_mut().enumerate() {
        *word = fpu_pattern(seed, i as u64);
    }
}

/// What a program looks at to know whether it may use the wide registers:
/// that the processor has AVX, that the kernel has turned on the saving of
/// more than SSE's state, and which state it said it saves. Whether the
/// processor has AVX, whether a program may use it, and whether it may use
/// AVX-512.
fn wide_registers() -> (bool, bool, bool) {
    let features = core::arch::x86_64::__cpuid(1);
    let has = features.ecx & (1 << 28) != 0;
    // The kernel's part: OSXSAVE, and then what it saves.
    if !has || features.ecx & (1 << 27) == 0 {
        return (has, false, false);
    }
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    let saved = (hi as u64) << 32 | lo as u64;
    let avx512 = core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 16) != 0;
    (has, saved & 0b110 == 0b110, avx512 && saved & 0xE6 == 0xE6)
}

unsafe fn ymm_load(vals: &[u64; 64]) {
    unsafe {
        core::arch::asm!(
            "vmovdqu ymm0, [{v} + 0*32]",
            "vmovdqu ymm1, [{v} + 1*32]",
            "vmovdqu ymm2, [{v} + 2*32]",
            "vmovdqu ymm3, [{v} + 3*32]",
            "vmovdqu ymm4, [{v} + 4*32]",
            "vmovdqu ymm5, [{v} + 5*32]",
            "vmovdqu ymm6, [{v} + 6*32]",
            "vmovdqu ymm7, [{v} + 7*32]",
            "vmovdqu ymm8, [{v} + 8*32]",
            "vmovdqu ymm9, [{v} + 9*32]",
            "vmovdqu ymm10, [{v} + 10*32]",
            "vmovdqu ymm11, [{v} + 11*32]",
            "vmovdqu ymm12, [{v} + 12*32]",
            "vmovdqu ymm13, [{v} + 13*32]",
            "vmovdqu ymm14, [{v} + 14*32]",
            "vmovdqu ymm15, [{v} + 15*32]",
            v = in(reg) vals.as_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

unsafe fn ymm_read() -> [u64; 64] {
    let mut vals = [0u64; 64];
    unsafe {
        core::arch::asm!(
            "vmovdqu [{v} + 0*32], ymm0",
            "vmovdqu [{v} + 1*32], ymm1",
            "vmovdqu [{v} + 2*32], ymm2",
            "vmovdqu [{v} + 3*32], ymm3",
            "vmovdqu [{v} + 4*32], ymm4",
            "vmovdqu [{v} + 5*32], ymm5",
            "vmovdqu [{v} + 6*32], ymm6",
            "vmovdqu [{v} + 7*32], ymm7",
            "vmovdqu [{v} + 8*32], ymm8",
            "vmovdqu [{v} + 9*32], ymm9",
            "vmovdqu [{v} + 10*32], ymm10",
            "vmovdqu [{v} + 11*32], ymm11",
            "vmovdqu [{v} + 12*32], ymm12",
            "vmovdqu [{v} + 13*32], ymm13",
            "vmovdqu [{v} + 14*32], ymm14",
            "vmovdqu [{v} + 15*32], ymm15",
            v = in(reg) vals.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
    vals
}

unsafe fn zmm_load(vals: &[u64; 256], masks: &[u64; 8]) {
    unsafe {
        core::arch::asm!(
            "vmovdqu64 zmm0, [{v} + 0*64]",
            "vmovdqu64 zmm1, [{v} + 1*64]",
            "vmovdqu64 zmm2, [{v} + 2*64]",
            "vmovdqu64 zmm3, [{v} + 3*64]",
            "vmovdqu64 zmm4, [{v} + 4*64]",
            "vmovdqu64 zmm5, [{v} + 5*64]",
            "vmovdqu64 zmm6, [{v} + 6*64]",
            "vmovdqu64 zmm7, [{v} + 7*64]",
            "vmovdqu64 zmm8, [{v} + 8*64]",
            "vmovdqu64 zmm9, [{v} + 9*64]",
            "vmovdqu64 zmm10, [{v} + 10*64]",
            "vmovdqu64 zmm11, [{v} + 11*64]",
            "vmovdqu64 zmm12, [{v} + 12*64]",
            "vmovdqu64 zmm13, [{v} + 13*64]",
            "vmovdqu64 zmm14, [{v} + 14*64]",
            "vmovdqu64 zmm15, [{v} + 15*64]",
            "vmovdqu64 zmm16, [{v} + 16*64]",
            "vmovdqu64 zmm17, [{v} + 17*64]",
            "vmovdqu64 zmm18, [{v} + 18*64]",
            "vmovdqu64 zmm19, [{v} + 19*64]",
            "vmovdqu64 zmm20, [{v} + 20*64]",
            "vmovdqu64 zmm21, [{v} + 21*64]",
            "vmovdqu64 zmm22, [{v} + 22*64]",
            "vmovdqu64 zmm23, [{v} + 23*64]",
            "vmovdqu64 zmm24, [{v} + 24*64]",
            "vmovdqu64 zmm25, [{v} + 25*64]",
            "vmovdqu64 zmm26, [{v} + 26*64]",
            "vmovdqu64 zmm27, [{v} + 27*64]",
            "vmovdqu64 zmm28, [{v} + 28*64]",
            "vmovdqu64 zmm29, [{v} + 29*64]",
            "vmovdqu64 zmm30, [{v} + 30*64]",
            "vmovdqu64 zmm31, [{v} + 31*64]",
            "kmovq k1, [{k} + 1*8]",
            "kmovq k2, [{k} + 2*8]",
            "kmovq k3, [{k} + 3*8]",
            "kmovq k4, [{k} + 4*8]",
            "kmovq k5, [{k} + 5*8]",
            "kmovq k6, [{k} + 6*8]",
            "kmovq k7, [{k} + 7*8]",
            v = in(reg) vals.as_ptr(),
            k = in(reg) masks.as_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

unsafe fn zmm_read(vals: &mut [u64; 256], masks: &mut [u64; 8]) {
    unsafe {
        core::arch::asm!(
            "vmovdqu64 [{v} + 0*64], zmm0",
            "vmovdqu64 [{v} + 1*64], zmm1",
            "vmovdqu64 [{v} + 2*64], zmm2",
            "vmovdqu64 [{v} + 3*64], zmm3",
            "vmovdqu64 [{v} + 4*64], zmm4",
            "vmovdqu64 [{v} + 5*64], zmm5",
            "vmovdqu64 [{v} + 6*64], zmm6",
            "vmovdqu64 [{v} + 7*64], zmm7",
            "vmovdqu64 [{v} + 8*64], zmm8",
            "vmovdqu64 [{v} + 9*64], zmm9",
            "vmovdqu64 [{v} + 10*64], zmm10",
            "vmovdqu64 [{v} + 11*64], zmm11",
            "vmovdqu64 [{v} + 12*64], zmm12",
            "vmovdqu64 [{v} + 13*64], zmm13",
            "vmovdqu64 [{v} + 14*64], zmm14",
            "vmovdqu64 [{v} + 15*64], zmm15",
            "vmovdqu64 [{v} + 16*64], zmm16",
            "vmovdqu64 [{v} + 17*64], zmm17",
            "vmovdqu64 [{v} + 18*64], zmm18",
            "vmovdqu64 [{v} + 19*64], zmm19",
            "vmovdqu64 [{v} + 20*64], zmm20",
            "vmovdqu64 [{v} + 21*64], zmm21",
            "vmovdqu64 [{v} + 22*64], zmm22",
            "vmovdqu64 [{v} + 23*64], zmm23",
            "vmovdqu64 [{v} + 24*64], zmm24",
            "vmovdqu64 [{v} + 25*64], zmm25",
            "vmovdqu64 [{v} + 26*64], zmm26",
            "vmovdqu64 [{v} + 27*64], zmm27",
            "vmovdqu64 [{v} + 28*64], zmm28",
            "vmovdqu64 [{v} + 29*64], zmm29",
            "vmovdqu64 [{v} + 30*64], zmm30",
            "vmovdqu64 [{v} + 31*64], zmm31",
            "kmovq [{k} + 1*8], k1",
            "kmovq [{k} + 2*8], k2",
            "kmovq [{k} + 3*8], k3",
            "kmovq [{k} + 4*8], k4",
            "kmovq [{k} + 5*8], k5",
            "kmovq [{k} + 6*8], k6",
            "kmovq [{k} + 7*8], k7",
            v = in(reg) vals.as_mut_ptr(),
            k = in(reg) masks.as_mut_ptr(),
            options(nostack, preserves_flags),
        );
    }
}

/// Whether the task that used the wide registers after this one found them
/// empty, as a new task's are; and whether it is to use the widest.
static WIDE_EMPTY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static WIDEST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static mut WIDE_VALUES: [u64; 256] = [0; 256];

extern "C" fn wide_worker() -> ! {
    use core::sync::atomic::Ordering;
    FPU_GO.acquire();
    // A new task's are nought: not what the task that made it had in them,
    // and not what whoever ran last left.
    let found = unsafe { ymm_read() };
    WIDE_EMPTY.store(found.iter().all(|&word| word == 0), Ordering::SeqCst);
    // And then they are this task's.
    let values = unsafe { &mut *(&raw mut WIDE_VALUES) };
    wide_pattern(0xB0B, values);
    unsafe {
        if WIDEST.load(Ordering::SeqCst) {
            let mut masks = [0u64; 8];
            wide_pattern(0xB0B5, &mut masks);
            zmm_load(values, &masks);
        } else {
            ymm_load(values[..64].try_into().unwrap());
        }
    }
    FPU_RAN.release();
    syscall::sys_exit_code(0);
}

/// The wide registers are a task's own as the SSE ones are, where a program
/// may use them at all.
fn test_wide_registers() {
    use core::sync::atomic::Ordering;
    let (has, may, widest) = wide_registers();
    if !has {
        println!("        this processor has no AVX: the wide registers are not checked");
        return;
    }
    check("the kernel says the wide registers may be used", may);
    if !may {
        return;
    }
    WIDEST.store(widest, Ordering::SeqCst);
    let Ok(_t) = thread::spawn_with_stack(wide_worker, 16) else {
        check("start a task to share the wide registers with", false);
        return;
    };
    let mut mine = [0u64; 256];
    let mut masks = [0u64; 8];
    wide_pattern(0xA11CE, &mut mine);
    wide_pattern(0xA11CE5, &mut masks);
    let (mut found, mut found_masks) = ([0u64; 256], [0u64; 8]);
    unsafe {
        if widest {
            zmm_load(&mine, &masks);
        } else {
            ymm_load(mine[..64].try_into().unwrap());
        }
    }
    FPU_GO.release();
    // Blocks, so the worker runs and loads its own pattern.
    FPU_RAN.acquire();
    unsafe {
        if widest {
            zmm_read(&mut found, &mut found_masks);
        } else {
            found[..64].copy_from_slice(&ymm_read());
        }
    }
    // The upper half of each of the first sixteen is what SSE never saved,
    // and is the half that says; the lower half is a register compiled code
    // may use for its own purposes between the two looks.
    let upper = |i: usize| i % 4 >= 2;
    let kept = (0..64).filter(|&i| upper(i)).all(|i| found[i] == mine[i]);
    check("the upper half of every wide register survives another task using them", kept);
    check("and a new task finds them empty", WIDE_EMPTY.load(Ordering::SeqCst));
    if widest {
        // Thirty-two registers of eight words: of the first sixteen, the
        // words above the first two; of the rest, every word.
        let wider = |i: usize| i / 8 >= 16 || i % 8 >= 2;
        check(
            "and so does every register of the widest kind, and every mask register",
            (0..256).filter(|&i| wider(i)).all(|i| found[i] == mine[i]) && found_masks[1..] == masks[1..],
        );
    } else {
        println!("        this processor has no AVX-512: the widest registers are not checked");
    }
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
    test_wide_registers();
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

// --- more than one processor ---

/// Where the counters of a child that spins are mapped: a `u64` for each of
/// its threads, which it adds to without making a call.
const BEATS_AT: usize = 0xAB_0000_0000;
/// A page one thread reads while another unmaps it.
const GONE_AT: usize = 0xAC_0000_0000;
const CALLERS_SLOT: usize = 47;

fn beat(slot: usize) -> u64 {
    unsafe { core::ptr::read_volatile((BEATS_AT + slot * 8) as *const u64) }
}

/// Whether every one of the first `threads` counters moves within `ticks`.
fn all_beating(threads: usize, ticks: u64) -> bool {
    let mut before = [0u64; 4];
    for (slot, was) in before.iter_mut().enumerate().take(threads) {
        *was = beat(slot);
    }
    let start = syscall::sys_ticks();
    while syscall::sys_ticks() - start < ticks {
        if (0..threads).all(|slot| beat(slot) != before[slot]) {
            return true;
        }
        syscall::sleep_ticks(1);
    }
    false
}

/// Whether none of the first `threads` counters moves in five ticks.
fn none_beating(threads: usize) -> bool {
    let mut before = [0u64; 4];
    for (slot, was) in before.iter_mut().enumerate().take(threads) {
        *was = beat(slot);
    }
    syscall::sleep_ticks(5);
    (0..threads).all(|slot| beat(slot) == before[slot])
}

/// Start `dchild spin N` counting in `memory`, which is mapped at
/// [`BEATS_AT`] here: its task.
fn start_spinner(memory: usize, threads: &[u8]) -> Option<usize> {
    let child = load_child(&[b"dchild", b"spin", threads])?;
    let tid = child.tid;
    if syscall::sys_fd_dup(tid, 3, memory).is_err() {
        child.discard();
        return None;
    }
    child.start().ok()?;
    Some(tid)
}

static GONE_READS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static GONE_STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Reads one page for ever, so that the processor it runs on always has the
/// page's translation to hand.
extern "C" fn gone_reader() -> ! {
    use core::sync::atomic::Ordering;
    while !GONE_STOP.load(Ordering::Relaxed) {
        unsafe { core::ptr::read_volatile(GONE_AT as *const u64) };
        GONE_READS.fetch_add(1, Ordering::Relaxed);
    }
    syscall::sys_exit_code(0);
}

static CALLERS_TO: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static CALLERS_STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// How many calls the second caller made, and whether every one was
/// answered with its own answer.
static CALLERS_MADE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CALLERS_WRONG: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Calls the echo server until told to stop, from its own thread — and so,
/// with more than one processor, at the same time as the thread that
/// started it is calling the same server.
extern "C" fn second_caller() -> ! {
    use core::sync::atomic::Ordering;
    use quark_rt::ipc::Message;
    let to = CALLERS_TO.load(Ordering::Relaxed);
    let mut made = 0u64;
    let mut reply = Message::empty();
    while !CALLERS_STOP.load(Ordering::Relaxed) {
        made += 1;
        // Tags of its own, so that an answer meant for the other caller
        // cannot pass for one meant for this.
        let ask = Message { sender: 0, tag: (1 << 40) | made, data: [0; 6] };
        let outcome = syscall::sys_call_timeout(to, &ask, &mut reply, 100);
        if !matches!(outcome, syscall::CallOutcome::Replied) || reply.tag != ask.tag + 1 {
            CALLERS_WRONG.store(true, Ordering::Relaxed);
            break;
        }
    }
    CALLERS_MADE.store(made, Ordering::Relaxed);
    syscall::sys_exit_code(0);
}

/// What a second processor changes, held to what one processor does: a
/// task that is not the caller may be running, in ring 3, at the moment
/// something is done to it.
///
/// Every check here is true with one processor too, except the first two,
/// which say what the number of processors is and whether two threads were
/// seen to run at once — and those are held to that number, either way.
fn test_smp() {
    use core::sync::atomic::Ordering;
    use quark_rt::ipc::Message;
    use syscall::ChildNews;
    println!("processors:");
    let (count, on) = syscall::sys_cpus();
    println!("        {} of them, and this is number {}", count, on);
    check(
        "the kernel says how many processors there are, and which this is",
        (1..=16).contains(&count) && on < count,
    );

    // Two threads that can only get anywhere if both are running.
    let together = run(b"dchild", &[b"together"]);
    check(
        "two threads run at the same time, if and only if there are two processors",
        matches!(together, Some(bits) if bits & !3 == 0 && (bits & 1 != 0) == (count > 1)),
    );
    check(
        "and are then on different ones",
        matches!(together, Some(bits) if bits & !3 == 0 && (bits & 2 != 0) == (count > 1)),
    );

    // A program whose three threads are in ring 3 whenever anybody looks,
    // and never in the kernel: what it is to end one, and to stop one, from
    // a processor it is not on.
    let memory = syscall::sys_memfd_create(1).ok().filter(|&fd| syscall::sys_mmap_fd(fd, BEATS_AT).is_ok());
    let (free_before, _) = syscall::sys_mem_info();
    let spinner = memory.and_then(|fd| start_spinner(fd, b"2"));
    check("a program's threads all run", spinner.is_some() && all_beating(3, 100));
    if let Some(tid) = spinner {
        check(
            "one running on other processors is ended, and is there to collect",
            syscall::sys_task_kill(tid).is_ok() && wait_for(tid) == Some(-9),
        );
        // Its other threads were told a moment after its first: none of them
        // is what this task waited for.
        syscall::sleep_ticks(2);
        check("and none of its threads goes on running", none_beating(3));
        let (free_after, _) = syscall::sys_mem_info();
        check("and its memory comes back", free_after + 64 >= free_before);
    }

    let spinner = memory.and_then(|fd| start_spinner(fd, b"1"));
    if let Some(tid) = spinner.filter(|_| all_beating(2, 100)) {
        let pid = syscall::sys_pid(tid).unwrap_or(0);
        check(
            "a program stopped while it runs on other processors is said to have stopped",
            syscall::sys_sig_raise_pid(pid, syscall::SIGSTOP).is_ok()
                && syscall::sys_wait_job(pid, syscall::WAIT_STOPPED) == Ok(Some(ChildNews::Stopped(pid, 19))),
        );
        syscall::sleep_ticks(2);
        check("and has: none of its threads is running", none_beating(2));
        check(
            "and every one of them goes on when it is continued",
            syscall::sys_sig_raise_pid(pid, syscall::SIGCONT).is_ok() && all_beating(2, 100),
        );
        let _ = syscall::sys_task_kill(tid);
        let _ = wait_for(tid);
    } else {
        check("start a program to stop", false);
    }
    if let Some(fd) = memory {
        let _ = syscall::sys_munmap(BEATS_AT, 1);
        let _ = syscall::sys_fd_close(fd);
    }

    // A child that ends while its parent waits for it is the parent's to
    // collect: woken for it, the parent says which child it was and takes
    // it apart. A processor with nothing to do also takes dead tasks apart,
    // and once got to this one first — the parent was told that process 0
    // had ended. Many times, because it was a race.
    let mut named = 0;
    for _ in 0..40 {
        let Some(child) = load_child(&[b"dchild", b"quit"]) else { break };
        let tid = child.tid;
        let pid = syscall::sys_pid(tid).unwrap_or(0);
        if child.start().is_ok() && syscall::sys_wait_for_pid(pid) == Ok((pid, 0)) {
            named += 1;
        }
    }
    check("a child waited for by name is the child the wait answers with, every time", named == 40);

    // A processor remembers where a page is. Taking the page away has to
    // reach every processor that has the program's memory loaded.
    check(
        "a page one thread replaces is the new page to a thread on another processor",
        run(b"dchild", &[b"tlb"]) == Some(0),
    );
    // And the other half: the page is simply gone. The thread reading it is
    // made to ask this task what to do about its faults, so that the fault
    // is something to hear of and not the end of the program.
    let me = syscall::sys_getpid() as usize;
    let gone = syscall::sys_mmap(GONE_AT, 1).ok().and_then(|()| {
        GONE_STOP.store(false, Ordering::Relaxed);
        let reader = thread::spawn_with_stack(gone_reader, 4).ok()?;
        syscall::sys_set_pager(reader.tid(), me).ok()?;
        let start = syscall::sys_ticks();
        while GONE_READS.load(Ordering::Relaxed) < 1000 && syscall::sys_ticks() - start < 100 {
            syscall::sleep_ticks(1);
        }
        let read = GONE_READS.load(Ordering::Relaxed) >= 1000;
        let unmapped = syscall::sys_munmap(GONE_AT, 1).is_ok();
        // By the time that call has returned, the page is gone from the
        // reader's processor too: it does not read it again. Looked at at
        // once, and without a call, because a processor that was not told
        // forgets by itself before long — the next time it changes what it
        // is running — and a check that waited would pass for that reason.
        let then = GONE_READS.load(Ordering::Relaxed);
        for _ in 0..400_000 {
            core::hint::spin_loop();
        }
        let stopped = GONE_READS.load(Ordering::Relaxed) - then <= 1;
        // Its next read found nothing there, and this task is asked.
        let mut fault = Message::empty();
        let asked = stopped
            && syscall::sys_recv_timeout(reader.tid(), &mut fault, 100).is_ok()
            && fault.tag == syscall::TAG_PAGE_FAULT
            && fault.data[0] as usize == GONE_AT;
        // Whatever happened, the reader ends: with a page under it again,
        // and an answer if it is waiting for one.
        GONE_STOP.store(true, Ordering::Relaxed);
        let _ = syscall::sys_mmap(GONE_AT, 1);
        if asked || syscall::sys_recv_timeout(reader.tid(), &mut fault, 20).is_ok() {
            let _ = syscall::sys_reply(reader.tid(), &Message::empty());
        }
        let _ = syscall::sys_wait_for(reader.tid());
        let _ = syscall::sys_munmap(GONE_AT, 1);
        Some(read && unmapped && asked)
    });
    check("and a page one thread unmaps is gone for a thread on another", gone == Some(true));

    check(
        "two threads that touch a new page at the same moment are both given it",
        run(b"dchild", &[b"touch"]) == Some(0),
    );
    check(
        "four threads adding to one number under a lock lose nothing",
        run(b"dchild", &[b"count"]) == Some(0),
    );

    // Two threads, one server, at once: the kernel is one processor's at a
    // time, and every call has to come out the other side its own.
    let echo = load_child(&[b"dchild", b"echo"]).and_then(|child| {
        let tid = child.tid;
        child.start().ok()?;
        mint_endpoint(CALLERS_SLOT, tid).then_some(tid)
    });
    let both = echo.and_then(|tid| {
        CALLERS_TO.store(tid, Ordering::Relaxed);
        CALLERS_STOP.store(false, Ordering::Relaxed);
        CALLERS_WRONG.store(false, Ordering::Relaxed);
        let second = thread::spawn_with_stack(second_caller, 8).ok()?;
        let start = syscall::sys_ticks();
        let mut made = 0u64;
        let mut right = true;
        let mut reply = Message::empty();
        while right && syscall::sys_ticks() - start < 100 {
            made += 1;
            let ask = Message { sender: 0, tag: made, data: [0; 6] };
            let outcome = syscall::sys_call_timeout(tid, &ask, &mut reply, 100);
            right = matches!(outcome, syscall::CallOutcome::Replied) && reply.tag == made + 1;
        }
        CALLERS_STOP.store(true, Ordering::Relaxed);
        let _ = syscall::sys_wait_for(second.tid());
        let theirs = CALLERS_MADE.load(Ordering::Relaxed);
        println!("        {} calls from one thread and {} from another, in a second", made, theirs);
        let stopped = matches!(
            syscall::sys_call_timeout(tid, &Message::empty(), &mut reply, 100),
            syscall::CallOutcome::Replied
        );
        if !stopped {
            let _ = syscall::sys_task_kill(tid);
        }
        let _ = wait_for(tid);
        Some(right && !CALLERS_WRONG.load(Ordering::Relaxed) && made >= 100 && theirs >= 100)
    });
    let _ = syscall::sys_cap_delete(CALLERS_SLOT);
    check("two threads calling one server at once are each answered, every time", both == Some(true));
}

/// A device that interrupts by sending a message, and the driver it takes:
/// what a driver is given to reach a device with. The driver is `edu`, for
/// the device of that name QEMU has; a distribution starts it, and on a
/// machine with no such device there is nothing here to check.
fn test_msi() {
    use quark_rt::ipc::Message;
    println!("a device's own interrupt:");
    let Some(edu) = nameserver::lookup_retry(b"edu", 2) else {
        println!("        no driver for a device that interrupts by message is running: not checked");
        return;
    };
    let ask2 = |tag: u64, with: u64, and: u64| {
        let mut reply = Message::empty();
        let msg = Message { sender: 0, tag, data: [with, and, 0, 0, 0, 0] };
        (syscall::sys_call(edu, &msg, &mut reply).is_ok() && reply.tag == 0).then_some(reply.data)
    };
    let ask = |tag: u64, with: u64| ask2(tag, with, 0);
    let who = ask(1, 0);
    check(
        "a driver with the right to device memory maps its device's registers, and they answer",
        matches!(who, Some(d) if d[0] & 0xFF == 0xED && d[1] == 1),
    );
    // That right reaches where devices are and nowhere else. The driver is
    // asked to try: memory, where the kernel is; a processor's interrupt
    // controller, which is among the devices' addresses and is nobody's to
    // map; and a range that begins among devices and ends in that.
    let may = |from: u64, to: u64| ask2(3, from, to).map(|d| d[0] == 1);
    check("which is not a right to memory: the kernel's is refused", may(0x10_0000, 0x10_1000) == Some(false));
    check(
        "nor to an interrupt controller's registers, nor to a range that runs into them",
        may(0xFEE0_0000, 0xFEE0_1000) == Some(false) && may(0xFEDF_F000, 0xFEE0_1000) == Some(false),
    );
    // And it is the driver's because it was given it. This program was not.
    const TRIAL_SLOT: usize = 49;
    let here = syscall::sys_cap_mint(TRIAL_SLOT, syscall::CAP_TYPE_PHYS_RANGE, 0xFEDF_0000, 0xFEDF_1000);
    let _ = syscall::sys_cap_delete(TRIAL_SLOT);
    check("a program without it is given no range at all", here.is_err());
    check(
        "and is given an interrupt that is the device's alone",
        matches!(who, Some(d) if d[2] >= 16 && d[3] == 1),
    );
    let first = ask(2, 0x0000_0001);
    check(
        "a message the device sends arrives as that interrupt",
        matches!((who, first), (Some(w), Some(d)) if d[0] == 1 && d[1] == 1 && d[2] == w[2]),
    );
    let again = (2..5u64).filter(|&n| matches!(ask(2, 1 << n), Some(d) if d[0] == 1 && d[1] == 1 << n)).count();
    check("and every time it sends one", again == 3);
}

/// Where a frame of ordinary memory is mapped to be written to and read.
const FRAME_AT: usize = 0xAD_0000_0000;

/// Which memory a frame comes from. Ordinary memory is given out from the
/// top of what the machine has, and memory a device will be told the
/// address of from below four gigabytes: a network card's registers are
/// thirty-two bits wide, and on a machine with more memory than that the
/// lowest frame that is free is not always one it can reach. On a machine
/// with less, the two ends are the two ends of the same four gigabytes, and
/// the checks are of that.
fn test_frames() {
    println!("frames:");
    const LINE: usize = 1 << 32;
    let (frames, end) = syscall::sys_mem_total();
    println!("        {} MiB of memory, the last of it at {:#x}", frames / 256, end * 4096);
    check("the kernel says how much memory there is, and where it ends", frames >= 16 * 256 && end >= frames);
    let (free_before, _) = syscall::sys_mem_info();

    let low = syscall::sys_phys_alloc_low(1);
    check("a frame for a device is below four gigabytes", matches!(low, Ok(at) if at % 4096 == 0 && at + 4096 <= LINE));
    let run = syscall::sys_phys_alloc_low(16);
    check("and so are sixteen in a row", matches!(run, Ok(at) if at % 4096 == 0 && at + 16 * 4096 <= LINE));

    let any = syscall::sys_phys_alloc(1);
    let above = end > LINE / 4096;
    check(
        "ordinary memory comes from the other end: above four gigabytes, where there is memory there",
        matches!((any, low), (Ok(any), Ok(low)) if any > low && (!above || any >= LINE)),
    );
    // And it is memory, wherever it is: what is written is what is read.
    let kept = any.is_ok_and(|frame| {
        if syscall::sys_map_phys(frame, FRAME_AT, 1).is_err() {
            return false;
        }
        let words = unsafe { core::slice::from_raw_parts_mut(FRAME_AT as *mut u64, 512) };
        for (i, word) in words.iter_mut().enumerate() {
            *word = 0x5EED_0000_0000_0000 | (frame as u64 ^ i as u64);
        }
        let kept = words.iter().enumerate().all(|(i, &word)| word == 0x5EED_0000_0000_0000 | (frame as u64 ^ i as u64));
        let _ = syscall::sys_munmap(FRAME_AT, 1);
        kept
    });
    check("and holds what is written to it", kept);

    if let Ok(at) = low {
        let _ = syscall::sys_phys_free(at, 1);
    }
    if let Ok(at) = run {
        let _ = syscall::sys_phys_free(at, 16);
    }
    if let Ok(at) = any {
        let _ = syscall::sys_phys_free(at, 1);
    }
    let (free_after, _) = syscall::sys_mem_info();
    // A page table for the mapping may have stayed.
    check("all of it goes back", free_after + 4 >= free_before);
}

/// Where the fork checks keep their pages: memory nothing else in this
/// program writes, so that a page stays shared for as long as a check needs
/// it to. Four megabytes, and then a page for each check that wants one.
const SHARED_AT: usize = 0xAE_0000_0000;
const SHARED_PAGES: usize = 1024;
const SHARED_HALF: usize = SHARED_PAGES / 2;
const fn own_page(n: usize) -> usize {
    SHARED_AT + (SHARED_PAGES + n) * 4096
}
const KERNEL_PAGE: usize = own_page(0);
const SERVER_PAGE: usize = own_page(1);
const LENT_PAGE: usize = own_page(2);
const READ_PAGE: usize = own_page(3);
const FUTEX_PAGE: usize = own_page(4);
const TID_PAGE: usize = own_page(5);
const SIGNAL_PAGE: usize = own_page(6);
const THREE_PAGE: usize = own_page(7);
const COUNT_PAGE: usize = own_page(8);
const OWN_PAGES: usize = 9;
/// A file mapped privately, for the last of them.
const PRIVATE_AT: usize = 0xAF_0000_0000;

/// What the four megabytes hold before the fork, what the parent writes to
/// its half afterwards, and what the child writes to its.
const WAS: u64 = 0x0A11_0000_0000_0000;
const PARENTS: u64 = 0x0B22_0000_0000_0000;
const CHILDS: u64 = 0x0C33_0000_0000_0000;
/// What is in the page that is given away, and what `dchild gift` knows to
/// look for.
const GIFTED: u64 = 0x6177_6179_2D74_6921;

/// Fork a child that waits to be told to go — a byte down descriptor 3 — and
/// then ends with 7 if `then` says so and 8 if it does not. Until it is told,
/// it shares every page this program had when it was made.
fn held_child(then: fn() -> bool) -> Option<usize> {
    match syscall::sys_fork() {
        Ok(0) => {
            let mut go = [0u8; 1];
            let told = syscall::sys_fd_read(3, &mut go) == 1;
            syscall::sys_exit_program(if told && then() { 7 } else { 8 });
        }
        Ok(child) => Some(child),
        Err(()) => None,
    }
}

/// Tell a held child to go, and say whether it ended with 7.
fn let_go(child: Option<usize>) -> bool {
    child.is_some_and(|c| syscall::sys_fd_write(4, b"g") == 1 && wait_for(c) == Some(7))
}

/// Whether the page at `at` is all `byte`.
fn page_is(at: usize, byte: u8) -> bool {
    (0..4096).all(|i| unsafe { core::ptr::read_volatile((at + i) as *const u8) } == byte)
}

fn fill_page(at: usize, byte: u8) {
    for i in 0..4096 {
        unsafe { core::ptr::write_volatile((at + i) as *mut u8, byte) };
    }
}

fn shared_word(i: usize) -> *mut u64 {
    (SHARED_AT + i * 4096) as *mut u64
}

/// The child of the first check: nothing its parent has written since the
/// fork is here, and what it writes itself is.
fn half_child() -> bool {
    let as_it_was = (0..SHARED_PAGES).all(|i| unsafe { shared_word(i).read_volatile() } == WAS ^ i as u64);
    for i in 0..SHARED_HALF {
        unsafe { shared_word(i).write_volatile(CHILDS ^ i as u64) };
    }
    as_it_was && (0..SHARED_HALF).all(|i| unsafe { shared_word(i).read_volatile() } == CHILDS ^ i as u64)
}

fn kernel_pages_child() -> bool {
    page_is(KERNEL_PAGE, b'a') && page_is(SERVER_PAGE, b'a')
}

fn lent_page_child() -> bool {
    page_is(LENT_PAGE, b'a')
}

fn read_page_child() -> bool {
    page_is(READ_PAGE, b'a')
}

fn nothing_to_do() -> bool {
    true
}

fn tid_page_child() -> bool {
    unsafe { core::ptr::read_volatile(TID_PAGE as *const u32) == 0x55 }
}

fn signal_page_child() -> bool {
    unsafe { core::ptr::read_volatile(SIGNAL_PAGE as *const u32) == 0 }
}

fn gift_child() -> bool {
    unsafe { core::ptr::read_volatile(GIFT as *const u64) == GIFTED }
}

static FORK_SERVER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static FORK_LEND_GO: sync::Semaphore = sync::Semaphore::new(0);
/// 1 once the lending call has been answered.
static FORK_LENT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// 1 if the read that was waiting was given what was written, 2 if not.
static FORK_READ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// 1 and what the wait answered, once it has.
static FORK_WOKEN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static FORK_TID_READY: sync::Semaphore = sync::Semaphore::new(0);
static FORK_TID_GO: sync::Semaphore = sync::Semaphore::new(0);
static FORK_WRITER_STOP: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Lends main a page, for writing, in a call main answers after it has
/// forked.
extern "C" fn fork_lender() -> ! {
    use quark_rt::ipc::Message;
    FORK_LEND_GO.acquire();
    let server = FORK_SERVER.load(core::sync::atomic::Ordering::SeqCst);
    let buf = unsafe { core::slice::from_raw_parts_mut(LENT_PAGE as *mut u8, 8) };
    let ask = Message { sender: 0, tag: 1, data: [0; 6] };
    let mut reply = Message::empty();
    if syscall::sys_call_lend_rw(server, &ask, &mut reply, buf).is_ok() {
        FORK_LENT.store(1, core::sync::atomic::Ordering::SeqCst);
    }
    syscall::sys_exit_code(0);
}

/// Reads descriptor 5 into a page, and is waiting there when main forks.
extern "C" fn fork_reader() -> ! {
    let buf = unsafe { core::slice::from_raw_parts_mut(READ_PAGE as *mut u8, 4) };
    let n = syscall::sys_fd_read(5, buf);
    let got = n == 4 && buf == b"late";
    FORK_READ.store(if got { 1 } else { 2 }, core::sync::atomic::Ordering::SeqCst);
    syscall::sys_exit_code(0);
}

/// Waits on a word for three seconds at most, and says how the wait ended.
extern "C" fn fork_waiter() -> ! {
    let r = syscall::sys_futex_wait_timeout(FUTEX_PAGE as *const u32, 0, syscall::ns(3_000_000_000));
    FORK_WOKEN.store(1 + r as u32, core::sync::atomic::Ordering::SeqCst);
    syscall::sys_exit_code(0);
}

/// Names a word for the kernel to clear when this thread ends, and ends
/// when it is told to.
extern "C" fn fork_leaver() -> ! {
    let _ = syscall::sys_set_clear_tid(TID_PAGE as *const u32);
    FORK_TID_READY.release();
    FORK_TID_GO.acquire();
    syscall::sys_exit_code(0);
}

/// Counts in a page for as long as it is let.
extern "C" fn fork_writer() -> ! {
    let count = COUNT_PAGE as *mut u64;
    while FORK_WRITER_STOP.load(core::sync::atomic::Ordering::SeqCst) == 0 {
        unsafe { count.write_volatile(count.read_volatile() + 1) };
    }
    syscall::sys_exit_code(0);
}

/// A fork shares what the program has and copies a page when one of the two
/// writes it. Nothing a program can see says that is what happened — which
/// is the point — so these are the places where it would show if it were
/// done wrong: who sees a write, what it costs, and every way into a page
/// that does not go through the page: the kernel writing one for a program,
/// a server writing one it was lent, a word a thread is waiting on, a word
/// the kernel clears or sets by itself, a page given away.
fn test_fork() {
    use core::sync::atomic::{AtomicU32, Ordering};
    use quark_rt::ipc::Message;
    println!("a fork shares, and a write copies:");
    let me = syscall::sys_getpid() as usize;
    if own_pipe(3, 4).is_err() || own_pipe(5, 6).is_err() {
        check("two pipes, to hold a child with and to read from", false);
        return;
    }
    if syscall::sys_map_anon(SHARED_AT, SHARED_PAGES + OWN_PAGES, false).is_err() {
        check("four megabytes to share", false);
        return;
    }
    for i in 0..SHARED_PAGES {
        unsafe { shared_word(i).write_volatile(WAS ^ i as u64) };
    }
    for n in 0..OWN_PAGES {
        fill_page(own_page(n), 0);
    }

    // What it costs, and who sees what. After one fork that is not counted:
    // the first task the kernel makes room for costs it room it then keeps.
    match syscall::sys_fork() {
        Ok(0) => syscall::sys_exit_program(0),
        Ok(child) => {
            let _ = wait_for(child);
        }
        Err(()) => {}
    }
    let (free0, charged0) = syscall::sys_mem_info();
    let child = held_child(half_child);
    let (free1, _) = syscall::sys_mem_info();
    let forked = free0.saturating_sub(free1);
    check(
        "a fork takes page tables, and not a page for a page",
        child.is_some() && forked < SHARED_PAGES / 4,
    );
    for i in SHARED_HALF..SHARED_PAGES {
        unsafe { shared_word(i).write_volatile(PARENTS ^ i as u64) };
    }
    let (free2, _) = syscall::sys_mem_info();
    let copied = free1.saturating_sub(free2);
    println!("        {} frames for the fork, {} for the {} pages then written", forked, copied, SHARED_HALF);
    check("a page is copied when it is written, and only then", (SHARED_HALF..SHARED_HALF + 16).contains(&copied));
    check("what a parent writes after a fork its child does not see", let_go(child));
    check(
        "nor the parent what the child writes",
        (0..SHARED_HALF).all(|i| unsafe { shared_word(i).read_volatile() } == WAS ^ i as u64)
            && (SHARED_HALF..SHARED_PAGES).all(|i| unsafe { shared_word(i).read_volatile() } == PARENTS ^ i as u64),
    );
    let (free3, charged3) = syscall::sys_mem_info();
    check("when the child has gone every frame is back", free3 + 16 >= free0);
    check("and the parent was charged for nothing it did not have before", charged3 == charged0);

    // The kernel writing a page for the program: what a read reads into.
    fill_page(KERNEL_PAGE, b'a');
    fill_page(SERVER_PAGE, b'a');
    let child = held_child(kernel_pages_child);
    let into = unsafe { core::slice::from_raw_parts_mut(KERNEL_PAGE as *mut u8, 4) };
    check(
        "the kernel writes a page shared since a fork as the program would",
        syscall::sys_fd_write(6, b"kern") == 4 && syscall::sys_fd_read(5, into) == 4 && into == b"kern",
    );
    // And a server writing one it is lent: the same, of a file.
    let into = unsafe { core::slice::from_raw_parts_mut(SERVER_PAGE as *mut u8, 4) };
    let served = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs_tid| {
        let o = vfs::open_with(vfs_tid, b"/dev/zero", 0).ok()?;
        let n = vfs::read(vfs_tid, o.handle, into, 0);
        let _ = vfs::close(vfs_tid, o.handle);
        n.ok()
    });
    check("and so does a server that is lent one", served == Some(4) && into == [0u8; 4]);
    check("into the program's own copy: its child has both pages as they were", let_go(child));

    // A page lent before the fork and written after it.
    fill_page(LENT_PAGE, b'a');
    FORK_SERVER.store(me, Ordering::SeqCst);
    match thread::spawn_with_stack(fork_lender, 8) {
        Ok(t) => {
            let t = t.tid();
            let granted = syscall::sys_cap_mint(syscall::SLOT_SCRATCH, syscall::CAP_TYPE_ENDPOINT, me as u64, 0).is_ok()
                && syscall::sys_cap_grant_any(t, syscall::SLOT_SCRATCH).is_ok();
            let _ = syscall::sys_cap_delete(syscall::SLOT_SCRATCH);
            FORK_LEND_GO.release();
            let mut msg = Message::empty();
            let arrived = granted && syscall::sys_recv(t, &mut msg).is_ok() && msg.tag == 1;
            let child = held_child(lent_page_child);
            check(
                "what was lent before a fork can be written after it",
                arrived && syscall::sys_lent_write(t, 0, b"serv") == Ok(4),
            );
            let _ = syscall::sys_reply(t, &Message::empty());
            let _ = wait_for(t);
            check(
                "and it is the lender's page that is written",
                FORK_LENT.load(Ordering::SeqCst) == 1
                    && unsafe { core::slice::from_raw_parts(LENT_PAGE as *const u8, 4) } == b"serv",
            );
            check("not its child's", let_go(child));
        }
        Err(()) => check("start a thread to lend a page", false),
    }

    // A read that was waiting when the program forked: the kernel checked
    // the page before it waited, and writes it after.
    fill_page(READ_PAGE, b'a');
    match thread::spawn_with_stack(fork_reader, 8) {
        Ok(t) => {
            syscall::sleep_ticks(5);
            let child = held_child(read_page_child);
            let wrote = syscall::sys_fd_write(6, b"late") == 4;
            let _ = wait_for(t.tid());
            check(
                "a read that was waiting when its program forked is answered into the program's own page",
                wrote
                    && FORK_READ.load(Ordering::SeqCst) == 1
                    && unsafe { core::slice::from_raw_parts(READ_PAGE as *const u8, 4) } == b"late",
            );
            check("and the child's is as it was", let_go(child));
        }
        Err(()) => check("start a thread to read", false),
    }

    // A word a thread is waiting on. It is the program's word, wherever the
    // page is: a child's wake is for the child's, and the program's own
    // wake finds the thread after the page has been copied to another frame.
    let word = FUTEX_PAGE as *mut u32;
    match thread::spawn_with_stack(fork_waiter, 8) {
        Ok(t) => {
            syscall::sleep_ticks(5);
            let stray = match syscall::sys_fork() {
                Ok(0) => {
                    let woke = syscall::sys_futex_wake(word, 1);
                    syscall::sys_exit_program(if woke == 0 { 7 } else { 8 });
                }
                Ok(child) => wait_for(child) == Some(7),
                Err(()) => false,
            };
            syscall::sleep_ticks(2);
            check(
                "a child's wake does not reach a thread of its parent's",
                stray && FORK_WOKEN.load(Ordering::SeqCst) == 0,
            );
            let child = held_child(nothing_to_do);
            unsafe { word.write_volatile(1) };
            let woke = syscall::sys_futex_wake(word, 1);
            let _ = wait_for(t.tid());
            check(
                "a wake finds a thread that waited before the page was copied",
                woke == 1 && FORK_WOKEN.load(Ordering::SeqCst) == 1,
            );
            let _ = let_go(child);
        }
        Err(()) => check("start a thread to wait", false),
    }

    // A word the kernel clears by itself, when a thread ends.
    let word = TID_PAGE as *mut u32;
    unsafe { word.write_volatile(0x55) };
    match thread::spawn_with_stack(fork_leaver, 8) {
        Ok(_) => {
            FORK_TID_READY.acquire();
            let child = held_child(tid_page_child);
            FORK_TID_GO.release();
            let mut cleared = false;
            for _ in 0..30 {
                if unsafe { word.read_volatile() } == 0 {
                    cleared = true;
                    break;
                }
                let _ = syscall::sys_futex_wait_timeout(word, 0x55, syscall::ns(100_000_000));
            }
            check("a thread that ends after its program forked has its word cleared", cleared);
            check("in its program, and not in the child", let_go(child));
        }
        Err(()) => check("start a thread to end", false),
    }

    // And one it sets by itself, when a signal is raised.
    const USR1: u64 = 10;
    let told: &'static AtomicU32 = unsafe { &*(SIGNAL_PAGE as *const AtomicU32) };
    let _ = syscall::sys_sig_action(USR1, syscall::SIG_HANDLE);
    let _ = syscall::sys_sig_take(Some(told));
    let child = held_child(signal_page_child);
    let raised = syscall::sys_sig_raise(me, USR1).is_ok();
    check(
        "a program is told of a signal through a page shared since a fork",
        raised && told.load(Ordering::SeqCst) == 1,
    );
    check(
        "and its child is told nothing",
        syscall::sys_sig_take(None) == 1 << (USR1 - 1) && let_go(child),
    );
    let _ = syscall::sys_sig_action(USR1, syscall::SIG_DEFAULT);

    // A page given away is the giver's alone before it goes.
    let made = syscall::sys_mmap(GIFT, 1).is_ok();
    if made {
        unsafe { core::ptr::write_volatile(GIFT as *mut u64, GIFTED) };
    }
    let taker = load_child(&[b"dchild", b"gift"]);
    let child = held_child(gift_child);
    let given = made
        && taker
            .as_ref()
            .is_some_and(|t| syscall::sys_addrspace_give(t.cr3, CHILD_SPARE, GIFT, 1, 1).is_ok());
    check("a page shared since a fork can be given away", given && nothing_at(GIFT));
    let ran = taker.is_some_and(|t| {
        let tid = t.tid;
        t.start().is_ok() && wait_for(tid) == Some(0)
    });
    check("whoever is given it finds what was in it, and writes it", ran);
    check("and the child that shared it has its own, as it was", let_go(child));

    // Three programs with one page.
    let three = THREE_PAGE as *mut u64;
    unsafe { three.write_volatile(1) };
    let each = match syscall::sys_fork() {
        Ok(0) => {
            let below = match syscall::sys_fork() {
                Ok(0) => {
                    unsafe { three.write_volatile(3) };
                    syscall::sys_exit_program(if unsafe { three.read_volatile() } == 3 { 7 } else { 8 });
                }
                Ok(grandchild) => wait_for(grandchild) == Some(7),
                Err(()) => false,
            };
            let mine = unsafe { three.read_volatile() } == 1;
            unsafe { three.write_volatile(2) };
            let wrote = unsafe { three.read_volatile() } == 2;
            syscall::sys_exit_program(if below && mine && wrote { 7 } else { 8 });
        }
        Ok(child) => wait_for(child) == Some(7) && unsafe { three.read_volatile() } == 1,
        Err(()) => false,
    };
    unsafe { three.write_volatile(9) };
    check(
        "a page three programs share is each one's own when it writes it",
        each && unsafe { three.read_volatile() } == 9,
    );

    // A thread that goes on writing a page while the program forks. Only the
    // task that forked is in the child, so nothing there writes the page:
    // if it changes, it is the parent's thread writing it, through what its
    // processor still remembered.
    let count = COUNT_PAGE as *mut u64;
    match thread::spawn_with_stack(fork_writer, 8) {
        Ok(t) => {
            const ROUNDS: usize = 40;
            let mut still = 0;
            for _ in 0..ROUNDS {
                match syscall::sys_fork() {
                    Ok(0) => {
                        let first = unsafe { count.read_volatile() };
                        for _ in 0..200_000 {
                            core::hint::spin_loop();
                        }
                        let then = unsafe { count.read_volatile() };
                        syscall::sys_exit_program(if first == then { 7 } else { 8 });
                    }
                    Ok(child) => {
                        if wait_for(child) == Some(7) {
                            still += 1;
                        }
                    }
                    Err(()) => {}
                }
            }
            let before = unsafe { count.read_volatile() };
            syscall::sleep_ticks(3);
            let after = unsafe { count.read_volatile() };
            FORK_WRITER_STOP.store(1, Ordering::SeqCst);
            let _ = wait_for(t.tid());
            check(
                "a thread still writing while its program forks writes nothing of the child's",
                still == ROUNDS,
            );
            check("and goes on writing its own", after > before);
        }
        Err(()) => check("start a thread to write", false),
    }

    // A file mapped privately: a page of it that has been touched is the
    // program's own copy, which still names the file. Children that had
    // such pages, and have gone, took nothing of the file's with them.
    const PRIVATE: &[u8] = b"/tmp/dtest-private";
    let kept = nameserver::lookup_retry(b"vfs", 20).and_then(|vfs_tid| {
        let o = vfs::open_with(vfs_tid, PRIVATE, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE).ok()?;
        for i in 0..4u8 {
            vfs::write(vfs_tid, o.handle, &[b'0' + i; 4096], i as u32 * 4096).ok()?;
        }
        let (slot, _) = vfs::map(vfs_tid, o.handle, false).ok()?;
        let made = syscall::sys_object_map(slot, PRIVATE_AT, 4, 0, syscall::OBJECT_MAP_WRITE);
        let _ = syscall::sys_cap_delete(slot);
        let _ = vfs::close(vfs_tid, o.handle);
        made.ok()?;
        let touched = page_is(PRIVATE_AT, b'0') && page_is(PRIVATE_AT + 4096, b'1');
        let mut gone = 0;
        for _ in 0..4 {
            match syscall::sys_fork() {
                Ok(0) => syscall::sys_exit_program(7),
                Ok(child) => {
                    if wait_for(child) == Some(7) {
                        gone += 1;
                    }
                }
                Err(()) => {}
            }
        }
        // Long enough for the file's server to be told nothing maps the
        // file, if the kernel thinks so, and to let it go.
        syscall::sleep_ticks(5);
        // In a child of its own, so that a file that is no longer there
        // ends the child and not this.
        let rest = match syscall::sys_fork() {
            Ok(0) => {
                let there = page_is(PRIVATE_AT + 2 * 4096, b'2') && page_is(PRIVATE_AT + 3 * 4096, b'3');
                syscall::sys_exit_program(if there { 7 } else { 8 });
            }
            Ok(child) => wait_for(child) == Some(7),
            Err(()) => false,
        };
        let _ = syscall::sys_munmap(PRIVATE_AT, 4);
        let _ = vfs::unlink(vfs_tid, PRIVATE);
        Some(touched && gone == 4 && rest)
    });
    check(
        "a file mapped privately is still there when children that had copies of its pages have gone",
        kept == Some(true),
    );

    for chunk in (0..SHARED_PAGES + OWN_PAGES).step_by(256) {
        let _ = syscall::sys_munmap(SHARED_AT + chunk * 4096, 256.min(SHARED_PAGES + OWN_PAGES - chunk));
    }
    for fd in 3..7 {
        let _ = syscall::sys_fd_close(fd);
    }
}

/// Turning the machine off is for whoever holds the right to, and this
/// program does not: it asks for none in its manifest, and neither does the
/// one it starts to try. That the checks are reached at all is most of what
/// they say.
fn test_power() {
    println!("power:");
    let tried = run(b"dchild", &[b"power"]);
    check("a program that does not hold the right cannot turn the machine off, or start it again", tried.is_some());
    check("and cannot make itself the right to", tried == Some(0));
}

/// The latest time any thread of this program has been told, how many times
/// one was told an earlier time than that, and how many threads have
/// finished asking.
static CLOCK_SEEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CLOCK_WENT_BACK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static CLOCK_ASKED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Ask the time for a tenth of a second, as fast as it can be asked, and
/// hold each answer to the latest anybody had before the question was put.
fn ask_the_time() {
    use core::sync::atomic::Ordering;
    let until = syscall::sys_clock() + 100_000_000;
    loop {
        let seen = CLOCK_SEEN.load(Ordering::SeqCst);
        let now = syscall::sys_clock();
        if now < seen {
            CLOCK_WENT_BACK.fetch_add(1, Ordering::SeqCst);
        }
        CLOCK_SEEN.fetch_max(now, Ordering::SeqCst);
        if now >= until {
            break;
        }
    }
    CLOCK_ASKED.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn clock_asker() -> ! {
    ask_the_time();
    syscall::sys_exit_code(0);
}

/// How long `wait` takes, in nanoseconds, each of `tries` times: the
/// shortest, and how many of them were over in less than `soon`.
///
/// The shortest holds a wait to what it must never do, end early. How many
/// were soon holds it to ending on time: most of them, not all, because a
/// machine with other work to do is sometimes late; and not one, because a
/// wait that ends on the next tick ends soon once in a while by where in a
/// tick it happened to begin.
fn timed(tries: usize, soon: u64, wait: impl Fn()) -> (u64, usize) {
    let mut shortest = u64::MAX;
    let mut quick = 0;
    for _ in 0..tries {
        let from = syscall::sys_clock();
        wait();
        let took = syscall::sys_clock() - from;
        shortest = shortest.min(took);
        if took < soon {
            quick += 1;
        }
    }
    (shortest, quick)
}

/// Time: what the clock says, how finely, and whether a wait ends when it
/// was asked to. The clock is the processor's counter where the kernel can
/// keep time by it, and the count of ticks where it cannot; what is asked of
/// the first is not asked of the second.
fn test_clock() {
    use core::sync::atomic::Ordering;
    use syscall::{ns, TICK_NS};
    const MS: u64 = 1_000_000;
    println!("the clock:");
    let first = syscall::sys_clock();
    check("the clock goes forward", first > 0 && syscall::sys_clock() >= first);
    let (ticks, clock) = (syscall::sys_ticks(), syscall::sys_clock() / TICK_NS);
    check("and a tick is ten milliseconds of it", clock == ticks || clock == ticks + 1);

    // How finely it moves: the smallest step between two readings, over
    // thirty milliseconds of reading it.
    let mut step = u64::MAX;
    let mut last = syscall::sys_clock();
    let until = last + 30 * MS;
    while last < until {
        let now = syscall::sys_clock();
        if now != last {
            step = step.min(now - last);
        }
        last = now;
    }
    let fine = step < TICK_NS;
    if fine {
        println!("        it moves in steps of {} ns or less", step);
    } else {
        println!("        this machine's clock is the tick: what a finer one does is not checked");
    }

    let wall = syscall::sys_clock_wall();
    let started = syscall::sys_boot_time();
    check(
        "the date is when the machine was started and the time since",
        (wall == 0 && started == 0) || wall.abs_diff(started * 1_000_000_000 + syscall::sys_clock()) < 2_000 * MS,
    );

    // A wait is never short, whatever it is a wait for; and where the clock
    // is fine it is not rounded up to a tick either.
    let (least, soon) = timed(20, 5 * MS, || syscall::sleep_ns(MS));
    check("a sleep of a millisecond is a millisecond at least", least >= MS);
    if fine {
        check("and not a tick: it ends when the millisecond does", soon >= 15);
    }
    let (least, soon) = timed(3, 3 * TICK_NS, || syscall::sleep_ticks(2));
    check("a sleep of two ticks is twenty milliseconds", least >= 2 * TICK_NS && soon >= 1);

    static WORD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    let word = &WORD as *const _ as *const u32;
    let gave_up = core::cell::Cell::new(true);
    let (least, soon) = timed(10, 8 * MS, || {
        if syscall::sys_futex_wait_timeout(word, 0, ns(3 * MS)) != syscall::FUTEX_TIMED_OUT {
            gave_up.set(false);
        }
    });
    check("a wait on a word gives up when its time is up, and no sooner", gave_up.get() && least >= 3 * MS);
    if fine {
        check("to the millisecond", soon >= 7);
    }
    let nothing = core::cell::Cell::new(true);
    let (least, soon) = timed(10, 7 * MS, || {
        if syscall::sys_poll(&mut [], ns(2 * MS)) != Ok(0) {
            nothing.set(false);
        }
    });
    check("a poll of nothing is a sleep of that long", nothing.get() && least >= 2 * MS && (!fine || soon >= 7));

    // A timer, which is a descriptor: read as how many times it has fired.
    let fired = |fd: usize| {
        let mut count = [0u8; 8];
        (syscall::sys_fd_read(fd, &mut count) == 8).then(|| u64::from_le_bytes(count))
    };
    match syscall::sys_timer_create() {
        Ok(fd) => {
            let from = syscall::sys_clock();
            let once = syscall::sys_timer_set(fd, ns(3 * MS), 0).is_ok() && fired(fd) == Some(1);
            let took = syscall::sys_clock() - from;
            check("a timer set in nanoseconds fires then", once && took >= 3 * MS && (!fine || took < 50 * MS));

            // Every millisecond, left alone for forty of them. What a read
            // says is how many milliseconds had gone by when it was made:
            // no fewer than had since the timer was known to be set, by
            // the time just before the read, and no more than had since
            // just before it was set, by the time just after.
            let before_set = syscall::sys_clock();
            let set = syscall::sys_timer_set(fd, ns(MS), ns(MS)).is_ok();
            let after_set = syscall::sys_clock();
            syscall::sleep_ns(40 * MS);
            let before_read = syscall::sys_clock();
            let count = fired(fd).unwrap_or(0);
            let after_read = syscall::sys_clock();
            check(
                "one that repeats counts every time it would have fired",
                set && count >= (before_read - after_set) / MS && count <= (after_read - before_set) / MS,
            );

            // And every nanosecond, which no machine can do and any program
            // may ask for. The kernel looks at it as often as it will look
            // at anything — not a thousand million times a second — and the
            // count is still the nanoseconds that went by; that this line
            // is reached at all is the machine having gone on running.
            let before_set = syscall::sys_clock();
            let set = syscall::sys_timer_set(fd, ns(1), ns(1)).is_ok();
            let after_set = syscall::sys_clock();
            syscall::sleep_ns(50 * MS);
            let before_read = syscall::sys_clock();
            let count = fired(fd).unwrap_or(0);
            let after_read = syscall::sys_clock();
            check(
                "one that repeats every nanosecond counts them, and stops nothing",
                set && count >= before_read - after_set && count <= after_read - before_set,
            );

            let set = syscall::sys_timer_set(fd, ns(500 * MS), 0).is_ok();
            let left = syscall::sys_timer_get(fd);
            let in_ticks = syscall::sys_timer_get_ticks(fd);
            check(
                "what is left of a timer is said in nanoseconds, and in ticks rounded up",
                set && matches!(left, Some((left, 0)) if left > 400 * MS && left <= 500 * MS)
                    && matches!(in_ticks, Some((left, 0)) if left > 40 && left <= 50),
            );
            check("and a timer turned off has nothing left", {
                syscall::sys_timer_set(fd, 0, 0).is_ok() && syscall::sys_timer_get(fd) == Some((0, 0))
            });
            let _ = syscall::sys_fd_close(fd);
        }
        Err(()) => check("make a timer", false),
    }

    // The alarm, set and taken back before it is due: this program has said
    // nothing about that signal.
    let none = syscall::sys_sig_alarm_ns(2_000 * MS, 0);
    let was = syscall::sys_sig_alarm_ns(0, 0);
    check(
        "an alarm says how long it has, to the nanosecond",
        none == (0, 0) && was.0 > 1_900 * MS && was.0 <= 2_000 * MS && was.1 == 0,
    );
    let _ = syscall::sys_sig_alarm(200, 0);
    let was = syscall::sys_sig_alarm(0, 0);
    check("and in ticks, rounded up", was == (200, 0) && syscall::sys_sig_alarm_left() == (0, 0));

    // The date is the machine's, and who may set it holds the right to.
    check("a program that does not hold the right may not set the clock", run(b"dchild", &[b"clockset"]) == Some(0));
    let me = syscall::sys_getpid() as usize;
    let may = (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(me, slot), Ok(c) if c.valid && c.cap_type == syscall::CAP_TYPE_CLOCK)
    });
    if may && wall != 0 {
        const AHEAD: u64 = 100_000 * MS;
        let (date, since_boot) = (syscall::sys_clock_wall(), syscall::sys_clock());
        let set = syscall::sys_clock_set(date + AHEAD).is_ok();
        let after = syscall::sys_clock_wall();
        check("one that does sets the date, and time goes on from there", set && after >= date + AHEAD && after < date + AHEAD + 1_000 * MS);
        let (least, soon) = timed(3, 1_000 * MS, || syscall::sleep_ns(5 * MS));
        check(
            "and no wait is moved by it: they are by the time since the machine started",
            syscall::sys_clock() - since_boot < 4_000 * MS && least >= 5 * MS && soon == 3,
        );
        check(
            "a date the clock cannot keep is refused",
            syscall::sys_clock_set(0).is_err() && syscall::sys_clock_set(7_300_000_000 * 1_000_000_000).is_err(),
        );
        // Back to what it was, and the time this took.
        let back = syscall::sys_clock_set(date + (syscall::sys_clock() - since_boot)).is_ok();
        check("and it is set back", back && syscall::sys_clock_wall().abs_diff(date) < 3_000 * MS);
    } else {
        println!("        this session may not set the clock, or the machine has none: setting it is not checked");
    }

    // Asked on every processor at once: no answer is earlier than one
    // already given.
    CLOCK_SEEN.store(0, Ordering::SeqCst);
    CLOCK_WENT_BACK.store(0, Ordering::SeqCst);
    CLOCK_ASKED.store(0, Ordering::SeqCst);
    let (processors, _) = syscall::sys_cpus();
    let mut askers = 0;
    for _ in 1..processors.min(4) {
        if thread::spawn_with_stack(clock_asker, 8).is_ok() {
            askers += 1;
        }
    }
    ask_the_time();
    for _ in 0..askers {
        let _ = syscall::sys_wait();
    }
    check(
        "the clock does not go back, whichever processor is asked",
        CLOCK_ASKED.load(Ordering::SeqCst) == askers + 1 && CLOCK_WENT_BACK.load(Ordering::SeqCst) == 0,
    );
}

/// The word a thread asks to have cleared when it ends, as every thread a C
/// library makes does: 1 while the thread is there.
static JOIN_WORD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static JOIN_READY: sync::Semaphore = sync::Semaphore::new(0);
static JOIN_GO: sync::Semaphore = sync::Semaphore::new(0);

/// Gives the kernel its word, says so, and ends when it is told to.
extern "C" fn joined_by_word() -> ! {
    let _ = syscall::sys_set_clear_tid(JOIN_WORD.as_ptr());
    JOIN_READY.release();
    JOIN_GO.acquire();
    syscall::sys_exit_code(7);
}

/// Gives the kernel its word and ends.
extern "C" fn joined_at_once() -> ! {
    let _ = syscall::sys_set_clear_tid(JOIN_WORD.as_ptr());
    syscall::sys_exit_code(0);
}

extern "C" fn leaves_with_five() -> ! {
    syscall::sys_exit_code(5);
}

/// Gives the kernel its word a while after it has started — by which time
/// its creator is waiting for it — and ends a while after that.
extern "C" fn word_given_late() -> ! {
    syscall::sleep_ticks(10);
    let _ = syscall::sys_set_clear_tid(JOIN_WORD.as_ptr());
    syscall::sleep_ticks(10);
    syscall::sys_exit_code(0);
}

/// Join a thread the way a C library does: wait on its word until the
/// kernel has cleared it. `false` if two seconds pass and it has not.
fn word_cleared() -> bool {
    use core::sync::atomic::Ordering;
    let start = syscall::sys_ticks();
    while JOIN_WORD.load(Ordering::SeqCst) != 0 {
        if syscall::sys_ticks() - start > 200 {
            return false;
        }
        let _ = syscall::sys_futex_wait_timeout(JOIN_WORD.as_ptr(), 1, 50);
    }
    true
}

/// A thread is joined one of two ways, and the thread says which: waited
/// for, as any child is, or through a word the kernel clears when it ends.
/// The second is a C library's, whose threads are not its children.
fn test_threads() {
    use core::sync::atomic::Ordering;
    println!("a thread is joined one of two ways:");

    let waited = thread::spawn_with_stack(leaves_with_five, 2).ok().map(|t| (t.tid(), syscall::sys_wait_for(t.tid())));
    check(
        "one that gives no word is a child, and a wait answers with what it ended with",
        matches!(waited, Some((tid, Ok((got, 5)))) if got == tid),
    );

    JOIN_WORD.store(1, Ordering::SeqCst);
    let Ok(t) = thread::spawn_with_stack(joined_by_word, 2) else {
        check("started a thread", false);
        return;
    };
    let tid = t.tid();
    JOIN_READY.acquire();
    check(
        "one that gives a word to clear is nobody's child: there is no waiting for it",
        syscall::sys_wait_nowait(tid) == Err(()),
    );
    JOIN_GO.release();
    check("its word is cleared when it ends, and whoever waits on the word is woken", word_cleared());

    // Nobody collects such a thread, so the kernel does, and what it had —
    // its place among the system's tasks, most of all, of which there are
    // sixty-four — comes back. It did not: an ended thread stayed until its
    // program did, and a C program that made threads one after another
    // came to a point where nothing in the system could make a task.
    let mut made = 0;
    for _ in 0..80 {
        JOIN_WORD.store(1, Ordering::SeqCst);
        if thread::spawn_with_stack(joined_at_once, 2).is_err() || !word_cleared() {
            break;
        }
        made += 1;
    }
    println!("        {} threads made, one after another", made);
    check("eighty of them, one after another, each give their place back", made == 80);

    // A thread may give its own word, after it has started: its creator may
    // by then be waiting for it as a child. The wait is told there is no
    // such child, there and then — it used to have nothing to wake it, ever.
    JOIN_WORD.store(1, Ordering::SeqCst);
    let late = thread::spawn_with_stack(word_given_late, 2).ok().map(|t| {
        let before = syscall::sys_ticks();
        let answer = syscall::sys_wait_for(t.tid());
        (answer, syscall::sys_ticks() - before)
    });
    check(
        "a creator waiting for a thread when it gives its word is told it has no such child",
        matches!(late, Some((Err(()), waited)) if (5..18).contains(&waited)),
    );
    check("and joins it through the word like any other", word_cleared());
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
        ("identity", test_identity),
        ("passwords", test_passwords),
        ("discard", test_discard),
        ("gone", test_callee_gone),
        ("users", test_users),
        ("auth", test_auth),
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
        ("diskfiles", test_disk_files),
        ("mounts", test_mounts),
        ("files", test_files),
        ("fifo", test_named_pipes),
        ("sync", test_sync),
        ("fpu", test_fpu),
        ("flags", test_flags),
        ("wire", test_wire),
        ("threads", test_threads),
        ("msi", test_msi),
        ("clock", test_clock),
        ("power", test_power),
        ("frames", test_frames),
        ("fork", test_fork),
        ("smp", test_smp),
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
