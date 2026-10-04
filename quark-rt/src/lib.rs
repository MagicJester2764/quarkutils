#![no_std]

#[cfg(not(feature = "rustc-dep-of-std"))]
extern crate alloc;

pub mod accounts;
pub mod args;
pub mod auth;
pub mod block;
pub mod calendar;
pub mod console;
pub mod crypt;
pub mod devices;
pub mod display;
pub mod font;
pub mod ipc;
pub mod keys;
pub mod layout;
pub mod logd;
pub mod manifest;
pub mod session;
pub mod signal;
pub mod spawn;
pub mod thread;
pub mod tls;
pub mod stdio;
pub mod sync;
pub mod wl;
pub mod syscall;
pub mod nameserver;
pub mod net;
pub mod nic;
pub mod pci;
pub mod pcm;
pub mod services;
pub mod socket;
pub mod sound;
pub mod random;
pub mod seat;
pub mod vfs;
pub mod usb;
pub mod virtio;
pub mod wm;

pub mod allocator;

/// Minimal libc compatibility shim for std's os::fd module.
pub mod libc;

/// Flat runtime API for the std PAL to call into.
/// When building as part of std (rustc-dep-of-std), these are the
/// entry points that library/std/src/sys/pal/quark uses.
pub mod rt;

#[cfg(not(feature = "rustc-dep-of-std"))]
use allocator::QuarkAllocator;

#[cfg(not(feature = "rustc-dep-of-std"))]
#[global_allocator]
static ALLOCATOR: QuarkAllocator = QuarkAllocator::new();
