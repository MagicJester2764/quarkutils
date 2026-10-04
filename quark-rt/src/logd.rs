//! The log's protocol: between `init` and `logd`, which nobody else may call.
//!
//! A service's descriptors 1 and 2 are IPC descriptors to `logd` whose tag is
//! [`STREAM`] plus its stream's number, so that whatever writes there — the
//! service, a thread of it, a child it forked — is that service. The kernel
//! makes each write a call, forty bytes a time; `logd` passes the bytes on to
//! the console and keeps the lines. `init` names a stream before anything
//! writes to it ([`TAG_NAME`]), says when the root is there to write
//! `/var/log/messages` on ([`TAG_FILES`]), asks for what a stream has said
//! lately ([`TAG_TAIL`]), says when the session has the console
//! ([`TAG_QUIET`]), and, at a shutdown, asks for everything kept to be
//! written ([`TAG_SYNC`], [`TAG_SYNCED`]).
//!
//! `logd` registers no name: `init` made it, so only `init` may call it, and
//! a descriptor is the only other way in. A line in the log is one a
//! service's descriptor wrote.

/// A stream's tag is this plus its number.
pub const STREAM: u64 = 0x1_0000;
/// The numbers a stream's tag may have.
pub const MAX_STREAMS: u64 = 0x1_0000;
/// The streams kept: one past the last whose lines are kept. What a stream
/// above it writes is passed on to the console and nothing more.
pub const KEPT_STREAMS: u64 = 256;
/// init's own; a service's is its place among init's services, plus one.
pub const INIT_STREAM: u64 = 0;

/// Name a stream: `data[0]` its number, `data[1..4]` the name, `data[4]` its
/// length, `data[5]` the process id of what writes to it.
pub const TAG_NAME: u64 = 1;
/// The root is up: what is kept may be written to it from now on.
pub const TAG_FILES: u64 = 2;
/// What stream `data[0]` has said lately, as text, into a lent buffer:
/// `[written, how long it all was]`.
pub const TAG_TAIL: u64 = 3;
/// Write everything kept: `[a number to ask TAG_SYNCED about]`.
pub const TAG_SYNC: u64 = 4;
/// Whether what `TAG_SYNC` answered `data[0]` for has been written:
/// `[1 if so]`.
pub const TAG_SYNCED: u64 = 5;
/// The session has the console: nothing more is passed on to it, only
/// kept. Answered once what was passed on before is on it, so that nothing
/// a service printed comes after the login prompt and pushes it off its
/// line.
pub const TAG_QUIET: u64 = 6;

pub const TAG_OK: u64 = 0;
pub const TAG_ERROR: u64 = u64::MAX;

/// Where the log is written.
pub const PATH: &[u8] = b"/var/log/messages";
/// And where it goes when that is a megabyte long.
pub const OLD_PATH: &[u8] = b"/var/log/messages.0";
