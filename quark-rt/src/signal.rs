/// Signal handling for Quark userspace.
///
/// Signals are delivered as notification badges (high bits) via the kernel's
/// async notification system. Tasks receive them as `TAG_NOTIFICATION` messages
/// via `sys_recv`. Use `extract_signal()` to check if a received notification
/// contains signal bits, then handle gracefully before the kernel's 5-second
/// force-kill deadline expires.

use crate::ipc::Message;
use crate::syscall;

pub use crate::syscall::{SIG_INT, SIG_KILL, SIG_MASK, SIG_TERM};

/// Check if a notification message contains a signal.
/// Returns the signal bits (nonzero if signal present).
///
/// This is the preferred way to detect signals: check every `TAG_NOTIFICATION`
/// message in your event loop via `extract_signal(&msg)`. Only the kernel's
/// count: a program can send the tag, but not as sender 0.
pub fn extract_signal(msg: &Message) -> u64 {
    if msg.sender == 0 && msg.tag == syscall::TAG_NOTIFICATION {
        msg.data[0] & SIG_MASK
    } else {
        0
    }
}

/// Default signal handler: exit the process.
/// Call this when you receive a signal and don't need custom cleanup.
pub fn default_handler(_sig: u64) -> ! {
    syscall::sys_exit();
}

// ---------------------------------------------------------------------------
// Handlers the kernel runs.
//
// The other kind of signal: Unix's, said to a program (`sys_sig_raise`). A
// program is told of one it handles and runs the handler itself when it
// next looks (`sys_sig_take`) — or has the kernel run it, which is what
// this is: the handler interrupts whatever the program was doing.
// ---------------------------------------------------------------------------

/// What the kernel leaves on a task's stack when it runs a handler: why,
/// and everything needed to be where the task was. A handler may change it
/// — the mask to go back to, the registers, where to go on from.
#[repr(C)]
pub struct Frame {
    pub signo: u64,
    /// 0 a program raised it (`value` is its process id), 1 the kernel
    /// did, 2 the task itself did something (`value` is the address).
    pub code: u64,
    pub value: u64,
    /// What the task held back before the handler, and holds back again
    /// when it returns.
    pub mask: u64,
    /// Bit 0: this is on the stack named for handlers.
    pub flags: u64,
    /// The handler, as [`handle`] gave it.
    pub cookie: u64,
    /// RAX RBX RCX RDX RSI RDI RBP R8–R15 RIP RFLAGS RSP.
    pub regs: [u64; 18],
    /// What came with it, of which `code` and `value` say part.
    pub info: crate::syscall::SigInfo,
}

/// Where in [`Frame::regs`] the task was, and its stack.
pub const REG_RIP: usize = 15;
pub const REG_RSP: usize = 17;

pub const BY_PROGRAM: u64 = 0;
pub const BY_KERNEL: u64 = 1;
pub const BY_FAULT: u64 = 2;

/// Whether the kernel has been told where handlers are entered.
static ENTERED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Have `handler` run when `signo` is raised for this program, whatever
/// the program is doing at the time: the kernel turns a task of it aside,
/// the handler runs on that task's stack, and the task goes on from where
/// it was — or from wherever the handler has changed the frame to say.
/// `mask` is held back, besides `signo`, while it runs.
///
/// A wait it ends answers [`syscall::INTERRUPTED`], as a wait a signal ends
/// always has — unless the program has said it wants Unix's answers
/// ([`speak_unix`]).
///
/// It is not saved and restored around the handler: the floating-point
/// registers. A program built for this system's own target uses none.
pub fn handle(signo: u64, handler: fn(&mut Frame), mask: u64, flags: u64) -> Result<u64, ()> {
    if signo == 0 || signo > 64 {
        return Err(());
    }
    if !ENTERED.load(core::sync::atomic::Ordering::SeqCst) {
        syscall::sys_sig_enter_at(entry as *const () as usize, false)?;
        ENTERED.store(true, core::sync::atomic::Ordering::SeqCst);
    }
    syscall::sys_sig_handle(signo, mask, flags, handler as *const () as u64)
}

/// Say that this program wants Unix's answers to a call a signal cuts
/// short: [`syscall::INTERRUPTED`] for a handler that did not ask for it to be
/// made again, [`syscall::RESTART`] for one that did, [`syscall::AGAIN`]
/// when nothing ran.
pub fn speak_unix() -> Result<(), ()> {
    syscall::sys_sig_enter_at(entry as *const () as usize, true)?;
    ENTERED.store(true, core::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Where the kernel enters the program: RDI is the frame, and the stack is
/// as a function finds it. The handler is called, and the frame given back.
#[unsafe(naked)]
extern "C" fn entry() -> ! {
    core::arch::naked_asm!(
        "push rdi",
        "call {run}",
        "pop rdi",
        "mov eax, {ret}",
        "syscall",
        "ud2",
        run = sym run,
        ret = const syscall::SYS_SIG_RETURN,
    );
}

extern "C" fn run(frame: *mut Frame) {
    let frame = unsafe { &mut *frame };
    if frame.cookie != 0 {
        let handler: fn(&mut Frame) = unsafe { core::mem::transmute(frame.cookie as usize) };
        handler(frame);
    }
}
