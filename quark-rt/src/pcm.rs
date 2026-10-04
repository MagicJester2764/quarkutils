//! A sound card, as the sound server sees one: the protocol between `sound`,
//! which mixes, and the driver of each card's output.
//!
//! A card's driver is started by the device manager, holding its card, and
//! registers as the first of `pcm0` to `pcm7` that nobody has. It answers
//! the program that claimed it and nobody else, and the claim goes when its
//! holder does. Sound is one format here — 48,000 frames a second, two
//! channels of sixteen bits, little-endian — and goes out in *periods*: a
//! ring of them the card plays round and round, each as long as the card
//! says when it is claimed. Whatever period nobody has written is silence;
//! and a card that has played nothing but silence for a while stops, until
//! it is next written to, where it begins again.
//!
//! Nothing waits on a card, nor a card on its claimant: when a period has
//! been played the driver *tells* the claimant ([`PLAYED`], to the endpoint
//! offered with the claim), and the claimant writes the next until the card
//! says it has no room.
//!
//! | Tag | Asks | Answer |
//! |---|---|---|
//! | [`TAG_CLAIM`] | be this card's, with an endpoint offered | `[period bytes, periods]` |
//! | [`TAG_WRITE`] | a period, lent: `data[0]` bytes, a whole period | `[1 taken, 0 no room]` |
//! | [`TAG_POSITION`] | how much has been played | `[frames since claimed, periods free]` |
//!
//! A refusal, or a request from anybody but the claimant, is answered with
//! tag `u64::MAX`. The driver end is [`serve`]: a driver supplies the card
//! ([`Card`]) and this supplies the rest.

use crate::ipc::{death_notice, Message, TAG_PING, TID_ANY};
use crate::syscall;

pub const TAG_CLAIM: u64 = 1;
pub const TAG_WRITE: u64 = 2;
pub const TAG_POSITION: u64 = 3;

/// What a driver's notification says: a period has been played.
pub const PLAYED: u64 = 1;

/// The one format: frames a second, channels, and bytes a frame.
pub const RATE: u64 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_BYTES: usize = 4;

/// The longest period a card may have.
pub const MAX_PERIOD: usize = 16384;

const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;

/// A card, as its driver supplies it.
pub trait Card {
    /// How long a period is, in bytes, and how many there are.
    fn periods(&self) -> (usize, usize);
    /// Stop, if it is playing, make every period silence, and count what
    /// is played from nought: the claimant has gone, or a new one has come.
    fn stop(&mut self);
    /// Put `samples` — one period — in the next period that may be
    /// written, and play it, beginning again if the card has stopped:
    /// whether there was a period to put it in.
    fn write(&mut self, samples: &[u8]) -> bool;
    /// How many periods may be written now.
    fn free(&self) -> usize;
    /// How many frames have been played since the card was claimed.
    fn played(&self) -> u64;
    /// The card's interrupt has come: deal with it, and say how many
    /// periods have been played since it last came.
    fn interrupt(&mut self) -> usize;
    /// Which interrupt that is.
    fn irq(&self) -> u8;
}

/// Register a card's driver under the first of `pcm0` to `pcm7` that
/// nobody has: the name, or `None` when all eight are taken.
pub fn register() -> Option<[u8; 4]> {
    (0..8u8).map(|n| [b'p', b'c', b'm', b'0' + n]).find(|name| crate::nameserver::register(name).is_ok())
}

/// Serve the card for ever: claims, periods, and its interrupt.
pub fn serve<C: Card>(card: &mut C) -> ! {
    let mut claimant = 0usize;
    let mut period = [0u8; MAX_PERIOD];
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        if let Some(gone) = death_notice(&msg) {
            if gone == claimant {
                claimant = 0;
                card.stop();
            }
            continue;
        }
        if msg.sender == 0 {
            if msg.tag == card.irq() as u64 && card.interrupt() > 0 && claimant != 0 {
                let _ = syscall::sys_notify(claimant, PLAYED);
            }
            continue;
        }
        let ok = |data: [u64; 6]| Message { sender: 0, tag: TAG_OK, data };
        let no = Message { sender: 0, tag: TAG_ERROR, data: [0; 6] };
        let (bytes, count) = card.periods();
        let reply = match msg.tag {
            TAG_PING => Message { sender: 0, tag: TAG_PING, data: [0; 6] },
            TAG_CLAIM if claimant == 0 || claimant == msg.sender => {
                // The endpoint offered is what the claimant is told by;
                // without one it would never hear that a period had gone.
                match syscall::sys_cap_take_any(msg.sender) {
                    Ok(_) if syscall::sys_task_watch(msg.sender).is_ok() => {
                        if claimant == 0 {
                            card.stop();
                        }
                        claimant = msg.sender;
                        ok([bytes as u64, count as u64, 0, 0, 0, 0])
                    }
                    _ => no,
                }
            }
            _ if msg.sender != claimant => no,
            TAG_WRITE => {
                let len = msg.data[0] as usize;
                if len != bytes || syscall::sys_lent_read(msg.sender, 0, &mut period[..len]) != Ok(len) {
                    no
                } else {
                    ok([card.write(&period[..len]) as u64, 0, 0, 0, 0, 0])
                }
            }
            TAG_POSITION => ok([card.played(), card.free() as u64, 0, 0, 0, 0]),
            _ => no,
        };
        let _ = syscall::sys_reply(msg.sender, &reply);
    }
}

/// A card, as the sound server holds one.
pub struct Output {
    pub tid: usize,
    /// How long a period is, in bytes, and how many the card has.
    pub period: usize,
    pub periods: usize,
}

impl Output {
    /// Claim the card registered as `name`, offering it the right to tell
    /// this program that a period has been played.
    pub fn claim(name: &[u8]) -> Option<Output> {
        let tid = crate::nameserver::lookup(name)?;
        let msg = Message { sender: 0, tag: TAG_CLAIM, data: [0; 6] };
        let mut reply = Message::empty();
        syscall::sys_call_offer_self(tid, &msg, &mut reply).ok()?;
        let (period, periods) = (reply.data[0] as usize, reply.data[1] as usize);
        (reply.tag == TAG_OK && period > 0 && period <= MAX_PERIOD && period % FRAME_BYTES == 0 && periods > 1)
            .then_some(Output { tid, period, periods })
    }

    /// Write a period: whether the card had room for it.
    pub fn write(&self, samples: &[u8]) -> bool {
        let msg = Message { sender: 0, tag: TAG_WRITE, data: [samples.len() as u64, 0, 0, 0, 0, 0] };
        let mut reply = Message::empty();
        syscall::sys_call_lend(self.tid, &msg, &mut reply, samples).is_ok() && reply.tag == TAG_OK && reply.data[0] == 1
    }

    /// How many frames the card has played since it was claimed, and how
    /// many periods may be written.
    pub fn position(&self) -> Option<(u64, usize)> {
        let msg = Message { sender: 0, tag: TAG_POSITION, data: [0; 6] };
        let mut reply = Message::empty();
        syscall::sys_call(self.tid, &msg, &mut reply).ok()?;
        (reply.tag == TAG_OK).then_some((reply.data[0], reply.data[1] as usize))
    }
}
