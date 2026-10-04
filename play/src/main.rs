#![no_std]
#![no_main]

//! `play FILE`: a WAV file — PCM, sixteen bits, one channel or two, at any
//! rate the sound server takes.
//! `play -t HZ [HZ ...] [-s SECONDS] [-v VOLUME]`: tones, one stream each,
//! for a second unless said, at full volume unless said (0 to 100).
//!
//! Through the sound server (`quark_rt::sound`), which mixes whatever is
//! playing. Done when everything has been mixed: what is in the card is
//! played after this has gone.

use quark_rt::sound::Stream;
use quark_rt::{args, nameserver, println, syscall, vfs};

/// A quarter of a sine wave, 256 steps and the top, out of 32767.
const QUARTER: [i16; 257] = [
    0, 201, 402, 603, 804, 1005, 1206, 1407, 1608, 1809, 2009, 2210,
    2410, 2611, 2811, 3012, 3212, 3412, 3612, 3811, 4011, 4210, 4410, 4609,
    4808, 5007, 5205, 5404, 5602, 5800, 5998, 6195, 6393, 6590, 6786, 6983,
    7179, 7375, 7571, 7767, 7962, 8157, 8351, 8545, 8739, 8933, 9126, 9319,
    9512, 9704, 9896, 10087, 10278, 10469, 10659, 10849, 11039, 11228, 11417, 11605,
    11793, 11980, 12167, 12353, 12539, 12725, 12910, 13094, 13279, 13462, 13645, 13828,
    14010, 14191, 14372, 14553, 14732, 14912, 15090, 15269, 15446, 15623, 15800, 15976,
    16151, 16325, 16499, 16673, 16846, 17018, 17189, 17360, 17530, 17700, 17869, 18037,
    18204, 18371, 18537, 18703, 18868, 19032, 19195, 19357, 19519, 19680, 19841, 20000,
    20159, 20317, 20475, 20631, 20787, 20942, 21096, 21250, 21403, 21554, 21705, 21856,
    22005, 22154, 22301, 22448, 22594, 22739, 22884, 23027, 23170, 23311, 23452, 23592,
    23731, 23870, 24007, 24143, 24279, 24413, 24547, 24680, 24811, 24942, 25072, 25201,
    25329, 25456, 25582, 25708, 25832, 25955, 26077, 26198, 26319, 26438, 26556, 26674,
    26790, 26905, 27019, 27133, 27245, 27356, 27466, 27575, 27683, 27790, 27896, 28001,
    28105, 28208, 28310, 28411, 28510, 28609, 28706, 28803, 28898, 28992, 29085, 29177,
    29268, 29358, 29447, 29534, 29621, 29706, 29791, 29874, 29956, 30037, 30117, 30195,
    30273, 30349, 30424, 30498, 30571, 30643, 30714, 30783, 30852, 30919, 30985, 31050,
    31113, 31176, 31237, 31297, 31356, 31414, 31470, 31526, 31580, 31633, 31685, 31736,
    31785, 31833, 31880, 31926, 31971, 32014, 32057, 32098, 32137, 32176, 32213, 32250,
    32285, 32318, 32351, 32382, 32412, 32441, 32469, 32495, 32521, 32545, 32567, 32589,
    32609, 32628, 32646, 32663, 32678, 32692, 32705, 32717, 32728, 32737, 32745, 32752,
    32757, 32761, 32765, 32766, 32767,
];

/// The wave at `phase`, a whole turn being 2^32.
fn sine(phase: u32) -> i32 {
    let i = (phase >> 22) as usize; // 0 to 1023
    let (quarter, step) = (i / 256, i % 256);
    match quarter {
        0 => QUARTER[step] as i32,
        1 => QUARTER[256 - step] as i32,
        2 => -(QUARTER[step] as i32),
        _ => -(QUARTER[256 - step] as i32),
    }
}

fn number(text: &[u8]) -> Option<u64> {
    if text.is_empty() || text.len() > 6 {
        return None;
    }
    text.iter().try_fold(0u64, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u64))
}

fn fail(why: &str) -> ! {
    println!("play: {}", why);
    syscall::sys_exit_code(1);
}

fn usage() -> ! {
    println!("usage: play FILE | play -t HZ [HZ ...] [-s SECONDS] [-v VOLUME]");
    syscall::sys_exit_code(2);
}

const RATE: u64 = 48_000;
/// What is made at once: a tenth of a second.
const CHUNK: usize = 4800;

/// Tones, each its own stream, written a tenth of a second at a time in
/// turn, so that they are mixed rather than played one after the other.
fn tones(hz: &[u64], seconds: u64, volume: u64) -> ! {
    let mut streams: [Option<Stream>; 4] = [None, None, None, None];
    for (i, _) in hz.iter().enumerate() {
        match Stream::open(RATE, 2) {
            Ok(s) => {
                if volume != 100 {
                    let _ = s.set_volume(volume);
                }
                streams[i] = Some(s);
            }
            Err(why) => fail(why),
        }
    }
    let mut phase = [0u32; 4];
    let mut frames = [0u8; CHUNK * 4];
    let total = seconds * RATE;
    let mut made = 0u64;
    while made < total {
        let n = (CHUNK as u64).min(total - made) as usize;
        for (i, &f) in hz.iter().enumerate() {
            let step = ((f << 32) / RATE) as u32;
            for k in 0..n {
                // Half of full scale, so that four together cannot clip.
                let v = (sine(phase[i]) / 2) as i16;
                phase[i] = phase[i].wrapping_add(step);
                frames[4 * k..4 * k + 2].copy_from_slice(&v.to_le_bytes());
                frames[4 * k + 2..4 * k + 4].copy_from_slice(&v.to_le_bytes());
            }
            if let Some(s) = &streams[i] {
                if !s.write_all(&frames[..n * 4]) {
                    fail("the sound server stopped taking it");
                }
            }
        }
        made += n as u64;
    }
    for s in streams.iter().flatten() {
        s.drain();
    }
    syscall::sys_exit_code(0);
}

fn read_le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |n, &x| n << 8 | x as u64)
}

/// A WAV file: its "fmt " chunk said, and its "data" streamed.
fn file(path: &[u8]) -> ! {
    let Some(vfs_tid) = nameserver::lookup(b"vfs") else {
        fail("no file server");
    };
    let (handle, size, is_dir) = match vfs::open(vfs_tid, path) {
        Ok(h) => h,
        Err(code) => {
            println!("play: {}: {}", core::str::from_utf8(path).unwrap_or("?"), vfs::why(code));
            syscall::sys_exit_code(1);
        }
    };
    if is_dir {
        fail("that is a directory");
    }
    let mut head = [0u8; 12];
    if vfs::read(vfs_tid, handle, &mut head, 0) != Ok(12) || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        fail("not a WAV file");
    }
    // The chunks, to "data", with "fmt " on the way.
    let mut at = 12u32;
    let mut format: Option<(u64, u64)> = None;
    let (start, length) = loop {
        let mut chunk = [0u8; 8];
        if at + 8 > size || vfs::read(vfs_tid, handle, &mut chunk, at) != Ok(8) {
            fail("no sound in the file");
        }
        let len = read_le(&chunk[4..8]) as u32;
        if &chunk[0..4] == b"fmt " {
            let mut fmt = [0u8; 16];
            if len < 16 || vfs::read(vfs_tid, handle, &mut fmt, at + 8) != Ok(16) {
                fail("the file's format is not one this reads");
            }
            let (kind, channels, rate, bits) =
                (read_le(&fmt[0..2]), read_le(&fmt[2..4]), read_le(&fmt[4..8]), read_le(&fmt[14..16]));
            if kind != 1 || bits != 16 || !(channels == 1 || channels == 2) {
                fail("only sixteen-bit PCM, one channel or two");
            }
            format = Some((rate, channels));
        } else if &chunk[0..4] == b"data" {
            break (at + 8, len.min(size.saturating_sub(at + 8)));
        }
        at = at.saturating_add(8 + len + (len & 1));
    };
    let Some((rate, channels)) = format else {
        fail("the file says nothing of its format");
    };
    let stream = match Stream::open(rate, channels) {
        Ok(s) => s,
        Err(why) => fail(why),
    };
    let mut buf = [0u8; 16384];
    let mut done = 0u32;
    while done < length {
        let want = (buf.len() as u32).min(length - done) as usize;
        let got = match vfs::read(vfs_tid, handle, &mut buf[..want], start + done) {
            Ok(n) if n > 0 => n as usize,
            _ => break,
        };
        if !stream.write_all(&buf[..got]) {
            fail("the sound server stopped taking it");
        }
        done += got as u32;
    }
    let _ = vfs::close(vfs_tid, handle);
    stream.drain();
    syscall::sys_exit_code(0);
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(first) = args::argv(1) else {
        usage();
    };
    if first != b"-t" {
        if args::argc() != 2 {
            usage();
        }
        file(first);
    }
    let mut hz = [0u64; 4];
    let mut count = 0;
    let (mut seconds, mut volume) = (1u64, 100u64);
    let mut i = 2;
    while let Some(arg) = args::argv(i) {
        match arg {
            b"-s" => {
                seconds = args::argv(i + 1).and_then(number).filter(|&s| s > 0 && s <= 3600).unwrap_or_else(|| usage());
                i += 2;
            }
            b"-v" => {
                volume = args::argv(i + 1).and_then(number).filter(|&v| v <= 100).unwrap_or_else(|| usage());
                i += 2;
            }
            _ => {
                let f = number(arg).filter(|&f| f > 0 && f < RATE / 2).unwrap_or_else(|| usage());
                if count == hz.len() {
                    usage();
                }
                hz[count] = f;
                count += 1;
                i += 1;
            }
        }
    }
    if count == 0 {
        usage();
    }
    tones(&hz[..count], seconds, volume);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("play: {}", info);
    syscall::sys_exit_code(1);
}
