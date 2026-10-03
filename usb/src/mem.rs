//! Memory the controller reads and writes: whole pages of this program's own
//! (`sys_phys_alloc`), so that behind an IOMMU they are what the controller
//! reaches, mapped one after another from `BASE` — below four gigabytes if
//! the controller says it cannot address more. A page given back is kept
//! for the next device rather than returned: devices come and go, and
//! frames and mappings are cheaper kept than made again.

use quark_rt::sync::Mutex;
use quark_rt::syscall;

const BASE: usize = 0x86_0000_0000;
/// As many pages as the manifest asks for.
pub const MAX_PAGES: usize = 256;
const KEPT: usize = 64;

/// A page: where this program sees it, and where the controller does.
#[derive(Clone, Copy, Default)]
pub struct Page {
    pub virt: usize,
    pub phys: u64,
}

impl Page {
    pub fn zero(&self) {
        unsafe { core::ptr::write_bytes(self.virt as *mut u8, 0, 4096) };
    }

    pub fn read8(&self, at: usize) -> u8 {
        unsafe { core::ptr::read_volatile((self.virt + at) as *const u8) }
    }

    pub fn read16(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.read8(at), self.read8(at + 1)])
    }

    pub fn read32(&self, at: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.virt + at) as *const u32) }
    }

    pub fn write8(&self, at: usize, v: u8) {
        unsafe { core::ptr::write_volatile((self.virt + at) as *mut u8, v) };
    }

    pub fn write32(&self, at: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.virt + at) as *mut u32, v) };
    }

    pub fn write64(&self, at: usize, v: u64) {
        self.write32(at, v as u32);
        self.write32(at + 4, (v >> 32) as u32);
    }

    /// `len` bytes from `at`, copied out.
    pub fn copy_out(&self, at: usize, into: &mut [u8]) {
        for (i, b) in into.iter_mut().enumerate() {
            *b = self.read8(at + i);
        }
    }
}

struct Pool {
    used: usize,
    kept: [Page; KEPT],
    nkept: usize,
    low: bool,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { used: 0, kept: [Page { virt: 0, phys: 0 }; KEPT], nkept: 0, low: false });

/// Whether pages must be below four gigabytes, as the controller says.
pub fn set_low(low: bool) {
    POOL.lock().low = low;
}

/// A page of zeroes, or `None` when there are no more.
pub fn page() -> Option<Page> {
    let mut pool = POOL.lock();
    if pool.nkept > 0 {
        pool.nkept -= 1;
        let page = pool.kept[pool.nkept];
        page.zero();
        return Some(page);
    }
    if pool.used == MAX_PAGES {
        return None;
    }
    let frame = if pool.low { syscall::sys_phys_alloc_low(1) } else { syscall::sys_phys_alloc(1) }.ok()?;
    let virt = BASE + pool.used * 4096;
    syscall::sys_map_phys(frame, virt, 1).ok()?;
    pool.used += 1;
    let page = Page { virt, phys: frame as u64 };
    page.zero();
    Some(page)
}

/// Keep `page` for the next that is asked for. One past what can be kept
/// stays mapped and unused.
pub fn give(page: Page) {
    let mut pool = POOL.lock();
    if page.virt != 0 && pool.nkept < KEPT {
        let n = pool.nkept;
        pool.kept[n] = page;
        pool.nkept = n + 1;
    }
}
