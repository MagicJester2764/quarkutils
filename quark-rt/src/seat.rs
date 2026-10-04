//! The seat: the console's keyboard, its pointer and its display, which are
//! the person's logged in at it.
//!
//! `input` and `fb` give them to the console — `qtty`, which init names to
//! them by its process id before it starts it — and to the seat as init says
//! it is ([`TAG_SEAT`]): the session a login began, and once somebody has
//! logged in there, every program of theirs, whatever session it is in. A
//! terminal's shell under a compositor is in one of its own. While nobody
//! is logged in it is the session service's session — getty's, or the
//! console's `login`'s, which hold nothing else, or a shell's where the
//! session is a shell and nobody logs in. A login takes the seat from init
//! (`services::take_seat`) once it has begun its session and before it
//! prompts, says whose it is once the password is right
//! (`services::seat_user`), and when it ends the seat is the session
//! service's again. What somebody left running when they logged out is
//! refused from then on: the keys the next person types, the lines they
//! enter, the screen they read.
//!
//! The console is named because nothing else would do: every program init
//! starts begins a session of its own, so no session is the system's.
//! `docs/services.md` says how init decides.

use crate::ipc::Message;
use crate::syscall;

/// From init to `input` and `fb`: `data[0]` is the session the seat is for,
/// or 0 for none; `data[1]` is 1 and `data[2]` the user once somebody has
/// logged in there; `data[3]` is the console's process id. Believed only
/// from the server's parent, which is init. Numbered clear of both servers'
/// own tags.
pub const TAG_SEAT: u64 = 0x300;

/// Who may use the seat, as a server that gives it out keeps it.
pub struct Seat {
    /// The console, by process id: never another program's.
    console: u64,
    /// The session the seat is for, or 0.
    session: u64,
    /// Who logged in there, once somebody has.
    user: Option<u32>,
    /// Who says: the server's parent.
    init: usize,
}

impl Seat {
    /// A server's, as it starts: nobody's, until init says.
    pub fn new() -> Seat {
        let me = syscall::sys_getpid() as usize;
        Seat {
            console: 0,
            session: 0,
            user: None,
            init: syscall::sys_task_info(me).map_or(0, |(_, parent, _)| parent),
        }
    }

    /// Whether task `tid` may have the keyboard, the pointer, the display or
    /// a line typed at the console.
    pub fn allows(&self, tid: usize) -> bool {
        let Some(pid) = syscall::sys_pid(tid) else { return false };
        if pid == self.console || (self.session != 0 && syscall::sys_getsid(pid) == Some(self.session)) {
            return true;
        }
        self.user.is_some_and(|user| syscall::sys_task_info(tid).is_ok_and(|(state, _, uid)| state != 3 && uid == user))
    }

    /// Whether `msg` says the seat has moved, from init — and if so, it has.
    pub fn moved(&mut self, msg: &Message) -> bool {
        if msg.tag != TAG_SEAT || self.init == 0 || msg.sender != self.init {
            return false;
        }
        self.session = msg.data[0];
        self.user = (msg.data[1] == 1).then_some(msg.data[2] as u32);
        self.console = msg.data[3];
        true
    }
}
