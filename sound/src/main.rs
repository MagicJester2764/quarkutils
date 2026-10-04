#![no_std]
#![no_main]

//! The sound server: one mixer, in front of the first sound card.
//!
//! A program opens a *stream* in its own format and writes samples to it
//! (`quark_rt::sound` is the protocol and the client end). This keeps up to
//! a third of a second of each stream, and whenever the card has room for a
//! period, mixes one out of every stream that has something — each at its
//! own volume and resampled to the card's 48 kHz, under one volume for the
//! whole mix — and gives it to the card (`quark_rt::pcm`). A stream with
//! nothing written is silent, and so is the card with no streams.
//!
//! Nothing here waits on a program: a write takes what fits and says how
//! much, and a program told "full" is told when there is room, by a
//! notification. A stream is its program's, and goes when the program does.
//!
//! Started at boot whether or not the machine has a card, and quiet about
//! it: the card (`pcm0`, which the device manager's driver for it registers
//! as) is looked for every second for the first minute, and claimed once
//! it is there — first, so that no other program has it. After that it is
//! looked for when a stream is opened, which is refused if there is none.

use quark_rt::ipc::{space_death_notice, Message, TAG_NOTIFICATION, TAG_PING, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::pcm::{self, Output};
use quark_rt::sound::{self, ALL, ASK, ROOM};
use quark_rt::{nameserver, println, syscall};

quark_rt::manifest!([CapReq::priority(quark_rt::syscall::PRIO_SERVER)]);

const MAX_STREAMS: usize = 16;
/// Streams a program may have at once.
const PER_PROGRAM: usize = 4;
/// What a stream keeps: a third of a second of 48 kHz stereo, or a little
/// more of anything less.
const RING: usize = 65536;
/// The card's period, as long as this mixes at once.
const MAX_FRAMES: usize = pcm::MAX_PERIOD / pcm::FRAME_BYTES;

const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;

struct Stream {
    used: bool,
    /// Bumped on each open, so a stream's id names one opening.
    generation: u64,
    /// The task told when there is room, the slot its endpoint is in, and
    /// its program, whose stream this is.
    owner: usize,
    slot: usize,
    space: u64,
    rate: u64,
    channels: usize,
    volume: u64,
    ring: [u8; RING],
    head: usize,
    len: usize,
    /// Where between this frame and the next the mix has got to, of 2^32.
    phase: u64,
    /// Frames mixed since it was opened.
    played: u64,
    /// Told it was full, and to be told when it is not.
    full: bool,
}

const CLOSED: Stream = Stream {
    used: false,
    generation: 0,
    owner: 0,
    slot: 0,
    space: 0,
    rate: 0,
    channels: 0,
    volume: 0,
    ring: [0; RING],
    head: 0,
    len: 0,
    phase: 0,
    played: 0,
    full: false,
};

// Every byte of these nought, so a megabyte of streams costs the program's
// file nothing.
static mut STREAMS: [Stream; MAX_STREAMS] = [CLOSED; MAX_STREAMS];
static mut MIX: [i32; MAX_FRAMES * 2] = [0; MAX_FRAMES * 2];
static mut PERIOD: [u8; pcm::MAX_PERIOD] = [0; pcm::MAX_PERIOD];
static mut MASTER: u64 = 100;

fn streams() -> &'static mut [Stream; MAX_STREAMS] {
    unsafe { &mut *core::ptr::addr_of_mut!(STREAMS) }
}

impl Stream {
    fn frame(&self) -> usize {
        2 * self.channels
    }

    fn frames(&self) -> usize {
        self.len / self.frame()
    }

    /// Frame `i` of what waits, as two channels.
    fn sample(&self, i: usize) -> (i32, i32) {
        let at = (self.head + i * self.frame()) % RING;
        let read = |at: usize| i16::from_le_bytes([self.ring[at % RING], self.ring[(at + 1) % RING]]) as i32;
        let left = read(at);
        let right = if self.channels == 2 { read(at + 2) } else { left };
        (left, right)
    }

    /// Add up to `frames` frames of this stream into `mix`, at 48 kHz: how
    /// many it had. A rate that is not the card's is drawn between frames.
    fn mix_into(&mut self, mix: &mut [i32], frames: usize) -> usize {
        let step = (self.rate << 32) / pcm::RATE;
        let have = self.frames();
        let mut done = 0;
        while done < frames {
            let i = (self.phase >> 32) as usize;
            if i >= have {
                break;
            }
            let (a, b) = (self.sample(i), self.sample((i + 1).min(have - 1)));
            let t = (self.phase & 0xFFFF_FFFF) as i64;
            let between = |x: i32, y: i32| (x as i64 + ((y as i64 - x as i64) * t >> 32)) as i32;
            let (l, r) = (between(a.0, b.0), between(a.1, b.1));
            mix[2 * done] += l * self.volume as i32 / 100;
            mix[2 * done + 1] += r * self.volume as i32 / 100;
            self.phase += step;
            done += 1;
        }
        // What was passed is let go; where between two frames the mix is,
        // is kept.
        let used = ((self.phase >> 32) as usize).min(have);
        self.phase -= (used as u64) << 32;
        self.head = (self.head + used * self.frame()) % RING;
        self.len -= used * self.frame();
        self.played += used as u64;
        done
    }
}

/// Mix one period of `frames` frames into `PERIOD`: whether anything was
/// there to mix.
fn mix(frames: usize) -> bool {
    let mix = unsafe { &mut *core::ptr::addr_of_mut!(MIX) };
    let period = unsafe { &mut *core::ptr::addr_of_mut!(PERIOD) };
    mix[..frames * 2].fill(0);
    let mut any = false;
    for s in streams().iter_mut().filter(|s| s.used && s.len > 0) {
        if s.mix_into(&mut mix[..frames * 2], frames) > 0 {
            any = true;
        }
    }
    let master = unsafe { MASTER } as i64;
    for i in 0..frames * 2 {
        let v = (mix[i] as i64 * master / 100).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        period[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
    }
    any
}

/// Give the card a period for every one it has room for, while anybody has
/// anything; then tell whoever was full that there is room. Whether the
/// card is still there to be given anything: a driver that has gone is
/// claimed again, or another, when a stream is next opened.
fn top_up(card: &Output) -> bool {
    let frames = card.period / pcm::FRAME_BYTES;
    let Some((_, mut free)) = card.position() else {
        return false;
    };
    while free > 0 && streams().iter().any(|s| s.used && s.len > 0) {
        mix(frames);
        let period = unsafe { &*core::ptr::addr_of!(PERIOD) };
        if !card.write(&period[..card.period]) {
            break;
        }
        free -= 1;
    }
    for s in streams().iter_mut().filter(|s| s.used && s.full && s.len < RING / 2) {
        s.full = false;
        let _ = syscall::sys_notify(s.owner, ROOM);
    }
    true
}

/// The stream `id` names, if it is open and the program `space`'s.
fn stream_of(id: u64, space: u64) -> Option<&'static mut Stream> {
    let s = streams().get_mut((id & 0xFF) as usize)?;
    (s.used && s.generation == id >> 8 && s.space == space).then_some(s)
}

fn close(s: &mut Stream) {
    let _ = syscall::sys_cap_delete(s.slot);
    s.used = false;
    s.len = 0;
    s.full = false;
}

/// The first card, if it is there yet.
fn look_for_card() -> Option<Output> {
    Output::claim(b"pcm0")
}

fn open(sender: usize, space: u64, msg: &Message, card: &mut Option<Output>) -> Message {
    if card.is_none() {
        *card = look_for_card();
        if card.is_none() {
            return error();
        }
    }
    let (rate, channels, bits) = (msg.data[0], msg.data[1], msg.data[2]);
    let fits = (sound::MIN_RATE..=sound::MAX_RATE).contains(&rate) && (channels == 1 || channels == 2) && bits == 16;
    let mine = streams().iter().filter(|s| s.used && s.space == space).count();
    let Some(i) = streams().iter().position(|s| !s.used) else {
        return error();
    };
    if !fits || mine >= PER_PROGRAM {
        return error();
    }
    // The endpoint it offered is how it is told there is room; and its
    // program's going is how its streams go.
    let Ok(slot) = syscall::sys_cap_take_any(sender) else {
        return error();
    };
    if syscall::sys_space_watch(space).is_err() {
        let _ = syscall::sys_cap_delete(slot);
        return error();
    }
    let s = &mut streams()[i];
    s.used = true;
    s.generation += 1;
    s.owner = sender;
    s.slot = slot;
    s.space = space;
    s.rate = rate;
    s.channels = channels as usize;
    s.volume = 100;
    s.head = 0;
    s.len = 0;
    s.phase = 0;
    s.played = 0;
    s.full = false;
    ok([s.generation << 8 | i as u64, 0, 0, 0, 0, 0])
}

fn write(sender: usize, space: u64, msg: &Message) -> Message {
    let Some(s) = stream_of(msg.data[0], space) else {
        return error();
    };
    let frame = s.frame();
    let room = (RING - s.len) / frame * frame;
    let want = (msg.data[1] as usize / frame * frame).min(room);
    // Into the ring where it ends, in at most two pieces.
    let mut taken = 0;
    while taken < want {
        let at = (s.head + s.len) % RING;
        let piece = (want - taken).min(RING - at);
        match syscall::sys_lent_read(sender, taken, &mut s.ring[at..at + piece]) {
            Ok(n) if n == piece => {
                s.len += piece;
                taken += piece;
            }
            _ => return error(),
        }
    }
    if taken < msg.data[1] as usize / frame * frame {
        s.full = true;
    }
    ok([taken as u64, 0, 0, 0, 0, 0])
}

fn ok(data: [u64; 6]) -> Message {
    Message { sender: 0, tag: TAG_OK, data }
}

fn error() -> Message {
    Message { sender: 0, tag: TAG_ERROR, data: [0; 6] }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    if nameserver::register(b"sound").is_err() {
        println!("[sound] could not register");
        syscall::sys_exit_code(1);
    }
    let mut card: Option<Output> = look_for_card();
    let mut looks = 60;

    loop {
        let mut msg = Message::empty();
        let heard = if card.is_some() || looks == 0 {
            syscall::sys_recv(TID_ANY, &mut msg).is_ok()
        } else {
            syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(1_000_000_000)).is_ok()
        };
        if !heard {
            if card.is_none() && looks > 0 {
                looks -= 1;
                card = look_for_card();
            }
            continue;
        }
        if let Some(gone) = space_death_notice(&msg) {
            for s in streams().iter_mut().filter(|s| s.used && s.space == gone) {
                close(s);
            }
            continue;
        }
        if msg.sender == 0 {
            // The card played a period.
            if msg.tag == TAG_NOTIFICATION && card.as_ref().is_some_and(|c| !top_up(c)) {
                card = None;
            }
            continue;
        }
        let space = syscall::sys_task_space(msg.sender).unwrap_or(0);
        let reply = match msg.tag {
            TAG_PING => Message { sender: 0, tag: TAG_PING, data: [0; 6] },
            sound::TAG_OPEN => open(msg.sender, space, &msg, &mut card),
            sound::TAG_WRITE => write(msg.sender, space, &msg),
            sound::TAG_STATUS => match stream_of(msg.data[0], space) {
                Some(s) => {
                    let played = card.as_ref().and_then(|c| c.position()).map_or(0, |(frames, _)| frames);
                    ok([s.frames() as u64, s.played, played, 0, 0, 0])
                }
                None => error(),
            },
            sound::TAG_CLOSE => match stream_of(msg.data[0], space) {
                Some(s) => {
                    close(s);
                    ok([0; 6])
                }
                None => error(),
            },
            sound::TAG_VOLUME => {
                let set = msg.data[1];
                if set != ASK && set > 100 {
                    error()
                } else if msg.data[0] == ALL {
                    if set != ASK {
                        unsafe { MASTER = set };
                    }
                    ok([unsafe { MASTER }, 0, 0, 0, 0, 0])
                } else {
                    match stream_of(msg.data[0], space) {
                        Some(s) => {
                            if set != ASK {
                                s.volume = set;
                            }
                            ok([s.volume, 0, 0, 0, 0, 0])
                        }
                        None => error(),
                    }
                }
            }
            _ => error(),
        };
        let _ = syscall::sys_reply(msg.sender, &reply);
        // A write may have given the card something to play at once.
        if msg.tag == sound::TAG_WRITE && card.as_ref().is_some_and(|c| !top_up(c)) {
            card = None;
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[sound] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
