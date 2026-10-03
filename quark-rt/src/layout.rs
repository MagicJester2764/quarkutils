//! Where a program puts what it has, chosen at random: an address learned
//! from one run of a program is no use against the next — what Linux calls
//! address-space layout randomisation. It is done where the choosing is,
//! which is here: a program names every address it maps, and the kernel
//! chooses none of them but the first program's stack. Each place is chosen
//! once, a random number of pages into a window of its own that nothing
//! else uses (`SYS_GETRANDOM`), and does not move afterwards. What is not
//! moved is the program itself, linked where it runs, and the page its
//! arguments are on, which is where it looks for them.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::syscall;

const PAGE_SIZE: usize = 4096;

/// A number of pages from nought to `window`, at random. Nought, if there
/// is no randomness to be had: the place is then where it always was.
pub fn random_pages(window: usize) -> usize {
    let mut chance = [0u8; 8];
    if syscall::sys_getrandom(&mut chance).is_err() || window == 0 {
        return 0;
    }
    (u64::from_le_bytes(chance) % window as u64) as usize
}

/// The place kept in `at`, chosen the first time it is asked for: `base`
/// moved a random number of pages, up to `window`, up or down. Two threads
/// asking at once are given the one place.
pub fn chosen(at: &AtomicUsize, base: usize, window: usize, up: bool) -> usize {
    let now = at.load(Ordering::Acquire);
    if now != 0 {
        return now;
    }
    let offset = random_pages(window) * PAGE_SIZE;
    let place = if up { base + offset } else { base - offset };
    match at.compare_exchange(0, place, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => place,
        Err(first) => first,
    }
}
