/// Syscall wrappers for the Quark microkernel.
///
/// Convention: RAX=nr, RDI=arg0, RSI=arg1, RDX=arg2, R10=arg3, R8=arg4, R9=arg5.
/// Return value in RAX.

use core::arch::asm;

// Syscall numbers
// --- 0x00  process lifecycle ---
pub const SYS_EXIT: u64 = 0;
pub const SYS_EXIT_CODE: u64 = 1;
pub const SYS_YIELD: u64 = 2;
pub const SYS_GETPID: u64 = 3;
pub const SYS_WAIT: u64 = 4;
pub const SYS_TASK_KILL: u64 = 5;
pub const SYS_SIGNAL: u64 = 6;
pub const SYS_TASK_INFO: u64 = 7;
/// End every task of the caller's program. `SYS_EXIT_CODE` ends one.
pub const SYS_EXIT_PROGRAM: u64 = 8;
/// The permission bits this program leaves off what it makes.
pub const SYS_UMASK: u64 = 9;
/// `SYS_WAIT` for one child, or without waiting.
pub const SYS_WAIT_FOR: u64 = 10;
/// What this program does about a signal.
pub const SYS_SIG_ACTION: u64 = 11;
/// Raise a signal for the program a task belongs to.
pub const SYS_SIG_RAISE: u64 = 12;
/// The signals raised for this program that it handles.
pub const SYS_SIG_TAKE: u64 = 13;
/// The process id of the program a task belongs to: never used twice.
pub const SYS_PID: u64 = 14;
/// Have SIGALRM raised for this program after a time.
pub const SYS_SIG_ALARM: u64 = 15;

// --- 0x10  IPC ---
pub const SYS_SEND: u64 = 16;
pub const SYS_RECV: u64 = 17;
pub const SYS_CALL: u64 = 18;
pub const SYS_REPLY: u64 = 19;
pub const SYS_CALL_TIMEOUT: u64 = 20;
pub const SYS_RECV_TIMEOUT: u64 = 21;
pub const SYS_NOTIFY: u64 = 22;
pub const SYS_CALL_LEND: u64 = 23;
pub const SYS_CALL_OFFER: u64 = 24;
pub const SYS_CALL_WITH: u64 = 27;
pub const SYS_LENT_READ: u64 = 25;
pub const SYS_LENT_WRITE: u64 = 26;

// --- 0x20  memory ---
pub const SYS_MMAP: u64 = 32;
pub const SYS_MUNMAP: u64 = 33;
pub const SYS_PHYS_ALLOC: u64 = 34;
pub const SYS_PHYS_FREE: u64 = 35;
pub const SYS_ADDRSPACE_CREATE: u64 = 36;
pub const SYS_ADDRSPACE_DESTROY: u64 = 44;
pub const SYS_ADDRSPACE_MAP: u64 = 37;
pub const SYS_MAP_PHYS: u64 = 38;
pub const SYS_SET_MEM_LIMIT: u64 = 39;
pub const SYS_SET_PAGER: u64 = 40;
pub const SYS_ADDRSPACE_SELF: u64 = 41;
pub const SYS_ADDRSPACE_GIVE: u64 = 43;

// --- 0x30  shared memory ---
pub const SYS_MMAP_FD: u64 = 42;
pub const SYS_SHMEM_CREATE: u64 = 48;
pub const SYS_MEMFD_CREATE: u64 = 53;
pub const SYS_MEMFD_TRUNCATE: u64 = 54;
pub const SYS_SHMEM_MAP: u64 = 49;
pub const SYS_SHMEM_UNMAP: u64 = 50;
pub const SYS_SHMEM_GRANT: u64 = 51;
pub const SYS_SHMEM_DESTROY: u64 = 52;

// --- 0x40  file descriptors and pipes ---
pub const SYS_FD_READ: u64 = 64;
pub const SYS_FD_WRITE: u64 = 65;
pub const SYS_FD_READ_NB: u64 = 66;
pub const SYS_FD_SET: u64 = 67;
pub const SYS_FD_DUP: u64 = 68;
pub const SYS_PIPE_CREATE: u64 = 69;
pub const SYS_PIPE_FD_SET: u64 = 70;
pub const SYS_FD_CLOSE: u64 = 71;
pub const SYS_SOCKETPAIR: u64 = 72;
pub const SYS_FD_SEND: u64 = 73;
pub const SYS_FD_RECV: u64 = 74;
pub const SYS_POLLSET_CREATE: u64 = 75;
pub const SYS_POLLSET_CTL: u64 = 76;
pub const SYS_POLLSET_WAIT: u64 = 77;
pub const SYS_POLL: u64 = 78;
pub const SYS_FD_WRITE_NB: u64 = 79;

pub const POLL_READABLE: u32 = 1;
pub const POLL_WRITABLE: u32 = 2;
pub const POLL_HANGUP: u32 = 4;
/// A descriptor that cannot be waited on. Reported by [`sys_poll`] in
/// `revents`; [`sys_pollset_add`] refuses such a descriptor outright instead.
pub const POLL_INVALID: u32 = 8;
/// Asked for beside readable, said beside a hangup: the other end has gone.
pub const POLL_PEER_GONE: u32 = 0x10;
/// In a set's watch: reported when what it watches has been noted since it
/// was last looked at, and is ready then (epoll's EPOLLET).
pub const POLL_EDGE: u32 = 1 << 16;
/// In a set's watch: reported once, and then not until it is modified.
pub const POLL_ONCE: u32 = 1 << 17;

// --- 0x50  capabilities ---
pub const SYS_CAP_MINT: u64 = 80;
pub const SYS_CAP_GRANT: u64 = 81;
pub const SYS_CAP_REVOKE: u64 = 82;
pub const SYS_CAP_INSPECT: u64 = 83;
pub const SYS_CAP_DELETE: u64 = 84;
pub const SYS_CAP_TRANSFER: u64 = 85;
pub const SYS_GRANT_CAP: u64 = 86;
pub const SYS_GRANT_IOPORT: u64 = 87;
pub const SYS_GRANT_IRQ: u64 = 88;
pub const SYS_SET_USER_CAPS: u64 = 89;
pub const SYS_GET_USER_CAPS: u64 = 90;
pub const SYS_CAP_TAKE: u64 = 91;
pub const SYS_CAP_READ: u64 = 92;

// --- 0x60  task lifecycle and identity ---
pub const SYS_TASK_CREATE: u64 = 96;
pub const SYS_TASK_START: u64 = 97;
pub const SYS_GET_UID: u64 = 98;
pub const SYS_SET_UID: u64 = 99;
pub const SYS_SET_GID: u64 = 100;
pub const SYS_GET_TUID: u64 = 101;
pub const SYS_SET_FS_BASE: u64 = 102;
pub const SYS_TASK_START_ARG: u64 = 103;
pub const SYS_TASK_WATCH: u64 = 104;
pub const SYS_TASK_PRIORITY: u64 = 105;
pub const SYS_SET_CLEAR_TID: u64 = 106;
pub const SYS_TASK_SPACE: u64 = 107;
pub const SYS_SPACE_WATCH: u64 = 108;
pub const SYS_TASK_CREATE_IN: u64 = 109;
pub const SYS_FORK: u64 = 110;
pub const SYS_EXEC_SPACE: u64 = 111;
pub const SYS_PTY_CREATE: u64 = 208;
pub const SYS_PTY_CTL: u64 = 209;
pub const SYS_PTY_OPEN: u64 = 210;
/// Process groups and sessions.
pub const SYS_PGROUP: u64 = 211;
/// The groups a task is in besides its own, and saying who a task is.
pub const SYS_GROUPS: u64 = 212;
pub const SYS_IDENTIFY: u64 = 213;
pub const SYS_TIMER_CREATE: u64 = 146;
pub const SYS_TIMER_SET: u64 = 147;
pub const SYS_TIMER_GET: u64 = 148;

// --- 0x70  hardware and drivers ---
pub const SYS_IRQ_REGISTER: u64 = 112;
pub const SYS_IRQ_ACK: u64 = 113;
pub const SYS_IOPORT: u64 = 114;
pub const SYS_IOPORT_REP: u64 = 115;
pub const SYS_GETRANDOM: u64 = 116;
pub const SYS_CPUS: u64 = 117;
pub const SYS_MSI_ALLOC: u64 = 118;
pub const SYS_POWER: u64 = 119;
pub const SYS_SIG_MASK: u64 = 120;
pub const SYS_SIG_RETURN: u64 = 121;
pub const SYS_SIG_STACK: u64 = 122;
pub const SYS_SIG_WAIT: u64 = 123;
pub const SYS_USAGE: u64 = 124;
pub const SYS_NICE: u64 = 125;
pub const SYS_CPU_LIMIT: u64 = 126;
pub const SYS_DEVICE_CLAIM: u64 = 127;

/// A PCI device, by bus, device and function, as `SYS_DEVICE_CLAIM` names
/// one.
pub fn pci_device(bus: u8, device: u8, function: u8) -> u64 {
    (bus as u64) << 8 | ((device & 0x1F) as u64) << 3 | (function & 7) as u64
}

/// Make PCI device `bdf` ([`pci_device`]) this program's to drive. On a
/// machine with an IOMMU the device then reaches the memory this program
/// was given for devices ([`sys_phys_alloc`]) and nothing else, and the
/// answer is `true`; `false` is a claim nothing on this machine can
/// enforce. Another program's device, or a program that may not configure
/// devices, is refused.
pub fn sys_device_claim(bdf: u64) -> Result<bool, Refused> {
    match unsafe { syscall2(SYS_DEVICE_CLAIM, bdf, 0) } {
        0 => Ok(false),
        1 => Ok(true),
        ret => Err(refusal(ret)),
    }
}

/// How many words [`sys_pci_device`] writes: see `quark_rt::pci::Info`.
pub const PCI_RECORD: usize = 21;

/// What the kernel found of the first PCI device at or after `from` that
/// this program holds (`CAP_TYPE_PCI_DEVICE`), written into `record`: its
/// address, or `None` when there is none.
pub fn sys_pci_device(from: u64, record: &mut [u64; PCI_RECORD]) -> Option<u64> {
    match unsafe { syscall2(SYS_PCI_DEVICE, from, record.as_mut_ptr() as u64) } {
        u64::MAX => None,
        at => Some(at),
    }
}

/// Read `width` bytes (1, 2 or 4) of device `bdf`'s configuration at
/// `offset`, a multiple of `width`.
pub fn sys_pci_read(bdf: u64, offset: u64, width: u64) -> Result<u32, ()> {
    match unsafe { syscall3(SYS_PCI_READ, bdf, offset, width) } {
        u64::MAX => Err(()),
        value => Ok(value as u32),
    }
}

/// Write them. What the kernel keeps — a BAR, the MSI capability, turning
/// bus mastering on before the device is claimed — is
/// [`Refused::NotAllowed`].
pub fn sys_pci_write(bdf: u64, offset: u64, width: u64, value: u32) -> Result<(), Refused> {
    match unsafe { syscall4(SYS_PCI_WRITE, bdf, offset, width, value as u64) } {
        0 => Ok(()),
        ret => Err(refusal(ret)),
    }
}

/// Memory for display device `bdf`'s screen, `pages` long: where it begins,
/// with a `PhysRange` over it now in this program's empty `slot`. One run
/// for each device, nobody's to free, the same every time it is asked for —
/// for a driver of a device that draws from memory, which has claimed it.
pub fn sys_display_memory(bdf: u64, pages: u64, slot: usize) -> Result<u64, ()> {
    match unsafe { syscall3(SYS_DISPLAY_MEMORY, bdf, pages, slot as u64) } {
        u64::MAX => Err(()),
        base => Ok(base),
    }
}

/// How many times device `bdf`, this program's, has reached for memory it
/// may not.
pub fn sys_device_stopped(bdf: u64) -> Result<u64, Refused> {
    match unsafe { syscall2(SYS_DEVICE_CLAIM, bdf, 1) } {
        ret @ (NOT_ALLOWED | u64::MAX) => Err(refusal(ret)),
        count => Ok(count),
    }
}

/// What was used of the machine ([`sys_usage`]): nanoseconds in the
/// program, nanoseconds in the kernel for it, and how many times it gave the
/// processor up and had it taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub user_ns: u64,
    pub system_ns: u64,
    pub voluntary: u64,
    pub involuntary: u64,
}

impl Usage {
    /// Nanoseconds run, in the program and in the kernel.
    pub fn total_ns(&self) -> u64 {
        self.user_ns + self.system_ns
    }
}

/// Whose use [`sys_usage`] says: this program's, the children it has
/// collected, this task's.
pub const USAGE_PROGRAM: u64 = 0;
pub const USAGE_CHILDREN: u64 = 1;
pub const USAGE_TASK: u64 = 2;
/// [`sys_usage`]'s 4: one task, any.
pub const USAGE_OF_TASK: u64 = 4;
const USAGE_OF: u64 = 3;

/// What this program, the children it has collected, or this task has used.
pub fn sys_usage(whose: u64) -> Result<Usage, ()> {
    usage(whose, 0)
}

/// What the program task `tid` is a task of has used. Anybody may ask.
pub fn sys_usage_of(tid: usize) -> Result<Usage, ()> {
    usage(USAGE_OF, tid as u64)
}

/// What task `tid` itself has used: a thread's own processor clock.
pub fn sys_usage_of_task(tid: usize) -> Result<Usage, ()> {
    usage(USAGE_OF_TASK, tid as u64)
}

/// Where the caller's robust list's head is, in its memory: the mutexes the
/// kernel marks as their owner's dead if the caller dies holding them. 0 for
/// none, `u64::MAX` to ask. Where it was.
pub fn sys_robust_list(head: u64) -> u64 {
    unsafe { syscall1(SYS_ROBUST_LIST, head) }
}

fn usage(whose: u64, of: u64) -> Result<Usage, ()> {
    let mut out = [0u64; 4];
    let ret = unsafe { syscall3(SYS_USAGE, whose, out.as_mut_ptr() as u64, of) };
    if ret == u64::MAX {
        return Err(());
    }
    Ok(Usage { user_ns: out[0], system_ns: out[1], voluntary: out[2], involuntary: out[3] })
}

/// How nice process `pid` (0 for this one) is, -20 to 19, set to `nice`
/// unless that is `None`: the share of its band it has while it and another
/// are both computing. Anybody may be nicer; to be less nice takes
/// `TaskMgmt`. Answers with how nice it was.
pub fn sys_nice(pid: u64, nice: Option<i64>) -> Result<i64, Refused> {
    let new = nice.map_or(u64::MAX, |n| n as u64);
    match unsafe { syscall2(SYS_NICE, pid, new) } {
        ret @ 0..=39 => Ok(ret as i64 - 20),
        ret => Err(refusal(ret)),
    }
}

/// How many seconds of processor time this program may have — SIGXCPU past
/// `soft`, the end at `hard`, `u64::MAX` for none — and what it was.
/// Raising the hard limit takes `TaskMgmt`.
pub fn sys_cpu_limit(soft: u64, hard: u64) -> Result<(u64, u64), Refused> {
    let mut old = [0u64; 2];
    match unsafe { syscall4(SYS_CPU_LIMIT, soft, hard, old.as_mut_ptr() as u64, 0) } {
        0 => Ok((old[0], old[1])),
        ret => Err(refusal(ret)),
    }
}

/// How many seconds of processor time this program may have, as
/// [`sys_cpu_limit`] says it.
pub fn sys_cpu_limit_get() -> (u64, u64) {
    let mut old = [u64::MAX; 2];
    let _ = unsafe { syscall4(SYS_CPU_LIMIT, 0, 0, old.as_mut_ptr() as u64, 1) };
    (old[0], old[1])
}
pub const SYS_MAP_ANON: u64 = 192;
pub const SYS_MEM_INFO: u64 = 193;
pub const SYS_OBJECT_CREATE: u64 = 194;
pub const SYS_OBJECT_MAP: u64 = 195;
pub const SYS_OBJECT_CTL: u64 = 196;
pub const SYS_OBJECT_SYNC: u64 = 197;
pub const SYS_PAGE_OUT: u64 = 198;

/// `sys_object_map`'s flags.
pub const OBJECT_MAP_WRITE: u64 = 1;
pub const OBJECT_MAP_SHARED: u64 = 2;
pub const OBJECT_MAP_EXEC: u64 = 4;
/// `sys_object_ctl`'s operations.
pub const OBJECT_RESIZE: u64 = 0;
pub const OBJECT_READ_PAGE: u64 = 1;
pub const OBJECT_WRITE_PAGE: u64 = 2;
pub const OBJECT_TAKE_DIRTY: u64 = 3;
pub const OBJECT_RELEASE: u64 = 4;
/// Make this object the one memory is written out to when there is not
/// enough of it. For a holder of [`CAP_TYPE_SWAP`]; one object at a time.
pub const OBJECT_SWAP: u64 = 5;
/// Take a dirty page to write: as [`OBJECT_TAKE_DIRTY`], but the page is
/// neither clean nor dirty until [`OBJECT_WRITTEN`] says how it went — so
/// that a page whose writing failed is not given up as if it had not.
pub const OBJECT_TAKE_OUT: u64 = 6;
/// A page taken with [`OBJECT_TAKE_OUT`] was written (`a` = 1) or could not
/// be (`a` = 0); `b` is the page.
pub const OBJECT_WRITTEN: u64 = 7;
/// [`OBJECT_RELEASE`]'s answer while a task other than the pager holds a
/// capability for the object — it was given one to map with and has not
/// mapped yet. Nothing was released, and nothing will say when the
/// capability has gone: ask again.
pub const OBJECT_RELEASE_LATER: u64 = 1;
/// A `MemObject` capability's access bits.
pub const OBJECT_ACCESS_READ: u64 = 1;
pub const OBJECT_ACCESS_WRITE: u64 = 2;

// --- 0x80  synchronisation ---
pub const SYS_FUTEX_WAIT: u64 = 128;
pub const SYS_FUTEX_WAKE: u64 = 129;
pub const SYS_FUTEX_WAIT_TIMEOUT: u64 = 130;
pub const SYS_EVENT_CREATE: u64 = 131;
/// Where the caller's robust list is: `set_robust_list`.
pub const SYS_ROBUST_LIST: u64 = 132;

// --- 0x88  signals, continued again ---
pub const SYS_SIG_QUEUE: u64 = 136;
pub const SYS_SIGNAL_FD: u64 = 137;

// --- 0x90  time ---
pub const SYS_TICKS: u64 = 144;
pub const SYS_BOOT_TIME: u64 = 145;
pub const SYS_CLOCK: u64 = 149;
pub const SYS_CLOCK_SET: u64 = 150;
pub const SYS_PTIMER: u64 = 151;

// --- 0xB0  sockets ---
pub const SYS_SOCK_FD: u64 = 176;
pub const SYS_SOCK_INFO: u64 = 177;
pub const SYS_SOCKET: u64 = 178;
pub const SYS_SOCKET_BIND: u64 = 179;
pub const SYS_SOCKET_LISTEN: u64 = 180;
pub const SYS_SOCKET_CONNECT: u64 = 181;
pub const SYS_SOCKET_ACCEPT: u64 = 182;
pub const SYS_SOCKET_PEER: u64 = 183;
pub const SYS_SOCKET_OPTION: u64 = 184;

// --- 0xA0  kernel debug console ---
pub const SYS_WRITE: u64 = 160;
pub const SYS_CONSOLE_POS: u64 = 161;
// 0xA8: devices, where block 0x70 had no room left.
pub const SYS_PCI_DEVICE: u64 = 168;
pub const SYS_PCI_READ: u64 = 169;
pub const SYS_PCI_WRITE: u64 = 170;
pub const SYS_DISPLAY_MEMORY: u64 = 171;

// --- 0xE0  descriptors, continued ---
pub const SYS_FD_SERVE: u64 = 224;
pub const SYS_FD_SERVED: u64 = 225;
pub const SYS_FD_HOLDS: u64 = 226;
pub const SYS_FD_COOKIE: u64 = 227;
pub const SYS_FD_FLAGS: u64 = 228;
pub const SYS_FD_REAP: u64 = 229;
/// What a descriptor names, and whether its other end has gone.
pub const SYS_FD_KIND: u64 = 230;
/// A server gives a task that is calling it an end of the pipe a key names.
pub const SYS_FD_SERVE_PIPE: u64 = 231;
/// Wait for the other end of a named pipe to be opened.
pub const SYS_PIPE_PEER: u64 = 232;
/// A server says what an object of its own is ready for, to a poll.
pub const SYS_FD_READY: u64 = 233;
/// The working directory's descriptor: one past the ordinary numbers. It can
/// be copied to and from and asked about, and nothing else.
pub const FD_CWD: usize = 64;
/// `SYS_FD_FLAGS`: close the descriptor when the program becomes another.
pub const FD_FLAG_CLOEXEC: u64 = 1;

// --- 0xF0  ABI introspection ---
pub const SYS_ABI_VERSION: u64 = 240;







// A call with fewer arguments is the call with five, and zeroes.
//
// Not a convenience. The kernel reads the registers a call is documented to
// take, and a call that is given another argument in a later version reads it
// from every caller there is — including the ones written before, which
// passed two and left in the third register whatever they had last computed.
// These used to mark the registers they did not set as clobbered and nothing
// more, which is exactly that. An argument not given is given as nothing.

#[inline(always)]
pub unsafe fn syscall0(nr: u64) -> u64 {
    syscall5(nr, 0, 0, 0, 0, 0)
}

#[inline(always)]
pub unsafe fn syscall1(nr: u64, arg0: u64) -> u64 {
    syscall5(nr, arg0, 0, 0, 0, 0)
}

#[inline(always)]
pub unsafe fn syscall2(nr: u64, arg0: u64, arg1: u64) -> u64 {
    syscall5(nr, arg0, arg1, 0, 0, 0)
}

#[inline(always)]
pub unsafe fn syscall3(nr: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    syscall5(nr, arg0, arg1, arg2, 0, 0)
}

#[inline(always)]
pub unsafe fn syscall4(nr: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    syscall5(nr, arg0, arg1, arg2, arg3, 0)
}

#[inline(always)]
pub unsafe fn syscall5(nr: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let ret: u64;
    asm!(
        "mov r10, {arg3}",
        "syscall",
        arg3 = in(reg) arg3,
        inlateout("rax") nr => ret,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        inlateout("rdx") arg2 => _,
        inlateout("r8") arg4 => _,
        out("rcx") _,
        out("r9") _,
        out("r10") _,
        out("r11") _,
        options(nostack)
    );
    ret
}

// Typed wrappers

pub fn sys_exit() -> ! {
    unsafe { syscall0(SYS_EXIT) };
    loop {
        core::hint::spin_loop();
    }
}

/// End the program: every task in this address space, with one status,
/// reported to a parent waiting in `sys_wait`. What returning from `main`
/// means, and what `exit` means in C.
pub fn sys_exit_program(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT_PROGRAM, code as u32 as u64) };
    loop {
        core::hint::spin_loop();
    }
}

/// End the calling task with a status, reported to a parent waiting in
/// `sys_wait`. A program with other threads goes on running in them, with
/// everything it has open: to end the program, see [`sys_exit_program`].
pub fn sys_exit_code(code: i32) -> ! {
    unsafe { syscall1(SYS_EXIT_CODE, code as u32 as u64) };
    loop {
        core::hint::spin_loop();
    }
}

pub fn sys_yield() {
    unsafe { syscall0(SYS_YIELD) };
}

pub fn sys_write(buf: &[u8]) -> u64 {
    unsafe { syscall2(SYS_WRITE, buf.as_ptr() as u64, buf.len() as u64) }
}

/// Returns (row, col) of the kernel console cursor.
pub fn sys_console_pos() -> (usize, usize) {
    let ret = unsafe { syscall0(SYS_CONSOLE_POS) };
    let row = (ret >> 32) as usize;
    let col = (ret & 0xFFFF_FFFF) as usize;
    (row, col)
}

pub fn sys_getpid() -> u64 {
    unsafe { syscall0(SYS_GETPID) }
}

pub fn sys_get_uid() -> (u32, u32) {
    let ret = unsafe { syscall0(SYS_GET_UID) };
    let uid = (ret >> 32) as u32;
    let gid = (ret & 0xFFFF_FFFF) as u32;
    (uid, gid)
}

pub fn sys_set_uid(tid: usize, uid: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SET_UID, tid as u64, uid as u64) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

pub fn sys_set_gid(tid: usize, gid: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SET_GID, tid as u64, gid as u64) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

/// How many groups a task may be in besides its own.
pub const MAX_GROUPS: usize = 16;

/// The groups `tid` is in besides its own (0 for the caller's), into `out`:
/// as many as fit. How many there are, which is how to ask with no room.
pub fn sys_groups(tid: usize, out: &mut [u32]) -> Result<usize, ()> {
    let ret = unsafe { syscall4(SYS_GROUPS, 0, tid as u64, out.as_mut_ptr() as u64, out.len() as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Say which groups `tid` is in besides its own: the caller (0), or a child
/// it has created and not started. Needs `SetUid`.
pub fn sys_set_groups(tid: usize, groups: &[u32]) -> Result<(), ()> {
    let ret = unsafe { syscall4(SYS_GROUPS, 1, tid as u64, groups.as_ptr() as u64, groups.len() as u64) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

/// Say who a task is — its user, its group and its groups, in one step —
/// for a server that holds `SetUid`. `client` is a task in a call to this
/// one, and `target` is that task or a child it has created and not started:
/// the kernel checks which as it acts, where a check made before a
/// `sys_set_uid` would be about whatever had the number by then.
pub fn sys_identify(client: usize, target: usize, uid: u32, gid: u32, groups: &[u32]) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(
            SYS_IDENTIFY,
            client as u64,
            target as u64,
            (uid as u64) << 32 | gid as u64,
            groups.as_ptr() as u64,
            groups.len() as u64,
        )
    };
    if ret == 0 { Ok(()) } else { Err(()) }
}

pub fn sys_get_tuid(tid: usize) -> Result<(u32, u32), ()> {
    let ret = unsafe { syscall1(SYS_GET_TUID, tid as u64) };
    if ret == u64::MAX { return Err(()); }
    let uid = (ret >> 32) as u32;
    let gid = (ret & 0xFFFF_FFFF) as u32;
    Ok((uid, gid))
}

pub fn sys_task_kill(tid: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_TASK_KILL, tid as u64) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

/// Returns (state, parent_tid, uid) or Err if no task at that TID.
/// state: 0=Ready, 1=Running, 2=Blocked, 3=Dead
/// Scheduling bands, best first. A task runs only when nothing better is
/// waiting; within a band they take turns.
pub const PRIO_DRIVER: u8 = 0;
pub const PRIO_SERVER: u8 = 1;
pub const PRIO_NORMAL: u8 = 2;
pub const PRIO_IDLE: u8 = 3;

/// Put `tid` in a scheduling band.
///
/// Needs `TaskMgmt` over the target, and cannot grant a better band than the
/// caller is in — the same narrowing rule capabilities follow.
pub fn sys_task_priority(tid: usize, band: u8) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_TASK_PRIORITY, tid as u64, band as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// What a program was started as: its command line, set and read.
pub const SYS_PROGRAM_NAME: u64 = 214;
/// How much of a program's command line the kernel keeps.
pub const PROGRAM_NAME_MAX: usize = 128;

/// Say what `tid`'s program was started as — `tid` a task of this program's,
/// or a child it has made and not started: its arguments, each ended by a
/// nought, as much of them as fits.
/// What a task is called: a thread's name, Linux's `comm`.
pub const SYS_TASK_NAME: u64 = 215;
/// The longest a task's name is.
pub const TASK_NAME_MAX: usize = 15;

/// Call task `tid` — one of the caller's program, or, for a server, one of
/// the program of `client`, who is calling it — `name`: its first fifteen
/// bytes. `client` is 0 for the caller's own.
pub fn sys_task_name_set(tid: usize, name: &[u8], client: usize) -> Result<(), ()> {
    let n = name.len().min(TASK_NAME_MAX);
    let ret = unsafe { syscall5(SYS_TASK_NAME, tid as u64, 0, name.as_ptr() as u64, n as u64, client as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// What task `tid` is called, into `out`: how long the name is, 0 for none —
/// its program's name, then.
pub fn sys_task_name(tid: usize, out: &mut [u8]) -> Option<usize> {
    let ret = unsafe { syscall4(SYS_TASK_NAME, tid as u64, 1, out.as_mut_ptr() as u64, out.len() as u64) };
    (ret != u64::MAX).then_some(ret as usize)
}

pub fn sys_program_name_set(tid: usize, args: &[&[u8]]) -> Result<(), ()> {
    let mut line = [0u8; PROGRAM_NAME_MAX];
    let mut n = 0;
    for arg in args {
        if n + arg.len() + 1 > line.len() {
            break;
        }
        line[n..n + arg.len()].copy_from_slice(arg);
        n += arg.len() + 1;
    }
    let ret = unsafe { syscall4(SYS_PROGRAM_NAME, tid as u64, 0, line.as_ptr() as u64, n as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// What `tid`'s program was started as, into `out`: how many bytes.
pub fn sys_program_name(tid: usize, out: &mut [u8]) -> Option<usize> {
    let ret = unsafe { syscall4(SYS_PROGRAM_NAME, tid as u64, 1, out.as_mut_ptr() as u64, out.len() as u64) };
    (ret != u64::MAX).then_some(ret as usize)
}

/// What a program is called: the last part of the first of its arguments,
/// fifteen bytes of it at most, as Unix's `comm` is.
pub fn program_comm(line: &[u8]) -> &[u8] {
    let first = &line[..line.iter().position(|&b| b == 0).unwrap_or(line.len())];
    let base = &first[first.iter().rposition(|&b| b == b'/').map_or(0, |p| p + 1)..];
    let base = base.strip_suffix(b".ELF").unwrap_or(base);
    &base[..base.len().min(15)]
}

/// The band task `tid` was put in, whatever it is running in for now.
pub fn sys_task_band(tid: usize) -> Option<u8> {
    let ret = unsafe { syscall2(SYS_TASK_PRIORITY, tid as u64, u64::MAX) };
    (ret != u64::MAX).then_some(ret as u8)
}

/// Ask to be told when `tid` dies.
///
/// The notification arrives at the caller's next receive as a message from the
/// kernel: sender 0, tag [`crate::ipc::TAG_TASK_DIED`], `data[0]` the task
/// that died. It is the answer to "who still holds this?" for anything lent
/// out — a display, a window, the keyboard — without polling for it.
///
/// Fails if `tid` is already dead, which is an answer rather than a problem:
/// there is nothing to wait for and the caller may reclaim at once.
pub fn sys_task_watch(tid: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_TASK_WATCH, tid as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// The program `tid` belongs to: its address space's id, never reused.
/// Threads of one program share it.
pub fn sys_task_space(tid: usize) -> Result<u64, ()> {
    let ret = unsafe { syscall1(SYS_TASK_SPACE, tid as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret) }
}

/// Make a task for the address space `cr3`, which this task created, to be
/// started there later. It belongs to that program from the start.
pub fn sys_task_create_in(cr3: u64) -> Result<usize, ()> {
    let ret = unsafe { syscall1(SYS_TASK_CREATE_IN, cr3) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Be sent [`crate::ipc::TAG_SPACE_DIED`] when program `space` has no task
/// left: sender 0, `data[0]` the space id. `Err` if it has none already.
pub fn sys_space_watch(space: u64) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_SPACE_WATCH, space) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_task_info(tid: usize) -> Result<(u8, usize, u32), ()> {
    let ret = unsafe { syscall1(SYS_TASK_INFO, tid as u64) };
    if ret == u64::MAX { return Err(()); }
    let state = (ret & 0xF) as u8;
    let parent = ((ret >> 4) & 0x0FFF_FFFF) as usize;
    let uid = (ret >> 32) as u32;
    Ok((state, parent, uid))
}

pub fn sys_send(dest: usize, msg: &crate::ipc::Message) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SEND, dest as u64, msg as *const _ as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_recv(from: usize, msg: &mut crate::ipc::Message) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_RECV, from as u64, msg as *mut _ as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_call(dest: usize, msg: &crate::ipc::Message, reply: &mut crate::ipc::Message) -> Result<(), ()> {
    let ret = unsafe {
        syscall3(SYS_CALL, dest as u64, msg as *const _ as u64, reply as *mut _ as u64)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// The task called may read what [`sys_call_lend`] and friends lend it.
pub const LEND_READ: u64 = 1 << 62;
/// The task called may write into it.
pub const LEND_WRITE: u64 = 1 << 63;

fn call_lend(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    buf: *const u8,
    len: usize,
    access: u64,
) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(
            SYS_CALL_LEND,
            dest as u64,
            msg as *const _ as u64,
            reply as *mut _ as u64,
            buf as u64,
            len as u64 | access,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// [`sys_call`], lending `dest` a buffer to read until it replies. `dest`
/// copies out of it with [`sys_lent_read`]; it never learns where it is.
pub fn sys_call_lend(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    buf: &[u8],
) -> Result<(), ()> {
    call_lend(dest, msg, reply, buf.as_ptr(), buf.len(), LEND_READ)
}

/// [`sys_call`], lending `dest` a buffer to fill with [`sys_lent_write`].
pub fn sys_call_lend_mut(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    buf: &mut [u8],
) -> Result<(), ()> {
    call_lend(dest, msg, reply, buf.as_ptr(), buf.len(), LEND_WRITE)
}

/// [`sys_call`], lending `dest` a buffer to read and write.
pub fn sys_call_lend_rw(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    buf: &mut [u8],
) -> Result<(), ()> {
    call_lend(dest, msg, reply, buf.as_ptr(), buf.len(), LEND_READ | LEND_WRITE)
}

/// [`sys_call`], offering `dest` a copy of the capability in `slot`, which it
/// may take with [`sys_cap_take`] before it replies. Offering an empty or
/// revoked slot fails without calling.
pub fn sys_call_offer(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    slot: usize,
) -> Result<(), ()> {
    let ret = unsafe {
        syscall4(
            SYS_CALL_OFFER,
            dest as u64,
            msg as *const _ as u64,
            reply as *mut _ as u64,
            slot as u64,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Mint a capability into a slot of this program's for the length of a call:
/// which slot. The highest empty one, taken by minting into it, which the
/// kernel refuses for a slot that is not empty — so that two threads of one
/// program, which share its capabilities, never take the same one. One slot
/// for everybody (`SLOT_SCRATCH`) was a slot one thread's offer took from
/// under another's: a USB disk's thread, offering itself for its name while
/// the driver's other thread was in a call with its own offer there, was
/// refused all four names a disk may have.
pub fn mint_scratch(cap_type: u64, param0: u64, param1: u64) -> Result<usize, ()> {
    let me = sys_getpid() as usize;
    for slot in (SLOT_SCRATCH + 2..64).rev().chain([SLOT_SCRATCH]) {
        if sys_cap_read(me, slot).is_ok_and(|c| c.cap_type != 0) {
            continue;
        }
        if sys_cap_mint(slot, cap_type, param0, param1).is_ok() {
            return Ok(slot);
        }
    }
    Err(())
}

/// [`sys_call`], offering `dest` the right to call this task back.
///
/// A server that only ever answers needs nothing of the kind. One that has to
/// tell a client something unprompted — the display is going, Ctrl-C was
/// pressed — does, and this is the only way it gets it: the capability is
/// minted into a slot of its own (`mint_scratch`) for the length of the call.
pub fn sys_call_offer_self(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
) -> Result<(), ()> {
    let me = sys_getpid();
    let slot = mint_scratch(CAP_TYPE_ENDPOINT, me, 0)?;
    let called = sys_call_offer(dest, msg, reply, slot);
    let _ = sys_cap_delete(slot);
    called
}

/// What goes with a [`sys_call_with`]: any of a buffer lent, a capability
/// offered and a deadline.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CallWith {
    /// The buffer lent, and its length with [`LEND_READ`] and [`LEND_WRITE`]
    /// as access; a `len_access` of 0 lends nothing.
    pub buf: u64,
    pub len_access: u64,
    /// The slot offered, or `u64::MAX` for none.
    pub offer: u64,
    /// Ticks to wait for the reply; 0 waits for ever.
    pub ticks: u64,
}

impl CallWith {
    /// Nothing lent, nothing offered, no deadline: a plain call.
    pub const PLAIN: CallWith = CallWith { buf: 0, len_access: 0, offer: u64::MAX, ticks: 0 };
}

/// A call carrying whatever `with` describes. Answers as
/// [`sys_call_timeout`] does; a part the kernel refuses (a buffer the caller
/// cannot lend, an empty slot) fails the call before it is made.
pub fn sys_call_with(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    with: &CallWith,
) -> CallOutcome {
    let ret = unsafe {
        syscall4(
            SYS_CALL_WITH,
            dest as u64,
            msg as *const _ as u64,
            reply as *mut _ as u64,
            with as *const _ as u64,
        )
    };
    match ret {
        0 => CallOutcome::Replied,
        1 => CallOutcome::TimedOut,
        _ => CallOutcome::Failed,
    }
}

/// Copy out of what `client` lent with the call being served, from `offset`.
/// Only between receiving that call and answering it.
pub fn sys_lent_read(client: usize, offset: usize, dst: &mut [u8]) -> Result<usize, ()> {
    let ret = unsafe {
        syscall4(
            SYS_LENT_READ,
            client as u64,
            offset as u64,
            dst.as_mut_ptr() as u64,
            dst.len() as u64,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Copy into what `client` lent with the call being served, at `offset`.
pub fn sys_lent_write(client: usize, offset: usize, src: &[u8]) -> Result<usize, ()> {
    let ret = unsafe {
        syscall4(
            SYS_LENT_WRITE,
            client as u64,
            offset as u64,
            src.as_ptr() as u64,
            src.len() as u64,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Outcome of a call that carries a deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The target replied; `reply` holds its message.
    Replied,
    /// The deadline passed with no reply.
    TimedOut,
    /// The call could not be made at all (bad target, no capability, dead task).
    Failed,
}

/// Synchronous call that gives up after `timeout_ticks` (100 Hz, so 10ms each).
///
/// Use this rather than [`sys_call`] for any destination that is not known to
/// be a running server: a task that never reaches sys_recv leaves a plain
/// sys_call blocked forever.
pub fn sys_call_timeout(
    dest: usize,
    msg: &crate::ipc::Message,
    reply: &mut crate::ipc::Message,
    timeout_ticks: u64,
) -> CallOutcome {
    let ret = unsafe {
        syscall4(
            SYS_CALL_TIMEOUT,
            dest as u64,
            msg as *const _ as u64,
            reply as *mut _ as u64,
            timeout_ticks,
        )
    };
    match ret {
        0 => CallOutcome::Replied,
        1 => CallOutcome::TimedOut,
        _ => CallOutcome::Failed,
    }
}

pub fn sys_reply(dest: usize, msg: &crate::ipc::Message) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_REPLY, dest as u64, msg as *const _ as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_task_create() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_TASK_CREATE) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Free an address space the caller made and nothing runs in, with every
/// page that was given to it.
pub fn sys_addrspace_destroy(cr3: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_ADDRSPACE_DESTROY, cr3 as u64) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

pub fn sys_addrspace_create() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_ADDRSPACE_CREATE) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

pub fn sys_addrspace_map(cr3: usize, virt: usize, phys: usize, pages: usize, flags: u64) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(
            SYS_ADDRSPACE_MAP,
            cr3 as u64,
            virt as u64,
            phys as u64,
            pages as u64,
            flags,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Move `pages` pages of the caller's own memory, starting at `from`, into
/// address space `cr3` at `virt`. They leave the caller and belong to `cr3`
/// from then on, which frees them when it is destroyed. `flags` bit 0 makes
/// them writable there.
///
/// Only memory the caller got from [`sys_mmap`] can be given, at most 256
/// pages a call, and only to an unoccupied range.
pub fn sys_addrspace_give(cr3: usize, virt: usize, from: usize, pages: usize, flags: u64) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(
            SYS_ADDRSPACE_GIVE,
            cr3 as u64,
            virt as u64,
            from as u64,
            pages as u64,
            flags,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_task_start(tid: usize, rip: u64, rsp: u64, cr3: usize) -> Result<(), ()> {
    let ret = unsafe {
        syscall4(SYS_TASK_START, tid as u64, rip, rsp, cr3 as u64)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// `count` frames in a row, of ordinary memory: wherever the kernel has
/// them, which on a machine with more than four gigabytes is above that.
/// Answers with the address of the first.
pub fn sys_phys_alloc(count: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_PHYS_ALLOC, count as u64, 0) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// `count` frames in a row below four gigabytes: memory a device is told
/// the address of. A network card's ring and a disk controller's table are
/// registers thirty-two bits wide, and a frame above that is one the device
/// cannot be told of — it is handed the low half of the address and writes
/// to whatever is there. `Err` when there is none below, whatever is free
/// above.
pub fn sys_phys_alloc_low(count: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_PHYS_ALLOC, count as u64, 1) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

pub fn sys_phys_free(addr: usize, count: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_PHYS_FREE, addr as u64, count as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_ioport_read(port: u16) -> u64 {
    unsafe { syscall3(SYS_IOPORT, port as u64, 0, 0) }
}

pub fn sys_ioport_write(port: u16, val: u8) {
    unsafe { syscall3(SYS_IOPORT, port as u64, 1, val as u64) };
}

pub fn sys_ioport_read16(port: u16) -> u16 {
    unsafe { syscall3(SYS_IOPORT, port as u64, 2, 0) as u16 }
}

pub fn sys_ioport_write16(port: u16, val: u16) {
    unsafe { syscall3(SYS_IOPORT, port as u64, 3, val as u64) };
}

pub fn sys_ioport_read32(port: u16) -> u32 {
    unsafe { syscall3(SYS_IOPORT, port as u64, 4, 0) as u32 }
}

pub fn sys_ioport_write32(port: u16, val: u32) {
    unsafe { syscall3(SYS_IOPORT, port as u64, 5, val as u64) };
}

pub fn sys_ioport_rep_insw(port: u16, buf: &mut [u16]) -> Result<(), ()> {
    let ret = unsafe {
        syscall4(SYS_IOPORT_REP, port as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_ioport_rep_outsw(port: u16, buf: &[u16]) -> Result<(), ()> {
    let ret = unsafe {
        syscall4(SYS_IOPORT_REP, port as u64, buf.as_ptr() as u64, buf.len() as u64, 1)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Reserve `pages` pages of memory at `addr`, each given its frame when first
/// touched — or all of them now, with `populate`. The range must be empty.
pub fn sys_map_anon(addr: usize, pages: usize, populate: bool) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_MAP_ANON, addr as u64, pages as u64, populate as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// `sys_map_anon`, refused outright if `pages` is more than the machine has:
/// what Linux does for a mapping without `MAP_NORESERVE`.
pub fn sys_map_anon_accounted(addr: usize, pages: usize) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_MAP_ANON, addr as u64, pages as u64, 2) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Make a memory object of `bytes` bytes, whose pages this task will provide
/// when asked (`ipc::TAG_PAGE_IN`), and which it knows as `cookie`. A
/// read-write `MemObject` capability goes into `slot`; the object's id is
/// returned.
pub fn sys_object_create(cookie: u64, bytes: u64, slot: usize) -> Result<u64, ()> {
    let ret = unsafe { syscall3(SYS_OBJECT_CREATE, cookie, bytes, slot as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret) }
}

/// Map `pages` pages of the object the capability in `slot` names, from page
/// `first`, at `addr`. Each page is fetched when first touched.
pub fn sys_object_map(slot: usize, addr: usize, pages: usize, first: u64, flags: u64) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(SYS_OBJECT_MAP, slot as u64, addr as u64, pages as u64, first, flags)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// A pager's operation `op` on its object `id`. What `a` and `b` are, and what
/// comes back, depend on the operation.
pub fn sys_object_ctl(id: u64, op: u64, a: u64, b: u64) -> u64 {
    unsafe { syscall4(SYS_OBJECT_CTL, id, op, a, b) }
}

/// Have what was written through shared file mappings in `pages` pages from
/// `addr` reach the files, returning once it has.
pub fn sys_object_sync(addr: usize, pages: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_OBJECT_SYNC, addr as u64, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Free frames in the machine, and pages charged to this task.
pub fn sys_mem_info() -> (usize, usize) {
    let ret = unsafe { syscall1(SYS_MEM_INFO, 0) };
    ((ret >> 32) as usize, (ret & 0xFFFF_FFFF) as usize)
}

/// Where memory is written out to when there is not enough of it: how many
/// pages of room there are, and how many are in use. Nought and nought on a
/// machine with nowhere.
pub fn sys_swap_room() -> (usize, usize) {
    let ret = unsafe { syscall1(SYS_MEM_INFO, 3) };
    if ret == u64::MAX {
        return (0, 0);
    }
    ((ret >> 32) as usize, (ret & 0xFFFF_FFFF) as usize)
}

/// Give up `pages` pages of this program's own from `addr`: its own memory
/// is written out, now, to wherever the machine writes memory out to, and
/// pages of files it maps to read go back to being untouched; the frames
/// are free when this returns. What is there is unchanged, and comes back
/// when it is next touched. Answers with how many pages were given up —
/// none of its own on a machine with nowhere to write them, and never one
/// that a system call another thread is in is using, one a `fork` left in
/// two programs, or the page it is told of signals through.
pub fn sys_page_out(addr: usize, pages: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_PAGE_OUT, addr as u64, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// How busy that has been: pages written out, and pages read back, since
/// the machine started.
pub fn sys_swap_traffic() -> (usize, usize) {
    let ret = unsafe { syscall1(SYS_MEM_INFO, 4) };
    if ret == u64::MAX {
        return (0, 0);
    }
    ((ret >> 32) as usize, (ret & 0xFFFF_FFFF) as usize)
}

/// How much memory the machine has, in frames of 4 KiB; and where it ends,
/// as the number of the frame after the last — more than a million
/// (0x100000) of them is a machine with memory above four gigabytes.
pub fn sys_mem_total() -> (usize, usize) {
    unsafe { (syscall1(SYS_MEM_INFO, 1) as usize, syscall1(SYS_MEM_INFO, 2) as usize) }
}

/// Fill as much of `buf` as one call gives (at most a mebibyte) with random
/// bytes, and say how much that was.
pub fn sys_getrandom(buf: &mut [u8]) -> Result<usize, ()> {
    let ret = unsafe { syscall3(SYS_GETRANDOM, buf.as_mut_ptr() as u64, buf.len() as u64, 0) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// How many processors the system is running on, and which of them — from 0
/// — the caller was on when it asked.
///
/// The second is true of that instant and of no other: a task is run by
/// whichever processor takes it next, and may be on another before this has
/// returned. It is for a test to see that there is more than one, and for a
/// statistic; nothing can be built on it.
pub fn sys_cpus() -> (usize, usize) {
    let ret = unsafe { syscall0(SYS_CPUS) };
    ((ret & 0xFFFF_FFFF) as usize, (ret >> 32) as usize)
}

pub fn sys_irq_register(irq: u8) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_IRQ_REGISTER, irq as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_irq_ack(irq: u8) {
    unsafe { syscall1(SYS_IRQ_ACK, irq as u64) };
}

/// An interrupt of a driver's own, for a device that sends its interrupts
/// as messages (MSI): the number the kernel will tell the driver of it by,
/// and what to program the device with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msi {
    /// The interrupt's number, 16 or above: the tag of the kernel's
    /// message, as a line's number is.
    pub irq: u8,
    /// The lower half of the address the device sends to. The upper is 0.
    pub address: u32,
    /// What it sends.
    pub data: u16,
}

/// Ask for an interrupt of this task's own. It is registered for it from
/// now on, as [`sys_irq_register`] registers a task for a line, until it
/// ends; there is nothing to acknowledge.
///
/// Needs the capability for any interrupt, and a machine whose processors
/// have local APICs — a message is sent to one. `Err` otherwise, or when
/// the thirty-two there are have all been given out.
pub fn sys_msi_alloc() -> Result<Msi, ()> {
    msi(unsafe { syscall2(SYS_MSI_ALLOC, 0, 0) })
}

/// The same, for PCI device `bdf`, which this program holds — and the
/// kernel aims the device's message itself, where it has an MSI
/// capability: one message, enabled, its line off. A device with MSI-X
/// alone has its messages in a table in its registers, which its driver
/// writes with what this answers.
pub fn sys_msi_alloc_for(bdf: u64) -> Result<Msi, ()> {
    msi(unsafe { syscall2(SYS_MSI_ALLOC, bdf, 1) })
}

fn msi(ret: u64) -> Result<Msi, ()> {
    if ret == u64::MAX {
        return Err(());
    }
    Ok(Msi { irq: (ret >> 48) as u8, data: (ret >> 32) as u16, address: ret as u32 })
}

pub fn sys_map_phys(phys: usize, virt: usize, pages: usize) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_MAP_PHYS, phys as u64, virt as u64, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_grant_ioport(tid: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_GRANT_IOPORT, tid as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_grant_irq(tid: usize, irq: u8) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_GRANT_IRQ, tid as u64, irq as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_grant_cap(tid: usize, caps: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_GRANT_CAP, tid as u64, caps as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub fn sys_fd_write(fd: usize, buf: &[u8]) -> u64 {
    unsafe { syscall3(SYS_FD_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

pub fn sys_fd_read(fd: usize, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(SYS_FD_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

/// Non-blocking read from a pipe fd.
/// Returns bytes read, 0 for EOF, 0xFFFF_FFFE if would block, u64::MAX on error.
pub fn sys_fd_read_nb(fd: usize, buf: &mut [u8]) -> u64 {
    unsafe { syscall3(SYS_FD_READ_NB, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) }
}

pub const WOULD_BLOCK: u64 = 0xFFFF_FFFE;

/// Write without waiting: what was taken, [`WOULD_BLOCK`] if nothing could
/// be, or `u64::MAX`.
pub fn sys_fd_write_nb(fd: usize, buf: &[u8]) -> u64 {
    unsafe { syscall3(SYS_FD_WRITE_NB, fd as u64, buf.as_ptr() as u64, buf.len() as u64) }
}

pub fn sys_fd_set(tid: usize, fd: usize, service_tid: usize, tag: u64) -> Result<(), ()> {
    let ret = unsafe { syscall4(SYS_FD_SET, tid as u64, fd as u64, service_tid as u64, tag) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Bind a net-server connection handle to a file descriptor.
///
/// Returns the descriptor, which then reads and writes with the ordinary
/// `sys_fd_read` and `sys_fd_write` — the point of the exercise.
pub fn sys_sock_fd(net_tid: usize, handle: usize) -> Result<usize, ()> {
    let r = unsafe { syscall2(SYS_SOCK_FD, net_tid as u64, handle as u64) };
    if r == u64::MAX { Err(()) } else { Ok(r as usize) }
}

/// The `(net_tid, handle)` behind a socket fd, for closing the connection.
pub fn sys_sock_info(fd: usize) -> Result<(usize, usize), ()> {
    let r = unsafe { syscall1(SYS_SOCK_INFO, fd as u64) };
    if r == u64::MAX {
        Err(())
    } else {
        Ok(((r >> 32) as usize, (r & 0xFFFF_FFFF) as usize))
    }
}

pub fn sys_futex_wait(addr: *const u32, expected: u32) -> u64 {
    unsafe { syscall2(SYS_FUTEX_WAIT, addr as u64, expected as u64) }
}

pub fn sys_futex_wake(addr: *const u32, max_wake: usize) -> u64 {
    unsafe { syscall2(SYS_FUTEX_WAKE, addr as u64, max_wake as u64) }
}

/// Returned by [`sys_futex_wait_timeout`] when the deadline passed.
pub const FUTEX_TIMED_OUT: u64 = 2;

/// The bit that makes a span of time a count of nanoseconds.
///
/// Every call that takes how long — a timeout, a timer, an alarm — takes a
/// *span*: a count of ticks, hundredths of a second, as all of them always
/// did, or with this bit set a count of nanoseconds. [`ns`] makes the
/// second kind.
pub const SPAN_NS: u64 = 1 << 63;

/// One tick, in nanoseconds.
pub const TICK_NS: u64 = 10_000_000;

/// A span of `nanos` nanoseconds, for any call that takes one. A time too
/// long to say this way — 292 years — is as long as can be said.
pub const fn ns(nanos: u64) -> u64 {
    if nanos >= SPAN_NS { u64::MAX } else { SPAN_NS | nanos }
}

/// A span as long as `time`.
pub fn span_of(time: core::time::Duration) -> u64 {
    let nanos = time.as_nanos();
    ns(if nanos > u64::MAX as u128 { u64::MAX } else { nanos as u64 })
}

/// Wait, giving up after `timeout`, a span: ticks, or nanoseconds from
/// [`ns`].
///
/// Returns 0 if woken, 1 if the word already differed, [`FUTEX_TIMED_OUT`] if
/// the deadline passed. A timeout of no time checks the word without
/// blocking.
pub fn sys_futex_wait_timeout(addr: *const u32, expected: u32, timeout: u64) -> u64 {
    unsafe { syscall3(SYS_FUTEX_WAIT_TIMEOUT, addr as u64, expected as u64, timeout) }
}

/// Receive with timeout, a span: ticks, or nanoseconds from [`ns`].
/// Returns Ok(()) if a message was received (written to `msg`),
/// Err(1) on timeout, Err(u64::MAX) on error.
pub fn sys_recv_timeout(from: usize, msg: &mut crate::ipc::Message, timeout: u64) -> Result<(), u64> {
    let ret = unsafe {
        syscall3(SYS_RECV_TIMEOUT, from as u64, msg as *mut _ as u64, timeout)
    };
    match ret {
        0 => Ok(()),
        other => Err(other),
    }
}

/// Seconds since 1970 when the machine was started, or 0 if it has no
/// clock. Add `sys_ticks() / 100` for the time now, to the second;
/// [`sys_clock_wall`] says it to the nanosecond.
pub fn sys_boot_time() -> u64 {
    unsafe { syscall0(SYS_BOOT_TIME) }
}

/// Seconds since 1970, or since boot on a machine with no clock.
pub fn unix_time() -> u64 {
    unix_ns() / 1_000_000_000
}

/// Nanoseconds since 1970, or since boot on a machine with no clock.
pub fn unix_ns() -> u64 {
    match sys_clock_wall() {
        0 => sys_clock(),
        wall => wall,
    }
}

/// The time since boot, in ticks: hundredths of a second.
pub fn sys_ticks() -> u64 {
    unsafe { syscall0(SYS_TICKS) }
}

/// The time since boot, in nanoseconds. It only goes forward, and it is
/// what every wait is measured by.
///
/// As fine as the machine's clock is: well under a microsecond where the
/// processor has a counter the kernel can keep time by, and ten
/// milliseconds where it has not.
pub fn sys_clock() -> u64 {
    unsafe { syscall1(SYS_CLOCK, 0) }
}

/// The date: nanoseconds since 1970, or 0 on a machine with no clock to say.
/// It moves when somebody sets it ([`sys_clock_set`]).
pub fn sys_clock_wall() -> u64 {
    unsafe { syscall1(SYS_CLOCK, 1) }
}

/// The clocks a program's timer is on: the date, and the time since boot.
pub const PTIMER_DATE: u64 = 0;
pub const PTIMER_SINCE_BOOT: u64 = 1;
/// [`SigInfo::code`] for a timer's signal, whose `who` is the timer's
/// number and, in the high half, its overruns.
pub const SI_TIMER: i64 = -2;

/// A timer of this program's on `clock` that raises `signo` (0 for none)
/// carrying `value`, for the program or for its task `task` alone (0 for
/// the program). Made disarmed. Its number.
pub fn sys_ptimer_create(clock: u64, signo: u64, value: u64, task: usize) -> Result<usize, ()> {
    match unsafe { syscall5(SYS_PTIMER, 0, clock, signo, value, task as u64) } {
        u64::MAX => Err(()),
        id => Ok(id as usize),
    }
}

fn ptimer_set(id: u64, first: u64, every: u64) -> Result<(u64, u64), ()> {
    let mut was = [0u64; 2];
    match unsafe { syscall5(SYS_PTIMER, 1, id, first, every, was.as_mut_ptr() as u64) } {
        u64::MAX => Err(()),
        _ => Ok((was[0], was[1])),
    }
}

/// Timer `id` first fires `first` from now and then every `every` (spans:
/// ticks, or [`ns`]); a `first` of 0 disarms it. How it stood before, in
/// nanoseconds: left, and between firings.
pub fn sys_ptimer_set(id: usize, first: u64, every: u64) -> Result<(u64, u64), ()> {
    ptimer_set(id as u64, first, every)
}

/// The same, first firing at `at`, a time on its clock in nanoseconds.
pub fn sys_ptimer_set_at(id: usize, at: u64, every: u64) -> Result<(u64, u64), ()> {
    ptimer_set(id as u64 | 1 << 32, at, every)
}

/// How timer `id` stands, in nanoseconds: left until it next fires (0 if it
/// is disarmed), and between firings.
pub fn sys_ptimer_get(id: usize) -> Result<(u64, u64), ()> {
    let mut stands = [0u64; 2];
    match unsafe { syscall3(SYS_PTIMER, 2, id as u64, stands.as_mut_ptr() as u64) } {
        u64::MAX => Err(()),
        _ => Ok((stands[0], stands[1])),
    }
}

/// Timer `id` is no more.
pub fn sys_ptimer_delete(id: usize) -> Result<(), ()> {
    match unsafe { syscall2(SYS_PTIMER, 3, id as u64) } {
        u64::MAX => Err(()),
        _ => Ok(()),
    }
}

/// Say what the date is: `nanos` nanoseconds since 1970, now. For a holder
/// of the right to (`CAP_TYPE_CLOCK`). The kernel writes it to the clock
/// that keeps time while the machine is off.
pub fn sys_clock_set(nanos: u64) -> Result<(), ()> {
    if unsafe { syscall1(SYS_CLOCK_SET, nanos) } == 0 { Ok(()) } else { Err(()) }
}

/// Turn the machine off, the way its firmware says to. For a holder of the
/// right (`CAP_TYPE_POWER`).
///
/// It comes back only if the machine is still on: the caller may not, or
/// the firmware's tables do not say how. What the machine owes its disks
/// is the caller's to see to first — `vfs::sync` — because the file servers
/// are programs and the kernel stops nothing but what it runs.
pub fn sys_power_off() {
    unsafe { syscall1(SYS_POWER, 0) };
}

/// Start the machine again. It comes back only if the caller may not.
pub fn sys_restart() {
    unsafe { syscall1(SYS_POWER, 1) };
}

/// A timer that is a descriptor: readable once its time has come, and read
/// as eight bytes — how many times it has fired since it was last read.
pub fn sys_timer_create() -> Result<usize, ()> {
    match unsafe { syscall0(SYS_TIMER_CREATE) } {
        u64::MAX => Err(()),
        fd => Ok(fd as usize),
    }
}

/// Set timer `fd` to fire `first` from now and every `interval` after that:
/// spans, ticks or nanoseconds from [`ns`]. No time at all for `first`
/// turns it off; none for `interval` is a timer that fires once.
pub fn sys_timer_set(fd: usize, first: u64, interval: u64) -> Result<(), ()> {
    if unsafe { syscall3(SYS_TIMER_SET, fd as u64, first, interval) } == 0 { Ok(()) } else { Err(()) }
}

/// How timer `fd` stands, in nanoseconds: how long until it next fires, 0
/// if it is not set, and what it repeats at.
pub fn sys_timer_get(fd: usize) -> Option<(u64, u64)> {
    let mut was = [0u64; 2];
    match unsafe { syscall2(SYS_TIMER_GET, fd as u64, was.as_mut_ptr() as u64) } {
        u64::MAX => None,
        _ => Some((was[0], was[1])),
    }
}

/// The same in ticks, as the call itself answers: each rounded up.
pub fn sys_timer_get_ticks(fd: usize) -> Option<(u64, u64)> {
    match unsafe { syscall2(SYS_TIMER_GET, fd as u64, 0) } {
        u64::MAX => None,
        packed => Some((packed & 0xFFFF_FFFF, packed >> 32)),
    }
}

/// Sleep for `nanos` nanoseconds: no less, and where the machine can wake a
/// task between two ticks, very little more.
pub fn sleep_ns(nanos: u64) {
    if nanos == 0 {
        return;
    }
    // Block by doing a recv_timeout from our own TID — nobody will send to us
    // specifically, so only the deadline ends it. Or a signal: one this
    // program handles ends the sleep, and that is the sleep over, since the
    // program asked to hear. A sleep that comes back early for any other
    // reason — a sibling thread was the one a signal was for — goes back.
    let from = sys_getpid() as usize;
    let deadline = sys_clock().saturating_add(nanos);
    loop {
        let now = sys_clock();
        if now >= deadline {
            return;
        }
        let mut msg = crate::ipc::Message::empty();
        if sys_recv_timeout(from, &mut msg, ns(deadline - now)) == Err(SLEEP_INTERRUPTED) {
            return;
        }
    }
}

/// Sleep for `ticks` ticks, ten milliseconds each.
pub fn sleep_ticks(ticks: u64) {
    sleep_ns(ticks.saturating_mul(TICK_NS));
}

/// Sleep for `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    sleep_ns(ms.saturating_mul(1_000_000));
}

/// One descriptor to watch, and what it did.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: u32,
    pub events: u32,
    pub revents: u32,
    pub _pad: u32,
}

impl PollFd {
    pub const fn new(fd: usize, events: u32) -> Self {
        PollFd { fd: fd as u32, events, revents: 0, _pad: 0 }
    }
}

/// Wait on several descriptors without building a set.
///
/// A set is the better primitive when it is waited on many times; this is for
/// the caller that waits once, which is what `poll(2)` is and what libwayland
/// calls every time it dispatches. The kernel builds a set internally, so the
/// saving is the two system calls that would otherwise bracket every wait.
///
/// Returns how many entries have a non-zero `revents`.
///
/// `timeout` is a span: ticks, or nanoseconds from [`ns`].
pub fn sys_poll(fds: &mut [PollFd], timeout: u64) -> Result<usize, ()> {
    let ret = unsafe {
        syscall3(SYS_POLL, fds.as_mut_ptr() as u64, fds.len() as u64, timeout)
    };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// One ready descriptor, as the kernel writes it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ready {
    pub token: u64,
    pub events: u32,
    pub _pad: u32,
}

impl Ready {
    pub const fn empty() -> Self {
        Ready { token: 0, events: 0, _pad: 0 }
    }
}

/// A set of descriptors to wait on. It is itself a descriptor.
///
/// Built once and waited on many times, which is the reason it is an object
/// rather than an array handed over on every call. [`sys_poll`] is the other
/// shape, for a caller that waits once.
pub fn sys_pollset_create() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_POLLSET_CREATE) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Watch `fd` for `events`, reporting `token` when it fires.
///
/// A descriptor that can never become ready is refused here rather than
/// accepted and never reported.
pub fn sys_pollset_add(set: usize, fd: usize, events: u32, token: u64) -> Result<(), ()> {
    let ret = unsafe { syscall5(SYS_POLLSET_CTL, set as u64, 0, fd as u64, events as u64, token) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Change what an already-watched descriptor is watched for.
pub fn sys_pollset_modify(set: usize, fd: usize, events: u32, token: u64) -> Result<(), ()> {
    let ret = unsafe { syscall5(SYS_POLLSET_CTL, set as u64, 1, fd as u64, events as u64, token) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Stop watching a descriptor.
pub fn sys_pollset_remove(set: usize, fd: usize) -> Result<(), ()> {
    let ret = unsafe { syscall5(SYS_POLLSET_CTL, set as u64, 2, fd as u64, 0, 0) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Why a set would not add, modify or remove a watch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PollRefused {
    /// Not a set of this program's, or the set itself to be watched.
    NotOne,
    /// Watched already.
    Exists,
    /// Not watched.
    Absent,
    /// Can never be ready.
    Cannot,
    /// A set that leads back to this one, or a chain too deep.
    Loop,
    /// The set is full.
    Full,
}

/// [`sys_pollset_add`] (op 0), [`sys_pollset_modify`] (1) or
/// [`sys_pollset_remove`] (2), told why not.
pub fn sys_pollset_ctl(set: usize, op: u64, fd: usize, events: u32, token: u64) -> Result<(), PollRefused> {
    let ret = unsafe { syscall5(SYS_POLLSET_CTL, set as u64, op | 1 << 8, fd as u64, events as u64, token) };
    match ret {
        0 => Ok(()),
        2 => Err(PollRefused::Exists),
        3 => Err(PollRefused::Absent),
        4 => Err(PollRefused::Cannot),
        5 => Err(PollRefused::Loop),
        6 => Err(PollRefused::Full),
        _ => Err(PollRefused::NotOne),
    }
}

/// Wait until something in the set is ready, or `timeout` passes: a span,
/// ticks or nanoseconds from [`ns`].
///
/// Returns how many entries of `out` were filled; 0 means it timed out.
pub fn sys_pollset_wait(set: usize, out: &mut [Ready], timeout: u64) -> Result<usize, ()> {
    let ret = unsafe {
        syscall4(
            SYS_POLLSET_WAIT,
            set as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
            timeout,
        )
    };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Write to a stream, optionally handing the peer one of our descriptors.
///
/// Passing needs no authority over the peer: it takes delivery by calling
/// [`sys_fd_recv`]. That is the difference from [`sys_fd_dup`], which puts a
/// descriptor into a task that did not ask and therefore needs `TaskMgmt`.
pub fn sys_fd_send(fd: usize, buf: &[u8], pass: Option<usize>) -> Result<usize, ()> {
    let p = match pass { Some(f) => f as u64, None => u64::MAX };
    let ret = unsafe {
        syscall4(SYS_FD_SEND, fd as u64, buf.as_ptr() as u64, buf.len() as u64, p)
    };
    if ret == u64::MAX { Err(()) } else { Ok((ret & 0xFFFF_FFFF) as usize) }
}

/// `at` for [`sys_fd_recv`] when any free descriptor will do.
pub const ANY_FD: usize = usize::MAX - 1;

/// Ask the kernel not to park: `MSG_DONTWAIT`.
const FD_DONTWAIT: u64 = 1;

/// Send without parking. `Ok(None)` means the stream is full and nothing was
/// written — the peer is not reading, which is its problem and not a reason
/// for this task to stop.
pub fn sys_fd_send_nb(fd: usize, buf: &[u8], pass: Option<usize>) -> Result<Option<usize>, ()> {
    let p = match pass { Some(f) => f as u64, None => u64::MAX };
    let ret = unsafe {
        syscall5(
            SYS_FD_SEND, fd as u64, buf.as_ptr() as u64, buf.len() as u64, p, FD_DONTWAIT,
        )
    };
    match ret {
        u64::MAX => Err(()),
        WOULD_BLOCK => Ok(None),
        n => Ok(Some((n & 0xFFFF_FFFF) as usize)),
    }
}

/// Receive without parking. `Ok(None)` means nothing had arrived.
///
/// A reader that loops until there is nothing left — which is what libwayland
/// does — needs this: the last call of every such loop is the one that finds
/// the stream empty, and if it parks there the loop never ends.
pub fn sys_fd_recv_nb(
    fd: usize,
    buf: &mut [u8],
    at: Option<usize>,
) -> Result<Option<(usize, Option<usize>)>, ()> {
    let a = match at { Some(f) => f as u64, None => u64::MAX };
    let ret = unsafe {
        syscall5(
            SYS_FD_RECV, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64, a, FD_DONTWAIT,
        )
    };
    match ret {
        u64::MAX => Err(()),
        WOULD_BLOCK => Ok(None),
        r => {
            let got = (r >> 32) as usize;
            Ok(Some((
                (r & 0xFFFF_FFFF) as usize,
                if got == 0 { None } else { Some(got - 1) },
            )))
        }
    }
}

/// Read from a stream. If `at` is given and a descriptor was attached, it is
/// installed there — or at any free slot if `at` is [`ANY_FD`].
///
/// Returns the byte count, and which descriptor arrived if one did.
/// Send `buf` down stream `fd` with `fds` passed along — all of them or
/// none, at most 32 — queued before the bytes. How many bytes went.
pub fn sys_fd_send_many(fd: usize, buf: &[u8], fds: &[u32]) -> Result<usize, ()> {
    let flags = 2 | (fds.len().min(255) as u64) << 8;
    match unsafe { syscall5(SYS_FD_SEND, fd as u64, buf.as_ptr() as u64, buf.len() as u64, fds.as_ptr() as u64, flags) } {
        u64::MAX => Err(()),
        n => Ok((n & 0xFFFF_FFFF) as usize),
    }
}

/// Receive into `buf` from stream `fd`, and as many descriptors as were sent
/// and `fds` has room for, each installed in the lowest free slot from 3:
/// how many bytes, and how many descriptors `fds` now begins with.
pub fn sys_fd_recv_many(fd: usize, buf: &mut [u8], fds: &mut [u32]) -> Result<(usize, usize), ()> {
    let flags = 2 | (fds.len().min(255) as u64) << 8;
    match unsafe {
        syscall5(SYS_FD_RECV, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64, fds.as_mut_ptr() as u64, flags)
    } {
        u64::MAX => Err(()),
        r => Ok(((r & 0xFFFF_FFFF) as usize, (r >> 32) as usize)),
    }
}

/// A local socket that is nothing yet: to be named and listened on, or
/// connected by a name — which a file server does (`vfs::bind_local`).
pub fn sys_socket_local() -> Result<usize, ()> {
    match unsafe { syscall1(SYS_SOCKET, 0) } {
        u64::MAX => Err(()),
        fd => Ok(fd as usize),
    }
}

/// Why a server's [`sys_socket_bind`] named nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotNamed {
    /// Another socket has the name.
    Taken,
    /// Not a task calling the caller, or not a socket that is nothing yet.
    NotOne,
}

/// A server names a local socket that `client`, which is calling it, holds
/// as `fd`: by `key`, a number of the server's.
pub fn sys_socket_bind(client: usize, fd: usize, key: u64) -> Result<(), NotNamed> {
    match unsafe { syscall3(SYS_SOCKET_BIND, client as u64, fd as u64, key) } {
        0 => Ok(()),
        1 => Err(NotNamed::Taken),
        _ => Err(NotNamed::NotOne),
    }
}

/// Why a server's [`sys_socket_connect`] connected nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotConnected {
    /// Nothing listens at the name.
    Nobody,
    /// What does has as many waiting as it has room for.
    Full,
    /// Not a task calling the caller, or not a socket that is nothing yet.
    NotOne,
}

/// A server connects a local socket that `client`, which is calling it,
/// holds as `fd`, to whatever listens at `key`.
pub fn sys_socket_connect(client: usize, fd: usize, key: u64) -> Result<(), NotConnected> {
    match unsafe { syscall3(SYS_SOCKET_CONNECT, client as u64, fd as u64, key) } {
        0 => Ok(()),
        1 => Err(NotConnected::Nobody),
        0xFFFF_FFFE => Err(NotConnected::Full),
        _ => Err(NotConnected::NotOne),
    }
}

/// Named socket `fd` listens, with room for `backlog` connections waiting
/// (at most 16).
pub fn sys_socket_listen(fd: usize, backlog: usize) -> Result<(), ()> {
    match unsafe { syscall2(SYS_SOCKET_LISTEN, fd as u64, backlog as u64) } {
        0 => Ok(()),
        _ => Err(()),
    }
}

/// Take a connection waiting on listener `fd`, waiting for one unless
/// `wait` is false: its descriptor. `Ok(None)` if there was none and it was
/// not to wait.
pub fn sys_socket_accept(fd: usize, wait: bool) -> Result<Option<usize>, ()> {
    match unsafe { syscall2(SYS_SOCKET_ACCEPT, fd as u64, if wait { 0 } else { 1 }) } {
        0xFFFF_FFFE => Ok(None),
        n if n < 0xFFFF_FFFD => Ok(Some(n as usize)),
        _ => Err(()),
    }
}

/// Who is at the other end of stream `fd`: process id, user, group.
pub fn sys_socket_peer(fd: usize) -> Result<(u32, u32, u32), ()> {
    let mut who = [0u32; 3];
    match unsafe { syscall2(SYS_SOCKET_PEER, fd as u64, who.as_mut_ptr() as u64) } {
        0 => Ok((who[0], who[1], who[2])),
        _ => Err(()),
    }
}

/// Whether socket `fd` asks to be told who sent what it receives; with
/// `set`, that it does or does not. What it was.
pub fn sys_socket_passcred(fd: usize, set: Option<bool>) -> Result<bool, ()> {
    let how = match set {
        Some(on) => on as u64,
        None => u64::MAX,
    };
    match unsafe { syscall3(SYS_SOCKET_OPTION, fd as u64, 0, how) } {
        u64::MAX => Err(()),
        was => Ok(was != 0),
    }
}

pub fn sys_fd_recv(
    fd: usize,
    buf: &mut [u8],
    at: Option<usize>,
) -> Result<(usize, Option<usize>), ()> {
    let a = match at { Some(f) => f as u64, None => u64::MAX };
    let ret = unsafe {
        syscall4(SYS_FD_RECV, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64, a)
    };
    if ret == u64::MAX {
        Err(())
    } else {
        let got = (ret >> 32) as usize;
        Ok((
            (ret & 0xFFFF_FFFF) as usize,
            if got == 0 { None } else { Some(got - 1) },
        ))
    }
}

/// Register a word to clear and wake when this task exits.
///
/// Linux calls this `set_tid_address`, and its clone flag
/// `CLONE_CHILD_CLEARTID`. musl does not treat it as optional: its
/// `pthread_exit` takes the thread-list lock and never unlocks it, because the
/// lock is this word and the kernel releasing it is what publishes the
/// thread's removal from the list.
///
/// A thread with such a word is joined through it and is nobody's child: no
/// `sys_wait` answers with it or counts it, and the kernel collects it. A
/// creator says so for a thread before starting it — the call takes the
/// task as a second argument — and this form, for the caller, is what a
/// program's first task and a forked child use.
pub fn sys_set_clear_tid(addr: *const u32) -> usize {
    unsafe { syscall1(SYS_SET_CLEAR_TID, addr as u64) as usize }
}

/// A connected pair of byte streams, both ends in this task's table.
///
/// Either end may be moved into another task with `sys_fd_dup`; an end is
/// reference counted, so the mover closing its own copy afterwards does not
/// tell the peer the connection has gone.
pub fn sys_socketpair() -> Result<(usize, usize), ()> {
    let ret = unsafe { syscall0(SYS_SOCKETPAIR) };
    if ret == u64::MAX {
        Err(())
    } else {
        Ok(((ret >> 32) as usize, (ret & 0xFFFF_FFFF) as usize))
    }
}

/// Allocate `pages` of shareable memory and name it with a descriptor.
///
/// The same region `sys_shmem_create` makes, but reachable as a descriptor —
/// which is what lets it be passed over a stream, inherited across a spawn, or
/// closed. `wl_shm` is a client doing exactly this and handing the result to a
/// compositor.
pub fn sys_memfd_create(pages: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall1(SYS_MEMFD_CREATE, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Give memory named by a descriptor a new size, and say what it became.
///
/// Only before anybody has mapped it and before the descriptor has been sent
/// anywhere: growing a region is changing what is behind a live mapping, and
/// nothing here would tell the holder of that mapping it had happened. That
/// window is exactly how a libc uses `ftruncate` on a fresh `memfd`.
pub fn sys_memfd_truncate(fd: usize, bytes: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_MEMFD_TRUNCATE, fd as u64, bytes as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Map the memory a descriptor names, and say how much of it there was.
///
/// The size is the point of the return value. A task that received the
/// descriptor over a stream has no other way to learn it, and the sender's
/// claim about it is the one thing it must not believe.
pub fn sys_mmap_fd(fd: usize, vaddr: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_MMAP_FD, fd as u64, vaddr as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// A terminal's settings, in Linux's layout: four flag words, a line
/// discipline byte and nineteen control characters. The kernel acts on a
/// handful of the bits and stores the rest.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 19],
}

/// Make a pseudo-terminal and return a descriptor for its master: what a
/// terminal emulator holds. What is written to it is typing; what is read
/// from it is what the program in the terminal printed.
pub fn sys_pty_create() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_PTY_CREATE) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// A descriptor for pty `number`'s slave: what the program in the terminal
/// holds as its standard input, output and error.
pub fn sys_pty_open(number: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall1(SYS_PTY_OPEN, number as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Which pty a descriptor for either end names.
pub fn sys_pty_number(fd: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall3(SYS_PTY_CTL, fd as u64, 4, 0) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

pub fn sys_pty_get_termios(fd: usize) -> Result<Termios, ()> {
    let mut t = Termios { c_iflag: 0, c_oflag: 0, c_cflag: 0, c_lflag: 0, c_line: 0, c_cc: [0; 19] };
    let ret = unsafe { syscall3(SYS_PTY_CTL, fd as u64, 0, &mut t as *mut Termios as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(t) }
}

pub fn sys_pty_set_termios(fd: usize, t: &Termios) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_PTY_CTL, fd as u64, 1, t as *const Termios as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// How big the terminal is, in characters: `(rows, columns)`.
pub fn sys_pty_size(fd: usize) -> Option<(u16, u16)> {
    let mut size = [0u16; 4];
    let ret = unsafe { syscall3(SYS_PTY_CTL, fd as u64, 2, size.as_mut_ptr() as u64) };
    (ret != u64::MAX).then_some((size[0], size[1]))
}

/// Say how big the terminal is, in characters. Stored, and handed to
/// whichever program asks.
pub fn sys_pty_set_size(fd: usize, rows: u16, cols: u16) -> Result<(), ()> {
    let size = [rows, cols, 0u16, 0u16];
    let ret = unsafe { syscall3(SYS_PTY_CTL, fd as u64, 3, size.as_ptr() as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Give the task calling this one a descriptor for one of this server's
/// objects, and say which number it got.
///
/// `at` is a free descriptor number, [`ANY_FD`] for the lowest free from 3, or
/// [`FD_CWD`] to make the object the client's working directory. `cookie` is
/// the server's own name for the object, and comes back in every question
/// about it. Refused unless `client` is in a call to this task: that call is
/// its consent.
pub fn sys_fd_serve(client: usize, cookie: u64, at: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall3(SYS_FD_SERVE, client as u64, cookie, at as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// [`sys_fd_serve`], for an object that is not a file: what is read from it
/// comes when it comes, and this server will say when it is ready
/// ([`sys_fd_ready`]). Until it does, a poll finds it ready for nothing.
pub fn sys_fd_serve_ready(client: usize, cookie: u64, at: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall4(SYS_FD_SERVE, client as u64, cookie, at as u64, 1) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// What object `cookie` — this server's, made with [`sys_fd_serve_ready`] —
/// is ready for, in a poll's bits: [`FD_READY_READ`], [`FD_READY_WRITE`],
/// [`FD_READY_HANGUP`]. Whoever polls it is told.
pub fn sys_fd_ready(cookie: u64, bits: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_FD_READY, cookie, bits as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

pub const FD_READY_READ: u32 = 1;
pub const FD_READY_WRITE: u32 = 2;
pub const FD_READY_HANGUP: u32 = 4;

/// In `data[2]` of a read or a write the kernel makes through a served
/// descriptor (`TAG_FD_READ`, `TAG_FD_WRITE`): the task may not wait.
pub const FD_IO_DO_NOT_WAIT: u64 = 1;
/// A server's count for such a read or write when it has nothing yet: the
/// task is told it would block.
pub const FD_IO_NOTHING_YET: u64 = 0xFFFF_FFFE;

/// What giving a client an end of a named pipe came to.
pub enum PipeEnd {
    /// The client's descriptor, and what it should wait on: 0 if the other
    /// end is held, and otherwise a number for [`sys_pipe_peer`].
    Given(usize, u64),
    /// It was to be given only if the other end is held, and it is not.
    NoPeer,
    /// The client is not calling, or a table is full.
    Failed,
}

/// Give `client`, which must be in a call to this task, one end of the pipe
/// that `key` names: the same pipe for everybody given the same key, for as
/// long as any of them holds an end. `only_with_peer` refuses instead of
/// giving an end whose other end nobody holds.
pub fn sys_fd_serve_pipe(client: usize, key: u64, write: bool, only_with_peer: bool) -> PipeEnd {
    let how = write as u64 | (only_with_peer as u64) << 1;
    match unsafe { syscall3(SYS_FD_SERVE_PIPE, client as u64, key, how) } {
        u64::MAX => PipeEnd::Failed,
        WOULD_BLOCK => PipeEnd::NoPeer,
        ret => PipeEnd::Given((ret & 0xFFFF_FFFF) as usize, ret >> 32),
    }
}

/// Wait until the other end of the named pipe `fd` is an end of has been
/// opened, if it has not been since `wait` was given with the descriptor.
/// `Err(true)` if a signal the program handles ended the wait.
pub fn sys_pipe_peer(fd: usize, wait: u64) -> Result<(), bool> {
    match unsafe { syscall2(SYS_PIPE_PEER, fd as u64, wait) } {
        0 => Ok(()),
        INTERRUPTED => Err(true),
        _ => Err(false),
    }
}

/// Which server one of this program's descriptors is an object of, and the
/// server's cookie for it. `Err` if the descriptor names something the kernel
/// keeps itself, or its server has gone.
pub fn sys_fd_served(fd: usize) -> Result<(usize, u64), ()> {
    let mut out = [0u64; 2];
    let ret = unsafe { syscall2(SYS_FD_SERVED, fd as u64, out.as_mut_ptr() as u64) };
    if ret == u64::MAX { Err(()) } else { Ok((out[0] as usize, out[1])) }
}

/// Whether `tid`'s program holds a descriptor for this server's `cookie`.
/// The check a server makes before it acts on a cookie somebody names.
pub fn sys_fd_holds(tid: usize, cookie: u64) -> bool {
    unsafe { syscall2(SYS_FD_HOLDS, tid as u64, cookie) == 1 }
}

/// This server's cookie at descriptor `fd` of `tid`'s program, if what is
/// there is one of this server's objects.
pub fn sys_fd_cookie(tid: usize, fd: usize) -> Option<u64> {
    let ret = unsafe { syscall2(SYS_FD_COOKIE, tid as u64, fd as u64) };
    (ret != u64::MAX).then_some(ret)
}

/// Collect one of this server's objects that no descriptor names any more.
/// Called until it says `None`, after the kernel's
/// [`crate::ipc::TAG_FD_RELEASED`].
pub fn sys_fd_reap() -> Option<u64> {
    let ret = unsafe { syscall0(SYS_FD_REAP) };
    (ret != u64::MAX).then_some(ret)
}

/// Collect child `tid` when it has ended, and nobody else: `(tid, status)`.
/// `Err` if it is not a child of this task.
pub fn sys_wait_for(tid: usize) -> Result<(usize, i32), ()> {
    let ret = unsafe { syscall2(SYS_WAIT_FOR, tid as u64, 0) };
    if ret == u64::MAX {
        return Err(());
    }
    Ok(((ret & 0xFFFF_FFFF) as usize, (ret >> 32) as i32))
}

/// A child that has ended, if one has: `Ok(None)` when there are children and
/// none has ended yet, `Err` when there are none. `tid` 0 is any child.
pub fn sys_wait_nowait(tid: usize) -> Result<Option<(usize, i32)>, ()> {
    match unsafe { syscall2(SYS_WAIT_FOR, tid as u64, 1) } {
        u64::MAX => Err(()),
        0 => Ok(None),
        ret => Ok(Some(((ret & 0xFFFF_FFFF) as usize, (ret >> 32) as i32))),
    }
}

// Signals: Unix's, said to a program. Not the three task signals
// (`SIG_INT` and its neighbours, with `sys_signal`), which are bits in one
// task's notification word and older than these.
pub const SIGHUP: u64 = 1;
pub const SIGINT: u64 = 2;
pub const SIGQUIT: u64 = 3;
pub const SIGKILL: u64 = 9;
pub const SIGALRM: u64 = 14;
pub const SIGTERM: u64 = 15;
pub const SIGCHLD: u64 = 17;
/// The five a job is stopped and started with: start again; stop, which
/// nothing can refuse; stop, typed at a terminal; and stop for reading a
/// terminal, or for changing it, from behind.
pub const SIGCONT: u64 = 18;
pub const SIGSTOP: u64 = 19;
pub const SIGTSTP: u64 = 20;
pub const SIGTTIN: u64 = 21;
pub const SIGTTOU: u64 = 22;
/// A terminal's size has changed: for whoever is in front of it, and
/// nothing to a program that has not asked.
pub const SIGWINCH: u64 = 28;

/// What a program does about a signal: what the signal does, nothing, or run
/// a handler of its own.
pub const SIG_DEFAULT: u64 = 0;
pub const SIG_IGNORE: u64 = 1;
pub const SIG_HANDLE: u64 = 2;

/// What a read of a terminal or a poll answers when a signal this program
/// handles arrived instead of what it was waiting for.
pub const INTERRUPTED: u64 = 0xFFFF_FFFD;
/// The same answer from [`sys_recv_timeout`] on the caller's own id — a
/// sleep — where 1 is the time running out.
pub const SLEEP_INTERRUPTED: u64 = 2;

/// Say what this program does about signal `signo`, and learn what it did.
///
/// A program that handles one is told in three ways, and runs the handler
/// itself: the word it gave [`sys_sig_take`] is set, the wait it was in ends
/// early with [`INTERRUPTED`], and `sys_sig_take` returns the signal.
pub fn sys_sig_action(signo: u64, what: u64) -> Result<u64, ()> {
    let ret = unsafe { syscall2(SYS_SIG_ACTION, signo, what) };
    if ret == u64::MAX { Err(()) } else { Ok(ret) }
}

/// What this program does about `signo`, left as it is.
pub fn sys_sig_action_get(signo: u64) -> Result<u64, ()> {
    sys_sig_action(signo, u64::MAX)
}

/// What [`sys_sig_action`] answers for a signal with a handler the kernel
/// runs ([`sys_sig_handle`]).
pub const SIG_RUN: u64 = 3;
/// How such a handler is run: with its own signal not held back; once,
/// after which the signal is as if nothing had been said; on the stack the
/// task named for the purpose ([`sys_sig_stack`]).
pub const SIG_NODEFER: u64 = 1;
pub const SIG_RESETHAND: u64 = 2;
pub const SIG_ONSTACK: u64 = 4;
/// And what it cuts short is to be made again: a program that said it
/// wants Unix's answers ([`sys_sig_enter_at`]) is answered [`RESTART`].
pub const SIG_RESTARTS: u64 = 8;

/// What a call a signal cut short answers a program that has said it wants
/// to know, besides [`INTERRUPTED`]: a handler ran that asked for the call to
/// be made again; or nothing was run here, and the call is simply made
/// again.
pub const RESTART: u64 = 0xFFFF_FFFC;
pub const AGAIN: u64 = 0xFFFF_FFFB;

/// Raise `signo` for task `tid` and no other: its handler runs there, and
/// while that task holds it back it waits there.
pub fn sys_sig_raise_thread(tid: usize, signo: u64) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_SIG_RAISE, tid as u64, signo, 4) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// What is waiting for this task that it holds back, as a set.
pub fn sys_sig_pending() -> u64 {
    unsafe { syscall2(SYS_SIG_MASK, 4, 0) }
}

/// What came with a signal: Linux's `si_code`, who raised it — a process id
/// in the low half and its user in the high (for SIGCHLD the child's) — and
/// what it carried: the value it was queued with, a child's status, a
/// fault's address. The end of a handler's [`crate::signal::Frame`], and
/// what [`sys_sig_wait_info`] answers with.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SigInfo {
    pub code: i64,
    pub who: u64,
    pub value: u64,
}

/// [`SigInfo::code`]: raised with `SYS_SIG_RAISE`, the same for one task,
/// with [`sys_sig_queue`], by the kernel; and SIGCHLD's — the child exited,
/// a signal ended it, it stopped, it was continued.
pub const SI_USER: i64 = 0;
pub const SI_TKILL: i64 = -6;
pub const SI_QUEUE: i64 = -1;
pub const SI_KERNEL: i64 = 0x80;
pub const CLD_EXITED: i64 = 1;
pub const CLD_KILLED: i64 = 2;
pub const CLD_STOPPED: i64 = 5;
pub const CLD_CONTINUED: i64 = 6;

/// Why [`sys_sig_queue`] raised nothing: nobody to raise it for, or a
/// real-time signal with as many of it waiting as can.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotQueued {
    Nobody,
    Full,
}

fn queued(ret: u64) -> Result<(), NotQueued> {
    match ret {
        0 => Ok(()),
        0xFFFF_FFFE => Err(NotQueued::Full),
        _ => Err(NotQueued::Nobody),
    }
}

/// Raise `signo` for the program `tid` is a task of, carrying `value`. A
/// real-time signal (32 and up) raised while one of its number is waiting
/// waits behind it, rather than being the same one again.
pub fn sys_sig_queue(tid: usize, signo: u64, value: u64) -> Result<(), NotQueued> {
    queued(unsafe { syscall4(SYS_SIG_QUEUE, tid as u64, signo, value, 0) })
}

/// The same, for the program whose process id is `pid`.
pub fn sys_sig_queue_pid(pid: u64, signo: u64, value: u64) -> Result<(), NotQueued> {
    queued(unsafe { syscall4(SYS_SIG_QUEUE, pid, signo, value, 1) })
}

/// The same, for task `tid` and no other.
pub fn sys_sig_queue_thread(tid: usize, signo: u64, value: u64) -> Result<(), NotQueued> {
    queued(unsafe { syscall4(SYS_SIG_QUEUE, tid as u64, signo, value, 4) })
}

/// A descriptor read for the signals in `mask` (bit `n - 1` for signal
/// `n`): a read takes those waiting for the reader — its own task's first,
/// then its program's — as 128-byte records, Linux's `signalfd_siginfo`.
/// A program holds back what it reads for.
pub fn sys_signal_fd(mask: u64) -> Result<usize, ()> {
    match unsafe { syscall2(SYS_SIGNAL_FD, u64::MAX, mask) } {
        u64::MAX => Err(()),
        fd => Ok(fd as usize),
    }
}

/// Signal descriptor `fd` is read for `mask` from now on, through every
/// descriptor for it.
pub fn sys_signal_fd_change(fd: usize, mask: u64) -> Result<(), ()> {
    match unsafe { syscall2(SYS_SIGNAL_FD, fd as u64, mask) } {
        u64::MAX => Err(()),
        _ => Ok(()),
    }
}

/// [`sys_sig_wait_for`], answering with everything that came with the
/// signal.
pub fn sys_sig_wait_info(set: u64, span: u64) -> Result<Option<(u64, SigInfo)>, ()> {
    let mut info = SigInfo::default();
    match unsafe { syscall4(SYS_SIG_WAIT, set, span, &mut info as *mut SigInfo as u64, 1) } {
        0 => Ok(None),
        n if n <= 64 => Ok(Some((n, info))),
        _ => Err(()),
    }
}

/// Take one of `set` that is waiting, or that arrives within `span` (ticks,
/// or nanoseconds with the top bit set; 0 not to wait, `u64::MAX` for ever),
/// without its handler running: the signal and who raised it — a process id
/// with the top bit set when it was a program. `None` if the time ran out;
/// `Err` if another signal's handler ended the wait.
pub fn sys_sig_wait_for(set: u64, span: u64) -> Result<Option<(u64, u64)>, ()> {
    let mut who = 0u64;
    match unsafe { syscall3(SYS_SIG_WAIT, set, span, &mut who as *mut u64 as u64) } {
        0 => Ok(None),
        n if n <= 64 => Ok(Some((n, who))),
        _ => Err(()),
    }
}

/// Have the kernel run a handler for `signo`: when the signal is raised, a
/// task of this program that is not holding it back is turned aside on its
/// way out of the kernel — within a tick, whatever it was doing — and goes
/// on where [`sys_sig_enter_at`] said, with RDI pointing at a record of
/// where it was (`signal::Frame`), which it gives to `SYS_SIG_RETURN` when it
/// has done. `mask` is held back besides the signal itself while that runs;
/// `flags` says how it is run in its low byte and is the program's own
/// above it, handed back in the frame; and so is `cookie`, the program's
/// word for which handler this is. Answers with what the program said about
/// the signal before.
///
/// `signal::handle` is this with the entry and the return written.
pub fn sys_sig_handle(signo: u64, mask: u64, flags: u64, cookie: u64) -> Result<u64, ()> {
    let ret = unsafe { syscall5(SYS_SIG_ACTION, signo, SIG_RUN, mask, flags, cookie) };
    if ret == u64::MAX { Err(()) } else { Ok(ret) }
}

/// Say where this program's handlers are entered — one place for every
/// signal — and, with `unix`, that a call a signal cuts short is to answer
/// as Unix would have it ([`RESTART`], [`AGAIN`]).
pub fn sys_sig_enter_at(entry: usize, unix: bool) -> Result<(), ()> {
    let ret = unsafe { syscall5(SYS_SIG_ACTION, 0, SIG_RUN, 0, unix as u64, entry as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// What [`sys_sig_mask`] is asked to do with the signals it is given.
pub const SIG_BLOCK: u64 = 0;
pub const SIG_UNBLOCK: u64 = 1;
pub const SIG_SETMASK: u64 = 2;

/// Change which signals the calling task holds back — bit `n - 1` for
/// signal `n` — and learn which it held back before. A signal held back by
/// every task of a program waits, whatever it would have done; it is this
/// task's own, a new thread begins with its maker's, a forked child with
/// its parent's, and `exec` keeps it.
pub fn sys_sig_mask(how: u64, set: u64) -> u64 {
    unsafe { syscall2(SYS_SIG_MASK, how, set) }
}

/// What the calling task holds back.
pub fn sys_sig_mask_get() -> u64 {
    unsafe { syscall2(SYS_SIG_MASK, u64::MAX, 0) }
}

/// Hold back exactly `set` and wait for a signal that is then not held
/// back; what was held back before is again when the wait is over.
pub fn sys_sig_wait(set: u64) {
    let _ = unsafe { syscall2(SYS_SIG_MASK, 3, set) };
}

/// Name a stack for handlers that ask to be run on one (`SIG_ONSTACK`):
/// `size` bytes from `base`, or none with a size of 0. The calling task's.
pub fn sys_sig_stack(base: usize, size: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SIG_STACK, base as u64, size as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Raise `signo` for the program `tid` is a task of. `signo` 0 raises
/// nothing and says whether one could be.
pub fn sys_sig_raise(tid: usize, signo: u64) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SIG_RAISE, tid as u64, signo) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

/// The signals raised for this program that it handles, as a mask — bit
/// `n - 1` for signal `n` — none of which is waiting afterwards. `word`, if
/// given, is where the kernel writes 1 when the next arrives.
pub fn sys_sig_take(word: Option<&'static core::sync::atomic::AtomicU32>) -> u64 {
    let at = word.map_or(0, |w| w as *const _ as u64);
    match unsafe { syscall1(SYS_SIG_TAKE, at) } {
        u64::MAX => 0,
        mask => mask,
    }
}

/// The process id of the program `tid` belongs to: the number of the task it
/// began as, which no other task is ever given. A task id is a slot, and the
/// next task made is usually given the one just let go; this is what to
/// remember a program by. Task 0 is the idle task and is in no program:
/// asked about 0, the kernel answers for the caller.
pub fn sys_pid(tid: usize) -> Option<u64> {
    match unsafe { syscall1(SYS_PID, tid as u64) } {
        u64::MAX => None,
        pid => Some(pid),
    }
}

/// Task `tid`'s own number, never given to another: its program's process id
/// if it is the task the program began as, and no process id otherwise.
pub fn sys_task_number(tid: usize) -> Option<u64> {
    match unsafe { syscall2(SYS_PID, tid as u64, 1) } {
        u64::MAX => None,
        n => Some(n),
    }
}

/// This program's process id.
pub fn sys_pid_self() -> u64 {
    unsafe { syscall1(SYS_PID, 0) }
}

/// Collect the child whose process id is `pid` when it has ended:
/// `(pid, status)`. `Err` if this task has no such child.
pub fn sys_wait_for_pid(pid: u64) -> Result<(u64, i32), ()> {
    let ret = unsafe { syscall2(SYS_WAIT_FOR, pid, 2) };
    if ret == u64::MAX {
        return Err(());
    }
    Ok((ret & 0xFFFF_FFFF, (ret >> 32) as i32))
}

/// Raise `signo` for the program whose process id is `pid`. A program that
/// has ended and not been collected is still there to be named, and the call
/// succeeds; `Err` means the id names nothing, or this may not signal it.
pub fn sys_sig_raise_pid(pid: u64, signo: u64) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_SIG_RAISE, pid, signo, 1) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

// ---------------------------------------------------------------------------
// Jobs: process groups, sessions, and what a shell does with them.
// ---------------------------------------------------------------------------

/// What a call about groups, sessions or a terminal's foreground answers
/// when the rules say no, as distinct from there being nothing of the kind.
pub const NOT_ALLOWED: u64 = u64::MAX - 1;

/// Why a change to a group, a session or a terminal was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No such process, or not this session's terminal.
    NoSuch,
    /// There is, and the rules say no.
    NotAllowed,
    /// The caller was signalled for asking, and may ask again.
    Interrupted,
}

fn refusal(ret: u64) -> Refused {
    match ret {
        NOT_ALLOWED => Refused::NotAllowed,
        INTERRUPTED => Refused::Interrupted,
        _ => Refused::NoSuch,
    }
}

/// The process group of process `pid`; 0 is this program's own.
pub fn sys_getpgid(pid: u64) -> Option<u64> {
    match unsafe { syscall2(SYS_PGROUP, 0, pid) } {
        u64::MAX => None,
        group => Some(group),
    }
}

/// Put process `pid` — this program, or a child of it; 0 is this one — in
/// group `pgid`: a group of the same session, or with 0 a new one of its own.
pub fn sys_setpgid(pid: u64, pgid: u64) -> Result<(), Refused> {
    match unsafe { syscall3(SYS_PGROUP, 1, pid, pgid) } {
        0 => Ok(()),
        ret => Err(refusal(ret)),
    }
}

/// The session of process `pid`; 0 is this program's own.
pub fn sys_getsid(pid: u64) -> Option<u64> {
    match unsafe { syscall2(SYS_PGROUP, 2, pid) } {
        u64::MAX => None,
        session => Some(session),
    }
}

/// Begin a session, and a group in it, both led by this program and named
/// after it. Refused for a program that already leads a group.
pub fn sys_setsid() -> Result<u64, Refused> {
    match unsafe { syscall1(SYS_PGROUP, 3) } {
        ret @ (u64::MAX | NOT_ALLOWED) => Err(refusal(ret)),
        session => Ok(session),
    }
}

/// Raise `signo` for every program in process group `pgid`; 0 is this
/// program's own group.
pub fn sys_sig_raise_group(pgid: u64, signo: u64) -> Result<(), Refused> {
    match unsafe { syscall3(SYS_SIG_RAISE, pgid, signo, 2) } {
        0 => Ok(()),
        ret => Err(refusal(ret)),
    }
}

/// What became of a child, as [`sys_wait_job`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildNews {
    /// It ended, with this status, and has been collected.
    Ended(u64, i32),
    /// A signal stopped it. It is still there.
    Stopped(u64, u8),
    /// It was started again.
    Continued(u64),
}

/// [`sys_wait_job`]: answer now rather than wait.
pub const WAIT_NOW: u64 = 1;
/// Hear of a child that has stopped, and of one that has been continued.
pub const WAIT_STOPPED: u64 = 4;
pub const WAIT_CONTINUED: u64 = 8;
/// What is named is a process group of children: 0 is this program's own.
pub const WAIT_GROUP: u64 = 16;

/// Wait for news of a child, named by process id (0 for any): its ending,
/// and with [`WAIT_STOPPED`] or [`WAIT_CONTINUED`] its stopping or starting
/// too. `Ok(None)` is [`WAIT_NOW`] with nothing to say; `Err` is no such
/// child.
pub fn sys_wait_job(pid: u64, flags: u64) -> Result<Option<ChildNews>, ()> {
    match unsafe { syscall2(SYS_WAIT_FOR, pid, flags | 2) } {
        u64::MAX => Err(()),
        0 => Ok(None),
        ret => {
            let child = ret & 0x7FFF_FFFF;
            let status = (ret >> 32) as i32;
            Ok(Some(if ret & (1 << 31) == 0 {
                ChildNews::Ended(child, status)
            } else if status != 0 {
                ChildNews::Stopped(child, status as u8)
            } else {
                ChildNews::Continued(child)
            }))
        }
    }
}

/// Make the terminal `fd` names the controlling terminal of this program's
/// session, with this program's group in front of it. For a session's
/// leader, of a terminal no session has.
pub fn sys_pty_set_session(fd: usize) -> Result<(), Refused> {
    match unsafe { syscall3(SYS_PTY_CTL, fd as u64, 7, 0) } {
        0 => Ok(()),
        ret => Err(refusal(ret)),
    }
}

/// The session the terminal is the controlling terminal of, if it is this
/// program's own.
pub fn sys_pty_session(fd: usize) -> Option<u64> {
    match unsafe { syscall3(SYS_PTY_CTL, fd as u64, 8, 0) } {
        u64::MAX => None,
        session => Some(session),
    }
}

/// The process group in front of this program's controlling terminal: the
/// one what is typed there is for.
pub fn sys_pty_front(fd: usize) -> Option<u64> {
    match unsafe { syscall3(SYS_PTY_CTL, fd as u64, 6, 0) } {
        u64::MAX => None,
        group => Some(group),
    }
}

/// Put group `pgid` of this session in front of its terminal.
///
/// Asked from behind, this raises SIGTTOU for the asker's own group, which
/// stops it unless it has said otherwise — a job in the background does not
/// bring itself forward. `quietly` asks without that: for whoever is taking
/// the terminal back after the job it was lent to has gone.
pub fn sys_pty_set_front(fd: usize, pgid: u64, quietly: bool) -> Result<(), Refused> {
    let group = pgid | if quietly { 1 << 63 } else { 0 };
    match unsafe { syscall3(SYS_PTY_CTL, fd as u64, 5, group) } {
        0 => Ok(()),
        ret => Err(refusal(ret)),
    }
}

/// Have SIGALRM raised for this program in `ticks` ticks, and again every
/// `every` ticks after that if `every` is not 0. `ticks` of 0 cancels the
/// alarm there is. Answers with how the alarm this replaces stood: the ticks
/// that were left of it, 0 if there was none, and what it repeated at.
///
/// It is the program's, not the task's: one for all its threads, kept by
/// `exec` and not copied by `fork`. A program that has said nothing about
/// SIGALRM is ended by it, which is what the signal does.
pub fn sys_sig_alarm(ticks: u64, every: u64) -> (u64, u64) {
    alarm_answer(unsafe { syscall3(SYS_SIG_ALARM, ticks, every, 0) })
}

/// As [`sys_sig_alarm`], to the nanosecond: the alarm is raised `first`
/// nanoseconds from now and every `every` after that, and the answer is how
/// the one it replaces stood, in nanoseconds.
pub fn sys_sig_alarm_ns(first: u64, every: u64) -> (u64, u64) {
    let mut was = [0u64; 2];
    let every = if every == 0 { 0 } else { ns(every) };
    match unsafe { syscall4(SYS_SIG_ALARM, ns(first), every, 0, was.as_mut_ptr() as u64) } {
        u64::MAX => (0, 0),
        _ => (was[0], was[1]),
    }
}

/// How this program's alarm stands, in nanoseconds — what is left of it and
/// what it repeats at — changing nothing.
pub fn sys_sig_alarm_left_ns() -> (u64, u64) {
    let mut was = [0u64; 2];
    match unsafe { syscall4(SYS_SIG_ALARM, 0, 0, 1, was.as_mut_ptr() as u64) } {
        u64::MAX => (0, 0),
        _ => (was[0], was[1]),
    }
}

/// How this program's alarm stands — the ticks left and what it repeats at —
/// changing nothing.
pub fn sys_sig_alarm_left() -> (u64, u64) {
    alarm_answer(unsafe { syscall3(SYS_SIG_ALARM, 0, 0, 1) })
}

fn alarm_answer(ret: u64) -> (u64, u64) {
    if ret == u64::MAX {
        return (0, 0);
    }
    (ret & 0xFFFF_FFFF, ret >> 32)
}

/// What kind of thing a descriptor names: [`sys_fd_kind`]'s answers.
pub const FD_KIND_ENDPOINT: u64 = 1;
pub const FD_KIND_PIPE_READ: u64 = 2;
pub const FD_KIND_PIPE_WRITE: u64 = 3;
pub const FD_KIND_STREAM: u64 = 4;
pub const FD_KIND_PTY_MASTER: u64 = 5;
pub const FD_KIND_PTY_SLAVE: u64 = 6;
pub const FD_KIND_TIMER: u64 = 7;
pub const FD_KIND_EVENT: u64 = 8;
pub const FD_KIND_POLLSET: u64 = 9;
pub const FD_KIND_MEMORY: u64 = 10;
pub const FD_KIND_SOCKET: u64 = 11;
pub const FD_KIND_SERVED: u64 = 12;

/// What descriptor `fd` names, and whether nothing is left at its other end.
/// `None` if it names nothing.
pub fn sys_fd_kind(fd: usize) -> Option<(u64, bool)> {
    match unsafe { syscall1(SYS_FD_KIND, fd as u64) } {
        u64::MAX => None,
        ret => Some((ret & 0xFF, ret & 0x100 != 0)),
    }
}

/// Set the permission bits this program leaves off a file or a directory it
/// makes, and return what they were. Kept across a fork and an exec.
pub fn sys_umask(mask: u32) -> u32 {
    unsafe { syscall1(SYS_UMASK, (mask & 0o777) as u64) as u32 }
}

/// The bits [`sys_umask`] holds, left as they are.
pub fn sys_umask_get() -> u32 {
    unsafe { syscall1(SYS_UMASK, u64::MAX) as u32 }
}

/// Make a copy of this program: a task of its own, in a copy of this address
/// space, with a second descriptor for everything this one has open. Returns
/// the child's id here and 0 there.
pub fn sys_fork() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_FORK) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Mark a descriptor to be closed when this program becomes another, or
/// take the mark off.
pub fn sys_fd_set_cloexec(fd: usize, on: bool) -> Result<(), ()> {
    let flags = if on { FD_FLAG_CLOEXEC } else { 0 };
    let ret = unsafe { syscall3(SYS_FD_FLAGS, fd as u64, 1, flags) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Whether a descriptor is marked to close when this program becomes another.
pub fn sys_fd_cloexec(fd: usize) -> Result<bool, ()> {
    let ret = unsafe { syscall3(SYS_FD_FLAGS, fd as u64, 0, 0) };
    if ret == u64::MAX { Err(()) } else { Ok(ret & FD_FLAG_CLOEXEC != 0) }
}

/// Release a descriptor.
///
/// The last reader or writer of a pipe closing is what makes the other end see
/// end-of-file, so this is not merely tidiness: without it a pipe's writer can
/// never go away and its reader waits for ever.
pub fn sys_fd_close(fd: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_FD_CLOSE, fd as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Set the memory limit (in pages) for a task. 0 = unlimited.
/// Requires CAP_TASK_MGMT.
pub fn sys_set_mem_limit(tid: usize, limit_pages: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SET_MEM_LIMIT, tid as u64, limit_pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Create a shared memory region. Returns a handle on success.
pub fn sys_shmem_create(pages: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall1(SYS_SHMEM_CREATE, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Map a shared memory region into the caller's address space.
/// vaddr must be page-aligned and in user space (>= 0x80_0000_0000).
pub fn sys_shmem_map(handle: usize, vaddr: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SHMEM_MAP, handle as u64, vaddr as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Grant another task access to a shared memory region.
/// Must be the region's creator or have CAP_TASK_MGMT.
pub fn sys_shmem_grant(handle: usize, target_tid: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SHMEM_GRANT, handle as u64, target_tid as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Unmap a shared memory region from the caller's address space.
pub fn sys_shmem_unmap(handle: usize, vaddr: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SHMEM_UNMAP, handle as u64, vaddr as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Destroy a shared memory region, freeing physical pages and reclaiming the handle.
/// Must be the region's creator or have CAP_TASK_MGMT.
pub fn sys_shmem_destroy(handle: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_SHMEM_DESTROY, handle as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Wait for a child task to exit. Returns `(tid, exit_code)`.
/// Returns Err(()) if the caller has no children.
pub fn sys_wait() -> Result<(usize, i32), ()> {
    let ret = unsafe { syscall0(SYS_WAIT) };
    if ret == u64::MAX {
        Err(())
    } else {
        Ok(((ret & 0xFFFF_FFFF) as usize, (ret >> 32) as u32 as i32))
    }
}

/// Set the pager task for exception forwarding (requires CAP_TASK_MGMT).
/// Page faults in the target task will be forwarded to pager_tid via IPC.
pub fn sys_set_pager(tid: usize, pager_tid: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SET_PAGER, tid as u64, pager_tid as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Send an asynchronous notification to a task.
/// `badge` bits are OR'd into the target's notification word (non-blocking).
/// The target receives a message with tag=TAG_NOTIFICATION and data[0]=accumulated word.
pub fn sys_notify(dest: usize, badge: u64) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_NOTIFY, dest as u64, badge) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// IPC tag for notification messages from the kernel.
/// data[0] = notification word (accumulated OR of all badges since last consume).
pub const TAG_NOTIFICATION: u64 = 0xFFFF_0002;

/// IPC tag for page fault messages from the kernel.
/// data: [fault_addr, error_code, rip, rsp, access_flags, 0]
pub const TAG_PAGE_FAULT: u64 = 0xFFFF_0001;

/// Map anonymous memory into the caller's address space.
/// `vaddr` must be page-aligned and in user space (>= 0x80_0000_0000).
/// Returns 0 on success, u64::MAX on failure.
pub fn sys_mmap(vaddr: usize, pages: usize) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_MMAP, vaddr as u64, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Unmap pages from the caller's address space and free their physical frames.
/// `vaddr` must be page-aligned and in user space (>= 0x80_0000_0000).
/// Returns the number of pages actually freed, or u64::MAX on invalid arguments.
pub fn sys_munmap(vaddr: usize, pages: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_MUNMAP, vaddr as u64, pages as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Transfer capabilities to another task. The caller must hold all bits in `caps`.
/// Unlike sys_grant_cap, this does NOT require CAP_TASK_MGMT.
pub fn sys_cap_transfer(dest: usize, caps: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_CAP_TRANSFER, dest as u64, caps as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Create a kernel pipe. Returns the pipe handle on success.
pub fn sys_pipe_create() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_PIPE_CREATE) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Install a pipe endpoint as a file descriptor on a task.
/// is_write: false = read end, true = write end.
/// Requires CAP_TASK_MGMT.
pub fn sys_pipe_fd_set(tid: usize, fd: usize, pipe_handle: usize, is_write: bool) -> Result<(), ()> {
    let ret = unsafe {
        syscall4(SYS_PIPE_FD_SET, tid as u64, fd as u64, pipe_handle as u64, is_write as u64)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Duplicate the caller's source fd onto a target task's target fd.
/// Handles pipe refcounting automatically. Requires CAP_TASK_MGMT.
pub fn sys_fd_dup(target_tid: usize, target_fd: usize, source_fd: usize) -> Result<(), ()> {
    let ret = unsafe {
        syscall3(SYS_FD_DUP, target_tid as u64, target_fd as u64, source_fd as u64)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// A second name for one of your own descriptors, at the lowest free number at
/// or above `floor`. This is `dup`, and it needs no authority: a second name
/// for something already held is not more of anything.
pub fn sys_fd_dup_self(source_fd: usize, floor: usize) -> Result<usize, ()> {
    let me = sys_getpid();
    let ret = unsafe {
        syscall4(SYS_FD_DUP, me as u64, ANY_FD as u64, source_fd as u64, floor as u64)
    };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

// Signal constants (badge bits, high to avoid collision with app notifications)
pub const SIG_INT: u64 = 1 << 16;
pub const SIG_TERM: u64 = 1 << 17;
pub const SIG_KILL: u64 = 1 << 18;
pub const SIG_MASK: u64 = SIG_INT | SIG_TERM | SIG_KILL;

/// Send a signal to a task. SIG_KILL is immediate; SIG_INT/SIG_TERM give the
/// task a 5-second grace period to handle the signal before being force-killed.
/// Same permissions as sys_task_kill (CAP_TASK_MGMT or same UID).
pub fn sys_signal(tid: usize, sig: u64) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SIGNAL, tid as u64, sig) };
    if ret == 0 { Ok(()) } else { Err(()) }
}

// Object capability syscalls

// Object capability types
pub const CAP_TYPE_IOPORT: u64 = 1;
pub const CAP_TYPE_PHYS_RANGE: u64 = 2;
pub const CAP_TYPE_IRQ: u64 = 3;
pub const CAP_TYPE_TASK_MGMT: u64 = 4;
pub const CAP_TYPE_PHYS_ALLOC: u64 = 5;
pub const CAP_TYPE_SET_UID: u64 = 6;
// 7 named a set of TIDs, and was withdrawn at ABI 2.0.
/// One task this one may sys_send / sys_call / sys_notify. Minted by TID —
/// by that task, its creator, or a holder of one — and recorded as the number
/// of its endpoint, which no other task will ever have.
pub const CAP_TYPE_ENDPOINT: u64 = 8;
pub const CAP_TYPE_MEMOBJECT: u64 = 9;
/// The right to map the registers of the machine's devices: its holder may
/// mint a `PhysRange` over any range that lies wholly in device memory —
/// the addresses the firmware's memory map leaves out — and map with that.
pub const CAP_TYPE_DEVICE_MEMORY: u64 = 10;
/// The right to say what time it is (`sys_clock_set`).
pub const CAP_TYPE_CLOCK: u64 = 11;
/// The right to turn the machine off and to start it again
/// (`sys_power_off`, `sys_restart`).
pub const CAP_TYPE_POWER: u64 = 12;
/// The right to be where memory is written out to: to hold every program's
/// unused pages, and hand them back (`OBJECT_SWAP`).
pub const CAP_TYPE_SWAP: u64 = 13;
/// A PCI device, by its address ([`pci_device`]), or every one of them
/// ([`PCI_ANY`]): its configuration, its BARs, its claim and an interrupt
/// for it by message. The device manager holds every device and gives each
/// driver its own.
pub const CAP_TYPE_PCI_DEVICE: u64 = 14;
/// `PciDevice`'s param0 for every device.
pub const PCI_ANY: u64 = 0xFFFF_FFFF;

/// CSpace slot conventions shared by init, login and the shell.
///
/// Every program starts with a capability to the nameserver here, and a
/// spawner passes its own on with sys_cap_grant. That one is enough: looking a
/// service up is also how a program is given the right to call it.
pub const SLOT_ENDPOINT: usize = 15;
/// A second fixed slot for an endpoint a spawner hands over itself: the
/// compositor puts one to itself here in each program it starts.
pub const SLOT_ENDPOINT_EXTRA: usize = 13;
/// Scratch slot used for the mint-grant-delete idiom.
pub const SLOT_SCRATCH: usize = 14;

/// Mint a new capability in the caller's CSpace slot.
/// The caller must already hold a cap of the same type whose params are a superset.
pub fn sys_cap_mint(slot: usize, cap_type: u64, param0: u64, param1: u64) -> Result<(), ()> {
    let ret = unsafe { syscall4(SYS_CAP_MINT, slot as u64, cap_type, param0, param1) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// One capability slot, as [`sys_cap_read`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapInfo {
    /// One of the `CAP_TYPE_*` numbers, or 0 for an empty slot.
    pub cap_type: u64,
    pub param0: u64,
    pub param1: u64,
    /// False once the capability it was derived from has been revoked.
    pub valid: bool,
}

/// Read slot `slot` of `tid`'s CSpace. A task may read its own; another's
/// needs `TaskMgmt` over it. Fails past the last slot.
pub fn sys_cap_read(tid: usize, slot: usize) -> Result<CapInfo, ()> {
    let mut out = [0u64; 4];
    let ret = unsafe { syscall3(SYS_CAP_READ, tid as u64, slot as u64, out.as_mut_ptr() as u64) };
    if ret == u64::MAX {
        return Err(());
    }
    Ok(CapInfo { cap_type: out[0], param0: out[1], param1: out[2], valid: out[3] != 0 })
}

/// A destination slot meaning "wherever it fits", for [`sys_cap_grant_any`]
/// and [`sys_cap_take_any`]. The kernel picks a slot from 16 up and says which.
pub const ANY_SLOT: usize = usize::MAX - 1;

/// Delegate a capability from src_slot to dest_tid's dest_slot.
pub fn sys_cap_grant(dest_tid: usize, src_slot: usize, dest_slot: usize) -> Result<(), ()> {
    let ret = unsafe { syscall3(SYS_CAP_GRANT, dest_tid as u64, src_slot as u64, dest_slot as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Delegate the capability in `src_slot` to `dest_tid`, wherever it fits, and
/// say where. An `Endpoint` it already holds is not copied again: the answer
/// is the slot that one is in.
pub fn sys_cap_grant_any(dest_tid: usize, src_slot: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall3(SYS_CAP_GRANT, dest_tid as u64, src_slot as u64, ANY_SLOT as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Take the capability `caller` offered with the call being served, into
/// `slot`. Only between receiving that call and answering it, and only once.
pub fn sys_cap_take(caller: usize, slot: usize) -> Result<usize, ()> {
    let ret = unsafe { syscall2(SYS_CAP_TAKE, caller as u64, slot as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// [`sys_cap_take`] into wherever it fits, as [`sys_cap_grant_any`] picks.
pub fn sys_cap_take_any(caller: usize) -> Result<usize, ()> {
    sys_cap_take(caller, ANY_SLOT)
}

/// Revoke a capability slot, invalidating all derived caps.
pub fn sys_cap_revoke(slot: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_CAP_REVOKE, slot as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Inspect a capability slot. Returns packed info:
/// bits [7:0] = type, [23:8] = param0 low 16, [39:24] = param1 low 16.
/// Returns u64::MAX on error.
pub fn sys_cap_inspect(slot: usize) -> Result<(u8, u16, u16), ()> {
    let ret = unsafe { syscall1(SYS_CAP_INSPECT, slot as u64) };
    if ret == u64::MAX { return Err(()); }
    let cap_type = (ret & 0xFF) as u8;
    let param0 = ((ret >> 8) & 0xFFFF) as u16;
    let param1 = ((ret >> 24) & 0xFFFF) as u16;
    Ok((cap_type, param0, param1))
}

/// Delete a capability from the caller's CSpace slot.
pub fn sys_cap_delete(slot: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_CAP_DELETE, slot as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Set the per-user default capability bitmask for a UID.
/// Requires CAP_SET_UID (root gets it automatically via UID bypass).
pub fn sys_set_user_caps(uid: u32, caps: u32) -> Result<(), ()> {
    let ret = unsafe { syscall2(SYS_SET_USER_CAPS, uid as u64, caps as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// Query the per-user default capability bitmask for a UID.
pub fn sys_get_user_caps(uid: u32) -> u32 {
    unsafe { syscall1(SYS_GET_USER_CAPS, uid as u64) as u32 }
}

// Capability bit constants
pub const CAP_IOPORT: u32 = 1 << 0;
pub const CAP_MAP_PHYS: u32 = 1 << 1;
pub const CAP_IRQ: u32 = 1 << 2;
pub const CAP_TASK_MGMT: u32 = 1 << 3;
pub const CAP_PHYS_ALLOC: u32 = 1 << 4;
pub const CAP_SET_UID: u32 = 1 << 5;
pub const CAP_ENDPOINT: u32 = 1 << 6;

/// The ABI this runtime was written against: the kernel's `docs/abi.md` at
/// this version is what the numbers above and the wrappers below were read
/// from.
///
/// It is written down because the kernel is another repository now, built at
/// another time. `tools/check-abi.sh` holds these against the header the
/// kernel installs — the same major, a minor the kernel has reached, and at an
/// equal version exactly the same calls — and `init` holds them against the
/// kernel that is actually running, before it does anything else.
pub const ABI_VERSION_MAJOR: u32 = 3;
pub const ABI_VERSION_MINOR: u32 = 34;

/// Syscall ABI version the running kernel implements, as (major, minor).
///
/// A program that cares should check the major and refuse to run against one
/// it was not built for; minor only ever grows by addition.
pub fn sys_abi_version() -> (u32, u32) {
    let v = unsafe { syscall0(SYS_ABI_VERSION) };
    ((v >> 16) as u32, (v & 0xFFFF) as u32)
}

/// Is the running kernel one this runtime can talk to?
///
/// The same major, and a minor at least the one this was written against: a
/// minor only adds calls, so a newer one still answers every call here, and an
/// older one is missing some. `Ok` and `Err` both carry what the kernel said.
///
/// `SYS_ABI_VERSION` is the one call this may safely make without knowing the
/// answer: its number has not moved since 1.0 and its block holds nothing
/// else.
pub fn abi_check() -> Result<(u32, u32), (u32, u32)> {
    let (major, minor) = sys_abi_version();
    if major == ABI_VERSION_MAJOR && minor >= ABI_VERSION_MINOR {
        Ok((major, minor))
    } else {
        Err((major, minor))
    }
}

/// The address space this task is running in.
///
/// Needed to start a thread, which is a task started with the same cr3. It
/// conveys no authority: the caller is already executing there.
pub fn sys_addrspace_self() -> Result<usize, ()> {
    let ret = unsafe { syscall0(SYS_ADDRSPACE_SELF) };
    if ret == u64::MAX { Err(()) } else { Ok(ret as usize) }
}

/// Set this task's FS base, where its thread-locals live.
///
/// Per task and self-directed, so it needs no capability: a task can already
/// write any of its own memory. Takes effect immediately, not at the next
/// context switch.
pub fn sys_set_fs_base(base: usize) -> Result<(), ()> {
    let ret = unsafe { syscall1(SYS_SET_FS_BASE, base as u64) };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}

/// As [`sys_task_start`], but hands `arg` to the entry point in RDI.
pub fn sys_task_start_arg(
    tid: usize,
    rip: u64,
    rsp: u64,
    cr3: usize,
    arg: u64,
) -> Result<(), ()> {
    let ret = unsafe {
        syscall5(SYS_TASK_START_ARG, tid as u64, rip, rsp, cr3 as u64, arg)
    };
    if ret == u64::MAX { Err(()) } else { Ok(()) }
}
