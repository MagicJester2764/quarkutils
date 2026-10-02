//! Whose request is being served.
//!
//! A file's mode is checked against a user, a group, and the groups the user
//! is in besides. The first two are asked of the kernel by whatever handles
//! the request; the rest are asked once a request at most, and only when a
//! file's group is not the caller's own — which is most requests' whole
//! life, so most requests never ask.

use quark_rt::syscall;

/// How many groups a caller may be in besides its own.
pub const MAX: usize = syscall::MAX_GROUPS;
/// The user nobody is: who a caller that has gone is taken to be. It used to
/// be taken to be root, by a default nobody had meant to be a decision.
pub const NOBODY: u32 = 65534;

static mut SENDER: usize = 0;
static mut GROUPS: [u32; MAX] = [0; MAX];
static mut COUNT: usize = 0;
static mut KNOWN: bool = false;

/// A request from `sender` is about to be served.
pub fn began(sender: usize) {
    unsafe {
        SENDER = sender;
        KNOWN = false;
    }
}

/// The groups `sender` is in besides its own: what the server above said,
/// for a request it is passing on, and what the kernel says otherwise.
pub fn groups_of(sender: usize, out: &mut [u32; MAX]) -> usize {
    match crate::mounts::acting_groups(sender) {
        Some((groups, n)) => {
            *out = groups;
            n
        }
        None => syscall::sys_groups(sender, out).unwrap_or(0).min(MAX),
    }
}

/// Whether the caller of the request being served is in group `gid`,
/// besides its own.
pub fn in_group(gid: u32) -> bool {
    unsafe {
        if !KNOWN {
            COUNT = groups_of(SENDER, &mut *core::ptr::addr_of_mut!(GROUPS));
            KNOWN = true;
        }
        let groups = &*core::ptr::addr_of!(GROUPS);
        groups[..COUNT].contains(&gid)
    }
}
