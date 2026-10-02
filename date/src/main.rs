#![no_std]
#![no_main]

//! What the date is, and saying what it is.
//!
//! ```text
//! date                          the date and the time, UTC
//! date +FORMAT                  the same, as FORMAT says
//! date -s 2026-10-02 18:30:00   set them — for an account that may
//! ```
//!
//! FORMAT is the handful of `date(1)`'s that a script reaches for: `%Y %m
//! %d %H %M %S`, `%s` for seconds since 1970, `%N` for the nanoseconds of
//! the second, `%F` and `%T` for the date and the time whole, `%%`.
//!
//! The clock is the machine's, in UTC, and there is no time zone to print
//! it in another. Setting it is a capability (`Clock`), which a session
//! holds if its account has the `clock` right; the kernel writes the new
//! date to the clock that keeps time while the machine is off.

use quark_rt::calendar::Date;
use quark_rt::manifest::CapReq;
use quark_rt::{args, print, println, syscall};

quark_rt::manifest!([CapReq::clock()]);

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn usage() -> ! {
    println!("usage: date [+FORMAT]");
    println!("       date -s YYYY-MM-DD [HH:MM[:SS]]");
    println!("  FORMAT: %Y %m %d %H %M %S %s %N %F %T %%");
    syscall::sys_exit_code(2);
}

/// A number written in decimal digits and nothing else.
fn number(text: &[u8]) -> Option<u32> {
    if text.is_empty() || text.len() > 9 {
        return None;
    }
    text.iter().try_fold(0u32, |n, &b| b.is_ascii_digit().then(|| n * 10 + (b - b'0') as u32))
}

/// `text` as three numbers with `between` between them; the third is 0 if
/// `optional` and it is not there.
fn three(text: &[u8], between: u8, optional: bool) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(|&b| b == between);
    let first = number(parts.next()?)?;
    let second = number(parts.next()?)?;
    let third = match parts.next() {
        Some(part) => number(part)?,
        None if optional => 0,
        None => return None,
    };
    parts.next().is_none().then_some((first, second, third))
}

/// The date a `-s` was given: `YYYY-MM-DD`, and after it — as a second
/// word, or after a `T` or a space in the same one — `HH:MM` or `HH:MM:SS`.
/// A day with no time is the start of it.
fn given(day: &[u8], time: Option<&[u8]>) -> Option<Date> {
    let (day, time) = match time {
        Some(time) => (day, Some(time)),
        None => match day.iter().position(|&b| b == b'T' || b == b' ') {
            Some(at) => (&day[..at], Some(&day[at + 1..])),
            None => (day, None),
        },
    };
    let (year, month, day) = three(day, b'-', false)?;
    let (hour, minute, second) = match time {
        Some(time) => three(time, b':', true)?,
        None => (0, 0, 0),
    };
    Some(Date { year: year as i64, month, day, hour, minute, second })
}

fn set(day: &[u8], time: Option<&[u8]>) -> ! {
    let Some(seconds) = given(day, time).and_then(|date| date.to_unix()) else {
        println!("date: that is not a date: YYYY-MM-DD, and HH:MM or HH:MM:SS after it");
        syscall::sys_exit_code(2);
    };
    // Setting the clock is a capability, and a session holds it or does
    // not. Said, rather than tried and left unexplained.
    let me = syscall::sys_getpid() as usize;
    let may = (0..64).any(|slot| {
        matches!(syscall::sys_cap_read(me, slot), Ok(c) if c.valid && c.cap_type == syscall::CAP_TYPE_CLOCK)
    });
    if !may {
        println!("date: this account may not set the clock");
        syscall::sys_exit_code(1);
    }
    if syscall::sys_clock_set(seconds.saturating_mul(1_000_000_000)).is_err() {
        println!("date: the clock cannot be set to that: it keeps dates from 1970 to 2199");
        syscall::sys_exit_code(1);
    }
    show(None);
    syscall::sys_exit_code(0);
}

/// Print the date: as `format` says, or the way `date(1)` does with none.
fn show(format: Option<&[u8]>) {
    let now = syscall::unix_ns();
    let date = Date::from_unix(now / 1_000_000_000);
    let nanos = now % 1_000_000_000;
    let Some(format) = format else {
        println!(
            "{} {} {:2} {:02}:{:02}:{:02} UTC {}",
            WEEKDAYS[date.weekday() as usize],
            MONTHS[date.month as usize - 1],
            date.day,
            date.hour,
            date.minute,
            date.second,
            date.year
        );
        return;
    };
    let mut bytes = format.iter();
    while let Some(&b) = bytes.next() {
        if b != b'%' {
            print!("{}", b as char);
            continue;
        }
        match bytes.next() {
            Some(b'Y') => print!("{}", date.year),
            Some(b'm') => print!("{:02}", date.month),
            Some(b'd') => print!("{:02}", date.day),
            Some(b'H') => print!("{:02}", date.hour),
            Some(b'M') => print!("{:02}", date.minute),
            Some(b'S') => print!("{:02}", date.second),
            Some(b's') => print!("{}", now / 1_000_000_000),
            Some(b'N') => print!("{:09}", nanos),
            Some(b'F') => print!("{}-{:02}-{:02}", date.year, date.month, date.day),
            Some(b'T') => print!("{:02}:{:02}:{:02}", date.hour, date.minute, date.second),
            Some(b'%') => print!("%"),
            Some(&other) => print!("%{}", other as char),
            None => print!("%"),
        }
    }
    println!();
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    match (args::argv(1), args::argc()) {
        (None, _) => show(None),
        (Some(b"-s") | Some(b"--set"), 3) => set(args::argv(2).unwrap_or(b""), None),
        (Some(b"-s") | Some(b"--set"), 4) => set(args::argv(2).unwrap_or(b""), args::argv(3)),
        (Some(format), 2) if format.first() == Some(&b'+') => show(Some(&format[1..])),
        _ => usage(),
    }
    if syscall::sys_clock_wall() == 0 {
        // What was printed is how long the machine has been on.
        println!("date: this machine has no clock to say; that is the time since it was started");
        syscall::sys_exit_code(1);
    }
    syscall::sys_exit_code(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("date: PANIC: {}", info);
    syscall::sys_exit_code(1);
}
