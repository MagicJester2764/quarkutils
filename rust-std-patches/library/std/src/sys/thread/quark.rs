use crate::ffi::CStr;
use crate::io;
use crate::num::NonZero;
use crate::thread::ThreadInit;
use crate::time::Duration;

pub const DEFAULT_MIN_STACK_SIZE: usize = 1024 * 64;

/// A thread is a task sharing this one's address space. The kernel refcounts
/// address spaces, so the shared one outlives whichever of them exits first.
pub struct Thread(usize);

impl Thread {
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread> {
        let pages = (stack.max(DEFAULT_MIN_STACK_SIZE) + 4095) / 4096;

        // Leaked deliberately: ownership passes to the new thread, which
        // reconstitutes it. Reclaimed here only if the spawn fails.
        let data = Box::into_raw(init);

        extern "C" fn start(arg: usize) -> ! {
            // Thread-locals first. `ThreadInit::init` sets the current thread,
            // which is itself held in one, so reading it before FS is pointed
            // anywhere would fault.
            unsafe {
                // Mapped, not allocated: setting up storage before the thread
                // is fully running should not need the allocator's lock, and
                // an error in the layout arithmetic should hit unmapped memory
                // rather than another allocation's bookkeeping.
                if let Ok((mem, len)) = quark_rt::tls::map_region() {
                    let _ = quark_rt::tls::init_in(mem, len);
                }

                let init = Box::from_raw(arg as *mut ThreadInit);
                let rust_start = init.init();
                rust_start();

                // Exiting the task here skips everything a thread normally
                // does on the way out, and Quark has no hook that runs on the
                // way out either — `guard::enable` is a no-op for this target
                // because std is the whole runtime. So run both by hand.
                //
                // Destructors first: a thread-local's Drop may still reach for
                // the current thread handle, which thread_cleanup drops.
                crate::sys::thread_local::destructors::run();

                // thread_cleanup in particular drops this thread's handle,
                // which is what releases its reference to the join packet.
                // Without it `join` finds the Arc still shared and panics with
                // "threads should not terminate unexpectedly".
                crate::rt::thread_cleanup();
            }
            quark_rt::syscall::sys_exit_code(0);
        }

        match quark_rt::thread::spawn_with_arg(start, data as usize, pages) {
            Ok(t) => Ok(Thread(t.tid())),
            Err(()) => {
                // The spawn did not take ownership, so this is still ours.
                drop(unsafe { Box::from_raw(data) });
                Err(io::Error::UNSUPPORTED_PLATFORM)
            }
        }
    }

    pub fn join(self) {
        // sys_wait reaps whichever child exits first, so loop until it is ours.
        loop {
            match quark_rt::syscall::sys_wait() {
                Ok((tid, _)) if tid == self.0 => return,
                Ok(_) => continue,
                Err(()) => return,
            }
        }
    }
}

pub fn set_name(_name: &CStr) {}

pub fn current_os_id() -> Option<u64> {
    Some(quark_rt::syscall::sys_getpid() as u64)
}

pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    Ok(NonZero::new(1).unwrap())
}

pub fn yield_now() {
    quark_rt::syscall::sys_yield();
}

pub fn sleep(dur: Duration) {
    let ms = dur.as_millis() as u64;
    quark_rt::rt::sleep_ms(ms);
}

pub fn sleep_until(deadline: crate::time::Instant) {
    // No direct absolute-time sleep in Quark; fall back to relative sleep.
    use crate::time::Instant;
    while let Some(delay) = deadline.checked_duration_since(Instant::now()) {
        sleep(delay);
    }
}
