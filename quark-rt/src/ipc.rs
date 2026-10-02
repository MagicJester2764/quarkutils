/// IPC message type — mirrors the kernel's Message struct.

pub const TID_ANY: usize = usize::MAX;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Message {
    pub sender: usize,
    pub tag: u64,
    pub data: [u64; 6],
}

impl Message {
    pub const fn empty() -> Self {
        Message {
            sender: 0,
            tag: 0,
            data: [0; 6],
        }
    }
}

/// Conventional no-op request that every IPC server answers.
///
/// A ping has to measure a round trip to the service itself, but there is no
/// tag common to the protocols, and sending a real one would make the service
/// do work. This value sits far from the small integers the protocols use, so
/// any server can answer it without colliding with its own tags. The reply
/// carries no payload — the round trip is the whole point.
///
/// Note that not every service is an IPC server: the console is driven by a
/// pipe on fd 0 and has no dispatch to answer from, so it never replies.
pub const TAG_PING: u64 = 0xFFFF_FFFF_FFFF_FF01;

/// Kernel notification word, delivered with sender 0.
pub const TAG_NOTIFICATION: u64 = 0xFFFF_0002;

/// A task registered with [`crate::syscall::sys_task_watch`] has died;
/// `data[0]` is its TID. Sender is 0, as for every message the kernel makes
/// up — a receive loop that reads sender 0 as "an IRQ" needs to check the tag
/// before it believes that.
pub const TAG_TASK_DIED: u64 = 0xFFFF_0003;

/// From the kernel (sender 0): program `data[0]` — a space id — has no task
/// left. See `syscall::sys_space_watch`.
pub const TAG_SPACE_DIED: u64 = 0xFFFF_0004;
/// The kernel asks a pager for a page of an object: `data` is `[cookie,
/// page, object id]`, with a frame lent for writing. `sender` is the faulting
/// task with `PAGER_BIT` set; reply to it as it is.
pub const TAG_PAGE_IN: u64 = 0xFFFF_0005;
/// Nothing maps an object any more: `data` is `[cookie, object id]`, sender 0.
pub const TAG_OBJECT_IDLE: u64 = 0xFFFF_0006;
/// A program asked, through the kernel, for an object's written pages to reach
/// its file: `data` is `[cookie, object id]`, sender marked as for a page-in.
pub const TAG_OBJECT_SYNC: u64 = 0xFFFF_0007;
/// From the kernel, sender 0: memory is short, and pages of this pager's
/// that nothing maps could be given up if they were written. It names no
/// object: the pager writes what is dirty in each it has.
pub const TAG_OBJECT_CLEAN: u64 = 0xFFFF_000B;
/// From the kernel, sender 0: an object this task serves is named by no
/// descriptor any more. No data — collect with `sys_fd_reap` until it is empty.
pub const TAG_FD_RELEASED: u64 = 0xFFFF_0008;
/// The kernel reading through a descriptor for the task in `sender`:
/// `data` = `[cookie, length]`, with a buffer lent to fill. Reply tag 0 and
/// the count in `data[0]`. Check the sender holds the cookie: any task with
/// an endpoint for this server can send the tag.
pub const TAG_FD_READ: u64 = 0xFFFF_0009;
/// The same, writing: the buffer is lent to read.
pub const TAG_FD_WRITE: u64 = 0xFFFF_000A;
/// Set in `sender` by the kernel alone, on `TAG_PAGE_IN` and `TAG_OBJECT_SYNC`.
pub const PAGER_BIT: usize = 1 << 62;

/// The task `msg` reports dead, if it is the kernel's [`TAG_TASK_DIED`].
///
/// The tag alone proves nothing: any program that can call a server can send
/// it. The sender is what the kernel stamps, and it is 0 only for what the
/// kernel made up. A message with the tag and any other sender is a request,
/// and an unknown one — a server that believed it would forget a lease, a
/// registration or a claim because somebody asked it to.
/// Whether `msg` is the kernel saying that an object this task serves has no
/// descriptors left. As with a death, the tag alone proves nothing: only the
/// kernel sends as sender 0.
pub fn fd_released_notice(msg: &Message) -> bool {
    msg.sender == 0 && msg.tag == TAG_FD_RELEASED
}

pub fn death_notice(msg: &Message) -> Option<usize> {
    (msg.sender == 0 && msg.tag == TAG_TASK_DIED).then_some(msg.data[0] as usize)
}

/// The program `msg` reports gone, if it is the kernel's [`TAG_SPACE_DIED`].
pub fn space_death_notice(msg: &Message) -> Option<u64> {
    (msg.sender == 0 && msg.tag == TAG_SPACE_DIED).then_some(msg.data[0])
}
