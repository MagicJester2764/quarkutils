//! What the threads of this program share: keys and movement on their way to
//! `input`, what is plugged in, for whoever asks, and each disk's request.
//!
//! The controller is the controller's thread's alone; the others see it
//! through this, and wait on it here.

use quark_rt::sync::{Mutex, Semaphore};
use quark_rt::usb::Device as Listed;

pub const MAX_DISKS: usize = 4;
pub const MAX_LISTED: usize = 32;
/// What a disk's thread notifies the controller's thread with: a request.
pub const TOLD_DISK: u64 = 1 << 0;
/// What `input` is notified with when keys or movement come.
pub const NOTIFY_INPUT: u64 = 2;

#[derive(Clone, Copy, Default)]
pub struct Key {
    pub press: bool,
    pub ascii: u8,
    pub code: u8,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Default)]
pub struct Movement {
    pub dx: i32,
    pub dy: i32,
    pub buttons: u8,
    pub wheel: i32,
}

/// Oldest first, and the newest dropped when it is full.
pub struct Queue<T: Copy + Default, const N: usize> {
    items: [T; N],
    head: usize,
    len: usize,
}

impl<T: Copy + Default, const N: usize> Queue<T, N> {
    pub const fn new(empty: T) -> Self {
        Queue { items: [empty; N], head: 0, len: 0 }
    }

    pub fn push(&mut self, item: T) -> bool {
        if self.len == N {
            return false;
        }
        self.items[(self.head + self.len) % N] = item;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(item)
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            return None;
        }
        Some(&mut self.items[(self.head + self.len - 1) % N])
    }
}

/// A read or a write a disk's thread asks for, of the page its sectors go
/// through; or, with no sectors, that what was written be made lasting.
#[derive(Clone, Copy, Default)]
pub struct Request {
    pub write: bool,
    pub lba: u64,
    pub count: u32,
    pub offset: usize,
}

#[derive(Clone, Copy, Default)]
pub struct Disk {
    /// Plugged in. Taken out, its thread ends at its next request.
    pub present: bool,
    /// Which disk has this place: one more for each. A thread serves the
    /// one it was started for, and a disk plugged in where one was pulled
    /// out, before that one's thread has gone, is not it.
    pub generation: u32,
    pub sectors: u64,
    /// The page its sectors go through, as this program sees it.
    pub data: usize,
    /// The device's slot.
    pub slot: u8,
    pub request: Option<Request>,
    /// How the last request went.
    pub ok: bool,
    /// Its thread.
    pub thread: usize,
}

pub struct Shared {
    pub keys: Queue<Key, 128>,
    pub movements: Queue<Movement, 64>,
    /// The input server, once this program is one of its sources.
    pub input: usize,
    /// The controller's thread, which a disk's thread tells of a request.
    pub main: usize,
    pub listed: [Option<Listed>; MAX_LISTED],
    pub disks: [Disk; MAX_DISKS],
}

pub static SHARED: Mutex<Shared> = Mutex::new(Shared {
    keys: Queue::new(Key { press: false, ascii: 0, code: 0, modifiers: 0 }),
    movements: Queue::new(Movement { dx: 0, dy: 0, buttons: 0, wheel: 0 }),
    input: 0,
    main: 0,
    listed: [None; MAX_LISTED],
    disks: [Disk { present: false, generation: 0, sectors: 0, data: 0, slot: 0, request: None, ok: false, thread: 0 };
        MAX_DISKS],
});

/// Each disk's request, done: what its thread waits on.
pub static DONE: [Semaphore; MAX_DISKS] = [Semaphore::new(0), Semaphore::new(0), Semaphore::new(0), Semaphore::new(0)];
