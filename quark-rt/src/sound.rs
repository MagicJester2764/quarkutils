//! Sound, as a program makes it: the protocol of the sound server, `sound`,
//! and the client end of it.
//!
//! A program opens a *stream* in its own format — any rate from 8 to 96 kHz,
//! one channel or two, sixteen-bit samples, little-endian — and writes
//! samples to it. The server keeps a third of a second or so of each
//! stream, mixes every stream into what the card plays, and says, when it
//! is asked, how much of a stream is still waiting and how much has gone.
//! A write takes what fits and says how much; a program told "full" is told
//! when there is room, by a notification to the endpoint it offered when it
//! opened the stream. A stream is its program's and goes with it; a program
//! may have four.
//!
//! | Tag | Asks | Answer |
//! |---|---|---|
//! | [`TAG_OPEN`] | `[rate, channels, bits]`, an endpoint offered | `[the stream]` |
//! | [`TAG_WRITE`] | `[stream, bytes]`, the samples lent | `[bytes taken]` |
//! | [`TAG_STATUS`] | `[stream]` | `[frames waiting, frames played, the card's frames]` |
//! | [`TAG_CLOSE`] | `[stream]` | `[]` |
//! | [`TAG_VOLUME`] | `[stream or ALL, 0 to 100 or ASK]` | `[the volume]` |
//!
//! A refusal is tag `u64::MAX`.

use crate::ipc::Message;
use crate::syscall;

pub const TAG_OPEN: u64 = 1;
pub const TAG_WRITE: u64 = 2;
pub const TAG_STATUS: u64 = 3;
pub const TAG_CLOSE: u64 = 4;
pub const TAG_VOLUME: u64 = 5;

/// The whole of the mix, to [`TAG_VOLUME`]; and a volume asked about
/// rather than set.
pub const ALL: u64 = u64::MAX;
pub const ASK: u64 = u64::MAX;

/// What the server's notification says: a stream that was full has room.
pub const ROOM: u64 = 1;

/// The rates a stream may have.
pub const MIN_RATE: u64 = 8_000;
pub const MAX_RATE: u64 = 96_000;

const TAG_OK: u64 = 0;

/// How a stream stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// Frames written and not yet mixed, at the stream's rate.
    pub waiting: u64,
    /// Frames mixed since it was opened, at its rate.
    pub played: u64,
    /// Frames the card has played since the server claimed it, at 48 kHz.
    pub card: u64,
}

/// A stream this program opened.
pub struct Stream {
    server: usize,
    id: u64,
    frame: usize,
}

fn call(server: usize, tag: u64, data: [u64; 6]) -> Option<[u64; 6]> {
    let msg = Message { sender: 0, tag, data };
    let mut reply = Message::empty();
    syscall::sys_call(server, &msg, &mut reply).ok()?;
    (reply.tag == TAG_OK).then_some(reply.data)
}

impl Stream {
    /// Open a stream of `rate` frames a second and `channels` channels.
    pub fn open(rate: u64, channels: u64) -> Result<Stream, &'static str> {
        let server = crate::nameserver::lookup(b"sound").ok_or("there is no sound server")?;
        let msg = Message { sender: 0, tag: TAG_OPEN, data: [rate, channels, 16, 0, 0, 0] };
        let mut reply = Message::empty();
        if syscall::sys_call_offer_self(server, &msg, &mut reply).is_err() {
            return Err("the sound server did not answer");
        }
        if reply.tag != TAG_OK {
            return Err("the sound server would not have a stream like that");
        }
        Ok(Stream { server, id: reply.data[0], frame: 2 * channels as usize })
    }

    /// Write what fits of `samples`, whole frames: how many bytes it took.
    pub fn write(&self, samples: &[u8]) -> usize {
        let len = samples.len() - samples.len() % self.frame;
        if len == 0 {
            return 0;
        }
        let msg = Message { sender: 0, tag: TAG_WRITE, data: [self.id, len as u64, 0, 0, 0, 0] };
        let mut reply = Message::empty();
        if syscall::sys_call_lend(self.server, &msg, &mut reply, &samples[..len]).is_err() || reply.tag != TAG_OK {
            return 0;
        }
        (reply.data[0] as usize).min(len)
    }

    /// Write all of `samples`, waiting for room as it is made: whether it
    /// all went. The wait is for the server's notification, or a tenth of a
    /// second, whichever comes first — so a program with a loop of its own
    /// to keep, that hears from the kernel, calls [`Stream::write`] instead.
    pub fn write_all(&self, mut samples: &[u8]) -> bool {
        samples = &samples[..samples.len() - samples.len() % self.frame];
        while !samples.is_empty() {
            let took = self.write(samples);
            if took == 0 {
                if self.status().is_none() {
                    return false;
                }
                let mut msg = Message::empty();
                let _ = syscall::sys_recv_timeout(0, &mut msg, syscall::ns(100_000_000));
                continue;
            }
            samples = &samples[took..];
        }
        true
    }

    pub fn status(&self) -> Option<Status> {
        let data = call(self.server, TAG_STATUS, [self.id, 0, 0, 0, 0, 0])?;
        Some(Status { waiting: data[0], played: data[1], card: data[2] })
    }

    /// Wait until everything written has been mixed: whether it was. What
    /// has been mixed is the card's, and is played whether or not this
    /// program is still there to hear it.
    pub fn drain(&self) -> bool {
        loop {
            match self.status() {
                Some(s) if s.waiting == 0 => return true,
                Some(_) => syscall::sleep_ns(10_000_000),
                None => return false,
            }
        }
    }

    /// Set the stream's volume, 0 to 100: what it is now.
    pub fn set_volume(&self, volume: u64) -> Option<u64> {
        call(self.server, TAG_VOLUME, [self.id, volume, 0, 0, 0, 0]).map(|d| d[0])
    }

    /// The stream's id, as the server knows it.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = call(self.server, TAG_CLOSE, [self.id, 0, 0, 0, 0, 0]);
    }
}

/// The volume of the whole mix, 0 to 100, set to `volume` if it is given.
pub fn volume(volume: Option<u64>) -> Option<u64> {
    let server = crate::nameserver::lookup(b"sound")?;
    call(server, TAG_VOLUME, [ALL, volume.unwrap_or(ASK), 0, 0, 0, 0]).map(|d| d[0])
}
