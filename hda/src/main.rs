#![no_std]
#![no_main]

//! Intel's High Definition Audio: the controller a PC has had for its sound
//! since 2004 — QEMU's `intel-hda` and `ich9-intel-hda` — and a codec
//! behind it.
//!
//! Started by the device manager for one, holding it. The controller talks
//! to its codecs through two rings in memory, commands out (the CORB) and
//! answers back (the RIRB), and moves sound through a *stream*: a list of
//! buffers it reads round and round, saying when it has finished each. This
//! finds the first codec's audio function, a pin that can play and the
//! converter behind it, turns the way between them on, and plays one stream
//! of four periods out of it, sixteen-bit stereo at 48 kHz. What it serves
//! is `quark_rt::pcm`. Recording, and more than one output at once, are not
//! done.

use core::ptr::{read_volatile, write_volatile};
use quark_rt::manifest::CapReq;
use quark_rt::pcm::{self, Card};
use quark_rt::{pci, println, syscall};

// A driver's band; any HD audio controller; frames for its rings, its
// buffer list and its four periods.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives_class(0x04, 0x03),
    CapReq::phys_alloc(8),
]);

/// Where the registers are mapped and the slot their range is minted in;
/// where an MSI-X table would be.
const REGS_AT: usize = 0xA0_0000_0000;
const REGS_SLOT: usize = 10;
const TABLE_AT: usize = 0xA1_0000_0000;
const TABLE_SLOT: usize = 11;
/// The two rings, in one page: commands, then answers half way.
const RINGS_AT: usize = 0x86_0000_0000;
const RIRB_OFFSET: usize = 0x800;
/// The buffer list, a page of its own (it is to be 128-byte aligned).
const BDL_AT: usize = 0x86_0000_1000;
/// The periods.
const BUFFER_AT: usize = 0x86_0000_2000;

const PERIOD: usize = 4096;
const PERIODS: usize = 4;

// The controller's registers (HD Audio specification, §3.3).
const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const STATESTS: usize = 0x0E;
const INTCTL: usize = 0x20;
const INTSTS: usize = 0x24;
const CORBLBASE: usize = 0x40;
const CORBUBASE: usize = 0x44;
const CORBWP: usize = 0x48;
const CORBRP: usize = 0x4A;
const CORBCTL: usize = 0x4C;
const CORBSIZE: usize = 0x4E;
const RIRBLBASE: usize = 0x50;
const RIRBUBASE: usize = 0x54;
const RIRBWP: usize = 0x58;
const RINTCNT: usize = 0x5A;
const RIRBCTL: usize = 0x5C;
const RIRBSTS: usize = 0x5D;
const RIRBSIZE: usize = 0x5E;
/// The stream descriptors, input first, 0x20 each.
const STREAMS: usize = 0x80;
// A stream descriptor's own.
const SD_CTL: usize = 0x00;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

const GCTL_CRST: u32 = 1;
const CORBRP_RST: u16 = 1 << 15;
const RIRBWP_RST: u16 = 1 << 15;
const RING_RUN: u8 = 1 << 1;
const RIRB_RINTCTL: u8 = 1 << 0;
const INTCTL_GIE: u32 = 1 << 31;
const SD_SRST: u32 = 1 << 0;
const SD_RUN: u32 = 1 << 1;
const SD_IOCE: u32 = 1 << 2;
/// What a stream says when it has finished a buffer, and two errors.
const SD_BCIS: u8 = 1 << 2;
const SD_ERRORS: u8 = 1 << 3 | 1 << 4;
/// The stream's tag, which the converter is told to play.
const TAG: u32 = 1;
/// 48 kHz, sixteen bits, two channels.
const FORMAT: u16 = 0x0011;

// Verbs (§7.3).
const GET_PARAMETER: u32 = 0xF00;
const GET_CONNECTION: u32 = 0xF02;
const GET_CONFIG: u32 = 0xF1C;
const SET_SELECT: u32 = 0x701;
const SET_POWER: u32 = 0x705;
const SET_STREAM: u32 = 0x706;
const SET_PIN: u32 = 0x707;
const SET_EAPD: u32 = 0x70C;
/// Four-bit verbs, with sixteen bits of payload.
const SET_FORMAT: u32 = 0x2;
const SET_AMP: u32 = 0x3;
// Parameters.
const P_NODES: u32 = 0x04;
const P_GROUP: u32 = 0x05;
const P_WIDGET: u32 = 0x09;
const P_PIN: u32 = 0x0C;
const P_IN_AMP: u32 = 0x0D;
const P_CONNECTIONS: u32 = 0x0E;
const P_OUT_AMP: u32 = 0x12;

// What a widget is (bits 23:20 of its capabilities).
const W_OUTPUT: u32 = 0;
const W_INPUT: u32 = 1;
const W_MIXER: u32 = 2;
const W_PIN: u32 = 4;

/// Widgets this keeps track of, and the longest way from a pin to a
/// converter it follows.
const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 6;

fn read8(regs: usize, at: usize) -> u8 {
    unsafe { read_volatile((regs + at) as *const u8) }
}
fn read16(regs: usize, at: usize) -> u16 {
    unsafe { read_volatile((regs + at) as *const u16) }
}
fn read32(regs: usize, at: usize) -> u32 {
    unsafe { read_volatile((regs + at) as *const u32) }
}
fn write8(regs: usize, at: usize, v: u8) {
    unsafe { write_volatile((regs + at) as *mut u8, v) }
}
fn write16(regs: usize, at: usize, v: u16) {
    unsafe { write_volatile((regs + at) as *mut u16, v) }
}
fn write32(regs: usize, at: usize, v: u32) {
    unsafe { write_volatile((regs + at) as *mut u32, v) }
}

/// Wait up to `tries` tenths of a millisecond for `done`.
fn until(tries: usize, mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..tries {
        if done() {
            return true;
        }
        syscall::sleep_ns(100_000);
    }
    done()
}

/// `count` pages in a row of this program's own memory at `at`, cleared:
/// where they are.
fn pages(count: usize, at: usize) -> Option<u64> {
    let first = syscall::sys_phys_alloc(count).ok()?;
    syscall::sys_map_phys(first, at, count).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, count * 4096) };
    Some(first as u64)
}

/// The size field of a ring and how many entries that is: the most it can
/// have of 256, 16 and 2.
fn ring_size(caps: u8) -> (u8, usize) {
    if caps & 0x40 != 0 {
        (2, 256)
    } else if caps & 0x20 != 0 {
        (1, 16)
    } else {
        (0, 2)
    }
}

struct Hda {
    regs: usize,
    irq: u8,
    line: bool,
    codec: u32,
    corb_wp: usize,
    corb_entries: usize,
    rirb_rp: usize,
    rirb_entries: usize,
    /// The output stream's descriptor.
    sd: usize,
    /// The period being played, the next to write, and which have been
    /// written and not yet played.
    playing: usize,
    next: usize,
    filled: [bool; PERIODS],
    /// Periods played since the card was claimed, and of silence since the
    /// last that had something in it.
    finished: u64,
    quiet: usize,
    running: bool,
    /// Where the buffer list is.
    bdl: u64,
}

impl Hda {
    /// Say `verb` to node `node` of the codec, and wait for its answer.
    fn command(&mut self, node: u32, verb: u32) -> Option<u32> {
        let wp = (self.corb_wp + 1) % self.corb_entries;
        unsafe { write_volatile((RINGS_AT + wp * 4) as *mut u32, self.codec << 28 | node << 20 | verb) };
        self.corb_wp = wp;
        write16(self.regs, CORBWP, wp as u16);
        let regs = self.regs;
        for _ in 0..1000 {
            let written = read16(regs, RIRBWP) as usize % self.rirb_entries;
            while self.rirb_rp != written {
                self.rirb_rp = (self.rirb_rp + 1) % self.rirb_entries;
                let at = RINGS_AT + RIRB_OFFSET + self.rirb_rp * 8;
                let (answer, extra) = unsafe { (read_volatile(at as *const u32), read_volatile((at + 4) as *const u32)) };
                // Heard: the controller goes on to the next command only
                // once it has been told so.
                write8(regs, RIRBSTS, 0x05);
                if extra & 0x10 == 0 {
                    return Some(answer);
                }
            }
            syscall::sleep_ns(10_000);
        }
        None
    }

    fn parameter(&mut self, node: u32, which: u32) -> u32 {
        self.command(node, GET_PARAMETER << 8 | which).unwrap_or(0)
    }

    /// Node `node`'s connections, as many as `into` holds: how many.
    fn connections(&mut self, node: u32, into: &mut [u32; 16]) -> usize {
        let list = self.parameter(node, P_CONNECTIONS);
        let (length, long) = ((list & 0x7F) as usize, list & 0x80 != 0);
        let (per, bits) = if long { (2, 16) } else { (4, 8) };
        let mut count = 0;
        let mut previous = 0u32;
        let mut i = 0;
        while i < length && count < into.len() {
            let Some(word) = self.command(node, GET_CONNECTION << 8 | i as u32) else {
                break;
            };
            for j in 0..per {
                if i + j >= length || count >= into.len() {
                    break;
                }
                let entry = (word >> (j * bits)) & ((1 << bits) - 1);
                let (id, range) = (entry & ((1 << (bits - 1)) - 1), entry & (1 << (bits - 1)) != 0);
                if range && previous != 0 {
                    // Everything between the last and this one.
                    let mut n = previous + 1;
                    while n <= id && count < into.len() {
                        into[count] = n;
                        count += 1;
                        n += 1;
                    }
                } else {
                    into[count] = id;
                    count += 1;
                }
                previous = id;
            }
            i += per;
        }
        count
    }

    /// The gain of an amplifier (`P_OUT_AMP` or `P_IN_AMP`) that is no gain
    /// at all — its offset — from the widget or, where it says to look
    /// there, its function group.
    fn unity(&mut self, node: u32, caps: u32, group: u32, which: u32) -> u32 {
        let amp = if caps & 1 << 3 != 0 { self.parameter(node, which) } else { self.parameter(group, which) };
        amp & 0x7F
    }

    /// Find a way from `node` back to a converter, through widgets that
    /// pass sound on: the nodes of it, the converter last, and how many.
    fn find_way(&mut self, node: u32, kinds: &[u32; MAX_NODES], first: u32, way: &mut [u32; MAX_DEPTH], depth: usize) -> usize {
        if depth >= MAX_DEPTH {
            return 0;
        }
        way[depth] = node;
        let kind = kinds[(node - first) as usize % MAX_NODES];
        if kind == W_OUTPUT {
            return depth + 1;
        }
        let mut next = [0u32; 16];
        let count = self.connections(node, &mut next);
        for &n in &next[..count] {
            let known = n >= first && n < first + MAX_NODES as u32;
            if known && kinds[(n - first) as usize] != W_INPUT && kinds[(n - first) as usize] != W_PIN {
                let found = self.find_way(n, kinds, first, way, depth + 1);
                if found > 0 {
                    return found;
                }
            }
        }
        0
    }

    /// The first codec's audio function: a pin that can play, and the way
    /// to it from a converter, turned on and turned up. The converter.
    fn open_codec(&mut self) -> Option<u32> {
        let root = self.parameter(0, P_NODES);
        let (start, count) = ((root >> 16) & 0xFF, root & 0xFF);
        let group = (start..start + count).find(|&n| self.parameter(n, P_GROUP) & 0xFF == 0x01)?;
        let _ = self.command(group, SET_POWER << 8);
        let nodes = self.parameter(group, P_NODES);
        let (first, count) = ((nodes >> 16) & 0xFF, ((nodes & 0xFF) as usize).min(MAX_NODES));
        let mut kinds = [u32::MAX; MAX_NODES];
        let mut caps = [0u32; MAX_NODES];
        for i in 0..count {
            caps[i] = self.parameter(first + i as u32, P_WIDGET);
            kinds[i] = (caps[i] >> 20) & 0xF;
        }
        // The pins that can play and are connected to something, the
        // speaker first, then what is plugged into.
        let mut best: Option<(u32, u32, u32)> = None;
        for i in 0..count {
            let node = first + i as u32;
            if kinds[i] != W_PIN || self.parameter(node, P_PIN) & 1 << 4 == 0 {
                continue;
            }
            let config = self.command(node, GET_CONFIG << 8).unwrap_or(0);
            if config >> 30 == 1 {
                continue;
            }
            let rank = match (config >> 20) & 0xF {
                1 => 0, // a speaker
                0 => 1, // line out
                2 => 2, // headphones
                _ => 3,
            };
            if best.is_none_or(|(r, _, _)| rank < r) {
                best = Some((rank, node, config));
            }
        }
        let (_, pin, config) = best?;
        let mut way = [0u32; MAX_DEPTH];
        let length = self.find_way(pin, &kinds, first, &mut way, 0);
        if length == 0 {
            return None;
        }
        // Each widget on the way: on, listening to the next, and loud.
        for d in 0..length {
            let node = way[d];
            let i = (node - first) as usize;
            let _ = self.command(node, SET_POWER << 8);
            if d + 1 < length {
                let mut next = [0u32; 16];
                let count = self.connections(node, &mut next);
                let index = next[..count].iter().position(|&n| n == way[d + 1]).unwrap_or(0) as u32;
                if kinds[i] == W_MIXER {
                    // A mixer adds every input; this one is let through.
                    if caps[i] & 1 << 1 != 0 {
                        let gain = self.unity(node, caps[i], group, P_IN_AMP);
                        let _ = self.command(node, SET_AMP << 16 | 1 << 14 | 3 << 12 | index << 8 | gain);
                    }
                } else if count > 1 {
                    let _ = self.command(node, SET_SELECT << 8 | index);
                }
            }
            if caps[i] & 1 << 2 != 0 {
                let gain = self.unity(node, caps[i], group, P_OUT_AMP);
                let _ = self.command(node, SET_AMP << 16 | 1 << 15 | 3 << 12 | gain);
            }
        }
        let headphones = (config >> 20) & 0xF == 2;
        let _ = self.command(pin, SET_PIN << 8 | 0x40 | if headphones { 0x80 } else { 0 });
        if self.parameter(pin, P_PIN) & 1 << 16 != 0 {
            let _ = self.command(pin, SET_EAPD << 8 | 0x02);
        }
        let dac = way[length - 1];
        let _ = self.command(dac, SET_FORMAT << 16 | FORMAT as u32);
        let _ = self.command(dac, SET_STREAM << 8 | TAG << 4);
        println!("[hda] codec {}: pin {} from converter {}, {} widgets on the way", self.codec, pin, dac, length);
        Some(dac)
    }

    /// The period the stream is in, from where it is in its buffer.
    fn position(&self) -> usize {
        read32(self.regs, self.sd + SD_LPIB) as usize % (PERIOD * PERIODS) / PERIOD
    }

    /// Set the stream up from the beginning of its buffer: reset, its
    /// buffers listed, its format, its tag and its interrupt.
    fn program(&self) {
        let (regs, sd) = (self.regs, self.sd);
        write32(regs, sd + SD_CTL, SD_SRST);
        until(100, || read32(regs, sd + SD_CTL) & SD_SRST != 0);
        write32(regs, sd + SD_CTL, 0);
        until(100, || read32(regs, sd + SD_CTL) & SD_SRST == 0);
        write32(regs, sd + SD_CBL, (PERIOD * PERIODS) as u32);
        write16(regs, sd + SD_LVI, (PERIODS - 1) as u16);
        write16(regs, sd + SD_FMT, FORMAT);
        write32(regs, sd + SD_BDPL, self.bdl as u32);
        write32(regs, sd + SD_BDPU, (self.bdl >> 32) as u32);
        write32(regs, sd + SD_CTL, TAG << 20 | SD_IOCE);
    }

    /// Stop the stream where it is.
    fn halt(&mut self) {
        let (regs, sd) = (self.regs, self.sd);
        write32(regs, sd + SD_CTL, read32(regs, sd + SD_CTL) & !SD_RUN);
        until(100, || read32(regs, sd + SD_CTL) & SD_RUN == 0);
        write8(regs, sd + SD_STS, SD_BCIS | SD_ERRORS);
        self.running = false;
    }

    /// Every period silence and none written.
    fn clear(&mut self) {
        unsafe { core::ptr::write_bytes(BUFFER_AT as *mut u8, 0, PERIOD * PERIODS) };
        self.filled = [false; PERIODS];
    }
}

impl Card for Hda {
    fn periods(&self) -> (usize, usize) {
        (PERIOD, PERIODS)
    }

    fn stop(&mut self) {
        if self.running {
            self.halt();
        }
        self.clear();
        self.finished = 0;
    }

    fn write(&mut self, samples: &[u8]) -> bool {
        if samples.len() != PERIOD {
            return false;
        }
        if !self.running {
            // Begin again from the first period, with this in it.
            self.clear();
            unsafe { core::ptr::copy_nonoverlapping(samples.as_ptr(), BUFFER_AT as *mut u8, PERIOD) };
            self.filled[0] = true;
            self.playing = 0;
            self.next = 1;
            self.quiet = 0;
            self.program();
            write32(self.regs, self.sd + SD_CTL, read32(self.regs, self.sd + SD_CTL) | SD_RUN);
            self.running = true;
            return true;
        }
        // Nothing waiting to be played: begin just behind the stream, rather
        // than a whole turn of the ring away from it.
        if !self.filled.iter().any(|&f| f) {
            self.next = (self.playing + 1) % PERIODS;
        }
        let n = self.next;
        if self.filled[n] || n == self.playing {
            return false;
        }
        unsafe { core::ptr::copy_nonoverlapping(samples.as_ptr(), (BUFFER_AT + n * PERIOD) as *mut u8, PERIOD) };
        self.filled[n] = true;
        self.next = (n + 1) % PERIODS;
        true
    }

    fn free(&self) -> usize {
        if !self.running {
            return PERIODS;
        }
        let mut n = if self.filled.iter().any(|&f| f) { self.next } else { (self.playing + 1) % PERIODS };
        let mut count = 0;
        while count < PERIODS - 1 && !self.filled[n] && n != self.playing {
            count += 1;
            n = (n + 1) % PERIODS;
        }
        count
    }

    fn played(&self) -> u64 {
        let within = if self.running { read32(self.regs, self.sd + SD_LPIB) as usize % PERIOD } else { 0 };
        (self.finished * PERIOD as u64 + within as u64) / pcm::FRAME_BYTES as u64
    }

    fn interrupt(&mut self) -> usize {
        let status = read8(self.regs, self.sd + SD_STS);
        write8(self.regs, self.sd + SD_STS, status & (SD_BCIS | SD_ERRORS));
        let _ = read32(self.regs, INTSTS);
        if self.line {
            syscall::sys_irq_ack(self.irq);
        }
        if !self.running {
            return 0;
        }
        // Every period between where the stream was and where it is now has
        // been played: silence until somebody writes it again.
        let now = self.position();
        let mut done = 0;
        while self.playing != now {
            let p = self.playing;
            self.quiet = if self.filled[p] { 0 } else { self.quiet + 1 };
            unsafe { core::ptr::write_bytes((BUFFER_AT + p * PERIOD) as *mut u8, 0, PERIOD) };
            self.filled[p] = false;
            self.playing = (p + 1) % PERIODS;
            self.finished += 1;
            done += 1;
        }
        // A turn of the ring with nothing in it, and nothing waiting: stopped
        // until something is written.
        if self.quiet >= PERIODS && !self.filled.iter().any(|&f| f) {
            self.halt();
        }
        done
    }

    fn irq(&self) -> u8 {
        self.irq
    }
}

fn stop(why: &str) -> ! {
    println!("[hda] {}", why);
    syscall::sys_exit_code(1);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(device) = pci::this_device() else {
        stop("started without a controller: the device manager starts this, for HD audio.");
    };
    let Some(regs) = pci::map_bar(device, 0, REGS_AT, REGS_SLOT) else {
        stop("the controller's registers were not given");
    };
    if pci::claim(device).is_err() {
        stop("the controller is another program's");
    }
    if pci::enable(device, pci::COMMAND_MEMORY | pci::COMMAND_MASTER).is_err() {
        stop("the controller may not be turned on");
    }

    // Reset, and out of it: the codecs say they are there within half a
    // millisecond or so.
    write32(regs, GCTL, read32(regs, GCTL) & !GCTL_CRST);
    if !until(100, || read32(regs, GCTL) & GCTL_CRST == 0) {
        stop("the controller would not reset");
    }
    syscall::sleep_ns(200_000);
    write32(regs, GCTL, read32(regs, GCTL) | GCTL_CRST);
    if !until(100, || read32(regs, GCTL) & GCTL_CRST != 0) {
        stop("the controller would not come out of reset");
    }
    until(100, || read16(regs, STATESTS) & 0x7FFF != 0);
    let present = read16(regs, STATESTS) & 0x7FFF;
    if present == 0 {
        stop("no codec answers the controller");
    }
    write16(regs, STATESTS, present);
    let codec = present.trailing_zeros();
    let gcap = read16(regs, GCAP);
    let (inputs, outputs) = ((gcap >> 8 & 0xF) as usize, (gcap >> 12 & 0xF) as usize);
    if outputs == 0 {
        stop("the controller has no stream that plays");
    }

    // The rings: stopped, placed, sized, their pointers back to the start,
    // and running. The controller is told an answer was read after each,
    // and nothing interrupts for one.
    let Some(rings) = pages(1, RINGS_AT) else {
        stop("no memory for the controller's rings");
    };
    write8(regs, CORBCTL, 0);
    write8(regs, RIRBCTL, 0);
    until(100, || read8(regs, CORBCTL) & RING_RUN == 0 && read8(regs, RIRBCTL) & RING_RUN == 0);
    let (corb_size, corb_entries) = ring_size(read8(regs, CORBSIZE));
    let (rirb_size, rirb_entries) = ring_size(read8(regs, RIRBSIZE));
    write32(regs, CORBLBASE, rings as u32);
    write32(regs, CORBUBASE, (rings >> 32) as u32);
    write8(regs, CORBSIZE, corb_size);
    write16(regs, CORBWP, 0);
    // Its read pointer is reset by setting a bit and clearing it again; a
    // controller that clears it by itself is not waited for.
    write16(regs, CORBRP, CORBRP_RST);
    until(10, || read16(regs, CORBRP) & CORBRP_RST != 0);
    write16(regs, CORBRP, 0);
    until(10, || read16(regs, CORBRP) & CORBRP_RST == 0);
    let rirb = rings + RIRB_OFFSET as u64;
    write32(regs, RIRBLBASE, rirb as u32);
    write32(regs, RIRBUBASE, (rirb >> 32) as u32);
    write8(regs, RIRBSIZE, rirb_size);
    write16(regs, RIRBWP, RIRBWP_RST);
    write16(regs, RINTCNT, 1);
    write8(regs, RIRBCTL, RING_RUN | RIRB_RINTCTL);
    write8(regs, CORBCTL, RING_RUN);

    let stream = inputs;
    let sd = STREAMS + stream * 0x20;
    let mut hda = Hda {
        regs,
        irq: 0,
        line: false,
        codec,
        corb_wp: 0,
        corb_entries,
        rirb_rp: 0,
        rirb_entries,
        sd,
        playing: 0,
        next: 1,
        filled: [false; PERIODS],
        finished: 0,
        quiet: 0,
        running: false,
        bdl: 0,
    };
    if hda.open_codec().is_none() {
        stop("the codec has no pin to play through");
    }

    // The stream: reset, its buffers listed, its format, and its interrupt.
    let (Some(bdl), Some(buffer)) = (pages(1, BDL_AT), pages(PERIOD * PERIODS / 4096, BUFFER_AT)) else {
        stop("no memory for the stream");
    };
    for p in 0..PERIODS {
        let at = BDL_AT + p * 16;
        unsafe {
            write_volatile(at as *mut u64, buffer + (p * PERIOD) as u64);
            write_volatile((at + 8) as *mut u32, PERIOD as u32);
            write_volatile((at + 12) as *mut u32, 1);
        }
    }
    hda.bdl = bdl;
    hda.program();

    let Some(interrupt) = pci::interrupt(device, |bar| pci::map_bar(device, bar, TABLE_AT, TABLE_SLOT)) else {
        stop("no interrupt to be had for the controller");
    };
    hda.irq = interrupt.number();
    hda.line = interrupt.is_line();
    write32(regs, INTCTL, INTCTL_GIE | 1 << stream);

    let Some(name) = pcm::register() else {
        stop("pcm0 to pcm7 are all taken");
    };
    println!(
        "[hda] {}: interrupt {} ({}), {} periods of {} bytes",
        core::str::from_utf8(&name).unwrap_or("a card"),
        interrupt.number(),
        if interrupt.is_line() { "its line" } else { "a message of its own" },
        PERIODS,
        PERIOD
    );
    pcm::serve(&mut hda)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[hda] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
