//! A network card, as the network stack sees one: the protocol between
//! `net`, which is the stack, and the driver of each card.
//!
//! A card's driver is started by the device manager, holding its card, and
//! registers as the first of `eth0` to `eth7` that nobody has. It answers the
//! program that claimed it and nobody else — a program that could send
//! frames on a card, or read what arrives, would be the network — and the
//! claim goes when its holder does.
//!
//! Frames move with the call: the stack lends one to be read to send it,
//! and lends a buffer to be written to receive one. Nothing waits on a
//! card: when frames come, the driver *tells* its claimant (a notification,
//! [`ARRIVED`], to the endpoint the claimant offered with its claim), and
//! the claimant asks for them until there are none. A driver that waited
//! for its claimant, or a stack that waited on a driver, would be one
//! client stopping everybody.
//!
//! | Tag | Asks | Answer |
//! |---|---|---|
//! | [`TAG_CLAIM`] | be this card's, with an endpoint offered | `[the card's address]` |
//! | [`TAG_SEND`] | send the frame lent, `data[0]` bytes | `[]` |
//! | [`TAG_RECEIVE`] | the next frame that has come, into the buffer lent | `[its length, 0 for none]` |
//!
//! A refusal, or a request from anybody but the claimant, is answered with
//! tag `u64::MAX`. The driver end is [`serve`]: a driver supplies the card
//! ([`Card`]) and this supplies the rest.

use crate::ipc::{death_notice, Message, TAG_PING, TID_ANY};
use crate::syscall;

pub const TAG_CLAIM: u64 = 1;
pub const TAG_SEND: u64 = 2;
pub const TAG_RECEIVE: u64 = 3;

/// What a driver's notification says: frames have come.
pub const ARRIVED: u64 = 1;

/// The longest frame: an Ethernet frame without its checksum.
pub const FRAME: usize = 1514;

const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;

/// A card, as its driver supplies it.
pub trait Card {
    /// Its hardware address.
    fn address(&self) -> [u8; 6];
    /// Send `frame`: whether it went.
    fn send(&mut self, frame: &[u8]) -> bool;
    /// Take the next frame that has come into `into`: its length, or 0
    /// when there is none.
    fn receive(&mut self, into: &mut [u8]) -> usize;
    /// The card's interrupt has come: deal with it, and say whether frames
    /// may have come.
    fn interrupt(&mut self) -> bool;
    /// Which interrupt that is.
    fn irq(&self) -> u8;
}

/// The address as one word, low byte first.
pub fn pack(address: [u8; 6]) -> u64 {
    let mut word = [0u8; 8];
    word[..6].copy_from_slice(&address);
    u64::from_le_bytes(word)
}

pub fn unpack(word: u64) -> [u8; 6] {
    let bytes = word.to_le_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]
}

/// Register a card's driver under the first of `eth0` to `eth7` that nobody
/// has: the name, or `None` when all eight are taken.
pub fn register() -> Option<[u8; 4]> {
    (0..8u8).map(|n| [b'e', b't', b'h', b'0' + n]).find(|name| crate::nameserver::register(name).is_ok())
}

/// Serve the card for ever: claims, frames, and its interrupt.
pub fn serve<C: Card>(card: &mut C) -> ! {
    let mut claimant = 0usize;
    let mut frame = [0u8; FRAME];
    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }
        if let Some(gone) = death_notice(&msg) {
            if gone == claimant {
                claimant = 0;
            }
            continue;
        }
        if msg.sender == 0 {
            if msg.tag == card.irq() as u64 && card.interrupt() && claimant != 0 {
                let _ = syscall::sys_notify(claimant, ARRIVED);
            }
            continue;
        }
        let ok = |data: [u64; 6]| Message { sender: 0, tag: TAG_OK, data };
        let no = Message { sender: 0, tag: TAG_ERROR, data: [0; 6] };
        let reply = match msg.tag {
            TAG_PING => Message { sender: 0, tag: TAG_PING, data: [0; 6] },
            TAG_CLAIM if claimant == 0 || claimant == msg.sender => {
                // The endpoint offered is what the claimant is told by;
                // without one it would never hear that frames had come.
                match syscall::sys_cap_take_any(msg.sender) {
                    Ok(_) if syscall::sys_task_watch(msg.sender).is_ok() => {
                        claimant = msg.sender;
                        ok([pack(card.address()), 0, 0, 0, 0, 0])
                    }
                    _ => no,
                }
            }
            _ if msg.sender != claimant => no,
            TAG_SEND => {
                let len = msg.data[0] as usize;
                let read = len > 0
                    && len <= FRAME
                    && syscall::sys_lent_read(msg.sender, 0, &mut frame[..len]) == Ok(len);
                if read && card.send(&frame[..len]) { ok([0; 6]) } else { no }
            }
            TAG_RECEIVE => {
                let room = (msg.data[0] as usize).min(FRAME);
                let len = card.receive(&mut frame[..]);
                if len == 0 {
                    ok([0; 6])
                } else if len <= room && syscall::sys_lent_write(msg.sender, 0, &frame[..len]) == Ok(len) {
                    ok([len as u64, 0, 0, 0, 0, 0])
                } else {
                    // A frame that does not fit is dropped, as a card drops
                    // one too long for its ring.
                    no
                }
            }
            _ => no,
        };
        let _ = syscall::sys_reply(msg.sender, &reply);
    }
}

/// A card, as the stack holds one.
pub struct Link {
    pub tid: usize,
}

impl Link {
    /// Claim the card registered as `name`, offering it the right to tell
    /// this program that frames have come: the card, and its address.
    pub fn claim(name: &[u8]) -> Option<(Link, [u8; 6])> {
        let tid = crate::nameserver::lookup(name)?;
        let msg = Message { sender: 0, tag: TAG_CLAIM, data: [0; 6] };
        let mut reply = Message::empty();
        syscall::sys_call_offer_self(tid, &msg, &mut reply).ok()?;
        (reply.tag == TAG_OK).then(|| (Link { tid }, unpack(reply.data[0])))
    }

    /// Send `frame`: whether the card took it.
    pub fn send(&self, frame: &[u8]) -> bool {
        let msg = Message { sender: 0, tag: TAG_SEND, data: [frame.len() as u64, 0, 0, 0, 0, 0] };
        let mut reply = Message::empty();
        syscall::sys_call_lend(self.tid, &msg, &mut reply, frame).is_ok() && reply.tag == TAG_OK
    }

    /// The next frame that has come, into `into`: its length, or 0.
    pub fn receive(&self, into: &mut [u8]) -> usize {
        let msg = Message { sender: 0, tag: TAG_RECEIVE, data: [into.len() as u64, 0, 0, 0, 0, 0] };
        let mut reply = Message::empty();
        if syscall::sys_call_lend_mut(self.tid, &msg, &mut reply, into).is_err() || reply.tag != TAG_OK {
            return 0;
        }
        (reply.data[0] as usize).min(into.len())
    }
}
