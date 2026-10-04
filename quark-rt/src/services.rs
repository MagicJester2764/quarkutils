//! The service manager: `init`, which answers as `init` once the machine is
//! up.
//!
//! What every service is and how it is doing is anybody's to ask
//! ([`table`], [`state`], [`describe`]). Starting, stopping and starting one
//! again ([`start`], [`stop`], [`restart`]), and stopping them all for a
//! shutdown ([`stop_all`]), are for a caller holding `TaskMgmt` over every
//! task: init holds it too, and reads the caller's capabilities to see. A
//! stop is answered once the service has gone. What a service has printed is
//! [`log`]. `docs/services.md` says it whole; `svc` is the program.

use crate::ipc::Message;
use crate::{nameserver, syscall};

/// The name init registers.
pub const NAME: &[u8] = b"init";

/// Every service, as text, into a lent buffer: `[written, how long it was]`.
pub const TAG_TABLE: u64 = 1;
/// One, by name: its state, its task, how many times it has been started.
pub const TAG_STATE: u64 = 2;
pub const TAG_START: u64 = 3;
pub const TAG_STOP: u64 = 4;
pub const TAG_RESTART: u64 = 5;
/// What one has printed, as text, into a lent buffer.
pub const TAG_LOG: u64 = 6;
/// Stop every service, in order, before the machine is turned off; `data[0]`
/// 1 not to give them time.
pub const TAG_STOP_ALL: u64 = 7;
/// One, as text, into a lent buffer: what it is, what it needs, how it ended.
pub const TAG_DESCRIBE: u64 = 8;

pub const TAG_OK: u64 = 0;
pub const TAG_ERROR: u64 = u64::MAX;
/// Why not, in `data[0]` of a refusal: Linux's numbers.
pub const NOT_ALLOWED: u64 = 1;
pub const NO_SUCH: u64 = 2;
pub const SHUTTING_DOWN: u64 = 16;
pub const INVALID: u64 = 22;
/// A service that could not be started again is not stopped.
pub const CANNOT: u64 = 95;

/// The longest name a service has.
pub const NAME_MAX: usize = 24;

/// What a service is doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Running, and registered if it registers.
    Up,
    /// Running, and not yet registered under the name it is waited for by.
    Starting,
    /// Not started: something it needs is not up.
    Waiting,
    /// Stopped by `svc stop`, or by a shutdown.
    Stopped,
    /// Ended, and not to be started again: it failed, or failed too often.
    Failed,
    /// Ended by itself with status 0, and not to be started again.
    Done,
    /// Ended, and to be started again in a moment.
    Restarting,
    /// Sent SIGTERM, and not gone yet.
    Stopping,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Up => "up",
            State::Starting => "starting",
            State::Waiting => "waiting",
            State::Stopped => "stopped",
            State::Failed => "failed",
            State::Done => "done",
            State::Restarting => "restarting",
            State::Stopping => "stopping",
        }
    }

    pub fn from_word(n: u64) -> Option<State> {
        Some(match n {
            0 => State::Up,
            1 => State::Starting,
            2 => State::Waiting,
            3 => State::Stopped,
            4 => State::Failed,
            5 => State::Done,
            6 => State::Restarting,
            7 => State::Stopping,
            _ => return None,
        })
    }

    pub fn number(self) -> u64 {
        match self {
            State::Up => 0,
            State::Starting => 1,
            State::Waiting => 2,
            State::Stopped => 3,
            State::Failed => 4,
            State::Done => 5,
            State::Restarting => 6,
            State::Stopping => 7,
        }
    }
}

/// One service, as [`state`] says it.
#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub state: State,
    /// Its task, while it runs; 0 when it does not.
    pub tid: usize,
    /// Its process id, while it runs.
    pub pid: u64,
    /// How many times it has been started.
    pub starts: u64,
    /// How it last ended, if it has.
    pub ended: Option<i32>,
}

/// A name in three words and its length in a fourth, as the nameserver
/// takes one.
pub fn pack(name: &[u8]) -> [u64; 4] {
    let mut b = [0u8; NAME_MAX];
    let n = name.len().min(NAME_MAX);
    b[..n].copy_from_slice(&name[..n]);
    let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
    [w(0), w(1), w(2), n as u64]
}

/// The name [`pack`] made.
pub fn unpack(words: &[u64]) -> ([u8; NAME_MAX], usize) {
    let mut b = [0u8; NAME_MAX];
    for i in 0..3 {
        b[i * 8..i * 8 + 8].copy_from_slice(&words[i].to_le_bytes());
    }
    let n = (words[3] as usize).min(NAME_MAX);
    (b, n)
}

/// The service manager, if it is answering.
pub fn manager() -> Option<usize> {
    nameserver::lookup(NAME)
}

fn about(init: usize, tag: u64, name: &[u8], extra: u64) -> Result<Message, u64> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(INVALID);
    }
    let w = pack(name);
    let msg = Message { sender: 0, tag, data: [w[0], w[1], w[2], w[3], extra, 0] };
    let mut reply = Message::empty();
    syscall::sys_call(init, &msg, &mut reply).map_err(|_| NO_SUCH)?;
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok(reply) }
}

fn text(init: usize, tag: u64, name: &[u8], buf: &mut [u8]) -> Result<(usize, usize), u64> {
    let w = pack(name);
    let msg = Message { sender: 0, tag, data: [w[0], w[1], w[2], w[3], buf.len() as u64, 0] };
    let mut reply = Message::empty();
    syscall::sys_call_lend_mut(init, &msg, &mut reply, buf).map_err(|_| NO_SUCH)?;
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok((reply.data[0] as usize, reply.data[1] as usize)) }
}

/// Every service, as text: how much of `buf` was written, and how long all
/// of it was.
pub fn table(init: usize, buf: &mut [u8]) -> Result<(usize, usize), u64> {
    text(init, TAG_TABLE, b"", buf)
}

/// One service, as text.
pub fn describe(init: usize, name: &[u8], buf: &mut [u8]) -> Result<(usize, usize), u64> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(INVALID);
    }
    text(init, TAG_DESCRIBE, name, buf)
}

/// What one service has printed, as text.
pub fn log(init: usize, name: &[u8], buf: &mut [u8]) -> Result<(usize, usize), u64> {
    if name.is_empty() || name.len() > NAME_MAX {
        return Err(INVALID);
    }
    text(init, TAG_LOG, name, buf)
}

/// How one service is doing.
pub fn state(init: usize, name: &[u8]) -> Result<Status, u64> {
    let r = about(init, TAG_STATE, name, 0)?;
    Ok(Status {
        state: State::from_word(r.data[0]).ok_or(INVALID)?,
        tid: r.data[1] as usize,
        pid: r.data[2],
        starts: r.data[3],
        ended: (r.data[5] != 0).then_some(r.data[4] as i32),
    })
}

pub fn start(init: usize, name: &[u8]) -> Result<(), u64> {
    about(init, TAG_START, name, 0).map(|_| ())
}

/// Stop a service: answered once it has gone.
pub fn stop(init: usize, name: &[u8]) -> Result<(), u64> {
    about(init, TAG_STOP, name, 0).map(|_| ())
}

pub fn restart(init: usize, name: &[u8]) -> Result<(), u64> {
    about(init, TAG_RESTART, name, 0).map(|_| ())
}

/// Stop every service, the ones nothing needs first: answered once they
/// have gone and what they wrote is on the disk. `now` gives them no time.
pub fn stop_all(init: usize, now: bool) -> Result<(), u64> {
    let msg = Message { sender: 0, tag: TAG_STOP_ALL, data: [now as u64, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    syscall::sys_call(init, &msg, &mut reply).map_err(|_| NO_SUCH)?;
    if reply.tag == TAG_ERROR { Err(reply.data[0]) } else { Ok(()) }
}

/// What a refusal's number means, for a program to say.
pub fn why(code: u64) -> &'static str {
    match code {
        NOT_ALLOWED => "this account may not do that",
        NO_SUCH => "there is no such service",
        SHUTTING_DOWN => "the machine is shutting down",
        INVALID => "that is not a service's name",
        CANNOT => "that service could not be started again, so it is not stopped",
        _ => "the service manager would not do that",
    }
}
