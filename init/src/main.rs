#![no_std]
#![no_main]

extern crate alloc;

mod services;

use quark_rt::devices;
use quark_rt::ipc::Message;
use quark_rt::nameserver;
use quark_rt::spawn::{self, Scratch, Spawned};
use quark_rt::{println, syscall, vfs};
use services::{Manager, Policy, Program};

const PAGE_SIZE: usize = 4096;
const BOOT_INFO_ADDR: usize = 0x80_4000_0000;
const FILE_BUF_BASE: usize = 0x82_0000_0000;
const BOOT_IMG_BASE: usize = 0x85_0000_0000;
/// Where a program read through the VFS is staged, `MAX_IMAGE_PAGES` long.
/// Not `FILE_BUF_BASE`: the boot image path leaves its last program mapped
/// there, and staging never maps over anything.
const VFS_IMAGE_BASE: usize = 0x89_0000_0000;

// ---------------------------------------------------------------------------
// Boot info structures (matches kernel's BootInfo)
// ---------------------------------------------------------------------------

#[repr(C)]
struct BootInfo {
    module_count: u64,
    fb_addr: u64,
    fb_pitch: u32,
    fb_width: u32,
    fb_height: u32,
    fb_bpp: u8,
    fb_type: u8,
    fb_red_pos: u8,
    fb_green_pos: u8,
    fb_blue_pos: u8,
    _pad: [u8; 3],
    modules: [BootModuleDesc; 32],
}

#[repr(C)]
struct BootModuleDesc {
    phys_start: u64,
    phys_end: u64,
    name: [u8; 48],
}

// ---------------------------------------------------------------------------
// ELF64 structures
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Minimal FAT32 reader (read-only, root directory only)
// ---------------------------------------------------------------------------

struct Bpb {
    bytes_per_sector: u32,
    sectors_per_cluster: u32,
    reserved_sectors: u32,
    num_fats: u32,
    fat_size_32: u32,
    root_cluster: u32,
}

fn parse_bpb(data: &[u8]) -> Bpb {
    Bpb {
        bytes_per_sector: read_u16(data, 11) as u32,
        sectors_per_cluster: data[13] as u32,
        reserved_sectors: read_u16(data, 14) as u32,
        num_fats: data[16] as u32,
        fat_size_32: read_u32(data, 36),
        root_cluster: read_u32(data, 44),
    }
}

fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn fat_offset(bpb: &Bpb) -> usize {
    (bpb.reserved_sectors * bpb.bytes_per_sector) as usize
}

fn data_region_offset(bpb: &Bpb) -> usize {
    ((bpb.reserved_sectors + bpb.num_fats * bpb.fat_size_32) * bpb.bytes_per_sector) as usize
}

fn cluster_data_offset(bpb: &Bpb, cluster: u32) -> usize {
    data_region_offset(bpb)
        + ((cluster - 2) as usize)
            * (bpb.sectors_per_cluster * bpb.bytes_per_sector) as usize
}

fn fat_next(rootfs: &[u8], bpb: &Bpb, cluster: u32) -> Option<u32> {
    let off = fat_offset(bpb) + (cluster as usize) * 4;
    let next = read_u32(rootfs, off) & 0x0FFF_FFFF;
    if next >= 0x0FFF_FFF8 {
        None
    } else {
        Some(next)
    }
}

const MAX_DIR_ENTRIES: usize = 32;

struct RootDirEntry {
    name: [u8; 11],
    first_cluster: u32,
    file_size: u32,
}

fn scan_root_dir(rootfs: &[u8], bpb: &Bpb) -> ([RootDirEntry; MAX_DIR_ENTRIES], usize) {
    let mut entries: [RootDirEntry; MAX_DIR_ENTRIES] = unsafe { core::mem::zeroed() };
    let mut count = 0;
    let cluster_bytes = (bpb.sectors_per_cluster * bpb.bytes_per_sector) as usize;
    let mut cluster = bpb.root_cluster;

    loop {
        let base = cluster_data_offset(bpb, cluster);
        let num_entries = cluster_bytes / 32;

        for i in 0..num_entries {
            if count >= MAX_DIR_ENTRIES {
                return (entries, count);
            }

            let off = base + i * 32;
            let first_byte = rootfs[off];

            // End of directory
            if first_byte == 0x00 {
                return (entries, count);
            }
            // Deleted entry
            if first_byte == 0xE5 {
                continue;
            }

            let attr = rootfs[off + 11];
            if attr & 0x0F == 0x0F { continue; } // LFN
            if attr & 0x08 != 0 { continue; }     // volume label
            if attr & 0x10 != 0 { continue; }     // subdirectory

            let mut name = [0u8; 11];
            name.copy_from_slice(&rootfs[off..off + 11]);

            let cluster_hi = read_u16(rootfs, off + 20) as u32;
            let cluster_lo = read_u16(rootfs, off + 26) as u32;

            entries[count] = RootDirEntry {
                name,
                first_cluster: (cluster_hi << 16) | cluster_lo,
                file_size: read_u32(rootfs, off + 28),
            };
            count += 1;
        }

        match fat_next(rootfs, bpb, cluster) {
            Some(next) => cluster = next,
            None => break,
        }
    }

    (entries, count)
}

// ---------------------------------------------------------------------------
// File reading: assemble file data from cluster chain into contiguous buffer
// ---------------------------------------------------------------------------

fn read_file_to_buffer<'a>(
    rootfs: &[u8],
    bpb: &Bpb,
    first_cluster: u32,
    file_size: u32,
) -> Result<&'a [u8], ()> {
    let size = file_size as usize;
    let pages_needed = (size + PAGE_SIZE - 1) / PAGE_SIZE;

    // Allocate physical pages and map at FILE_BUF_BASE
    for p in 0..pages_needed {
        let frame = syscall::sys_phys_alloc(1)?;
        syscall::sys_map_phys(frame, FILE_BUF_BASE + p * PAGE_SIZE, 1)?;
    }

    let cluster_bytes = (bpb.sectors_per_cluster * bpb.bytes_per_sector) as usize;
    let mut cluster = first_cluster;
    let mut copied = 0usize;

    while copied < size {
        let src_off = cluster_data_offset(bpb, cluster);
        let chunk = cluster_bytes.min(size - copied);

        unsafe {
            core::ptr::copy_nonoverlapping(
                rootfs.as_ptr().add(src_off),
                (FILE_BUF_BASE + copied) as *mut u8,
                chunk,
            );
        }

        copied += chunk;

        if copied < size {
            match fat_next(rootfs, bpb, cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }
    }

    Ok(unsafe { core::slice::from_raw_parts(FILE_BUF_BASE as *const u8, size) })
}

// ---------------------------------------------------------------------------
// ELF spawning (takes pre-mapped byte slice)
// ---------------------------------------------------------------------------

/// Load an ELF into a new task but do NOT start it.
/// Call info.start() after wiring fds / granting caps.

// ---------------------------------------------------------------------------
// Program arguments
// ---------------------------------------------------------------------------

const ARGS_TEMP_PAGE: usize = 0x88_0000_0000;

/// Staging areas quark_rt::spawn maps through while building a child. init's
/// were written inline in its loader rather than named; 0x83 staged ELF pages
/// and 0x84 staged stacks.
const SPAWN_SCRATCH: Scratch = Scratch {
    elf: 0x83_0000_0000,
    stack: 0x84_0000_0000,
    args: ARGS_TEMP_PAGE,
};

/// Write program arguments into the child task's address space.
/// `args` is a list of byte slices (argv[0], argv[1], ...).
/// Allocates one physical page, writes the args layout, maps into child's CR3.

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn module_name(name: &[u8; 48]) -> &[u8] {
    let len = name.iter().position(|&b| b == 0).unwrap_or(48);
    &name[..len]
}

/// Where a small boot module is mapped to be read.
const ROOT_CFG_BASE: usize = 0x8A_0000_0000;

/// What the root filesystem is on: a block driver's name and one of its
/// volumes, as text, to hand the file server.
struct Root {
    driver: [u8; 16],
    driver_len: usize,
    volume: [u8; 8],
    volume_len: usize,
}

impl Root {
    fn driver(&self) -> &[u8] {
        &self.driver[..self.driver_len]
    }

    fn volume(&self) -> &[u8] {
        &self.volume[..self.volume_len]
    }
}

/// The boot module called `name`, whatever case the bootloader found it in.
fn find_module(name: &[u8]) -> Option<(usize, usize)> {
    let info = unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) };
    for i in 0..info.module_count as usize {
        let m = &info.modules[i];
        if module_name(&m.name).eq_ignore_ascii_case(name) {
            return Some((m.phys_start as usize, (m.phys_end - m.phys_start) as usize));
        }
    }
    None
}

/// How long the root's disk is waited for, by its partition: a USB disk's
/// driver, or a SATA disk's that is spinning up, can take seconds to say
/// it is there.
const ROOT_WAIT_MS: u64 = 30_000;

/// The disk that has partition `id`, waited for: the driver's name and the
/// volume. Each disk's driver takes the first free `diskN` when it is ready,
/// so which disk is `disk0` is a matter of which driver was quickest.
fn root_by_partition(id: &[u8; 16]) -> Option<Root> {
    let started = syscall::sys_clock();
    let mut said = false;
    loop {
        for n in 0..4u8 {
            let name = [b'd', b'i', b's', b'k', b'0' + n];
            let Some(tid) = nameserver::lookup(&name) else { continue };
            let Ok(whole) = quark_rt::block::info(tid, 0) else { continue };
            for volume in 1..whole.volumes.min(100) {
                if quark_rt::block::id(tid, volume).as_ref() == Ok(id) {
                    let mut root = Root { driver: [0; 16], driver_len: 5, volume: [0; 8], volume_len: 0 };
                    root.driver[..5].copy_from_slice(&name);
                    if volume >= 10 {
                        root.volume[root.volume_len] = b'0' + (volume / 10) as u8;
                        root.volume_len += 1;
                    }
                    root.volume[root.volume_len] = b'0' + (volume % 10) as u8;
                    root.volume_len += 1;
                    return Some(root);
                }
            }
        }
        let waited = syscall::sys_clock().wrapping_sub(started) / 1_000_000;
        if waited >= ROOT_WAIT_MS {
            println!("[init] No disk has the root's partition; the first disk is tried");
            return None;
        }
        if waited >= 1000 && !said {
            println!("[init] Waiting for the disk the root is on");
            said = true;
        }
        syscall::sleep_ms(50);
    }
}

/// Where the root is, if whoever installed this system wrote it down.
///
/// The bootloader hands over every file beside the kernel, and one of them
/// can be `root.cfg`: a line of text, `root partuuid GUID` — the partition
/// itself, on whichever disk it turns out to be — or `root DRIVER VOLUME`.
/// An image's build or an installer writes it, because only they know which
/// partition they put the system on. Without one the file server decides
/// for itself, which is right for a machine with one disk laid out the way
/// an image built on another machine is.
fn root_from_config() -> Option<Root> {
    let (phys, size) = find_module(b"root.cfg")?;
    let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    if size == 0 || pages > 1 || syscall::sys_map_phys(phys, ROOT_CFG_BASE, pages).is_err() {
        return None;
    }
    let text = unsafe { core::slice::from_raw_parts(ROOT_CFG_BASE as *const u8, size) };
    let mut found = None;
    for line in text.split(|&b| b == b'\n') {
        let mut words = line
            .split(|&b| b == b' ' || b == b'\t' || b == b'\r')
            .filter(|w| !w.is_empty());
        if words.next() != Some(&b"root"[..]) {
            continue;
        }
        let (Some(driver), Some(volume)) = (words.next(), words.next()) else { continue };
        if driver == b"partuuid" {
            if let Some(id) = quark_rt::block::guid_from_text(volume) {
                let _ = syscall::sys_munmap(ROOT_CFG_BASE, pages);
                return root_by_partition(&id);
            }
            continue;
        }
        if driver.len() > 16 || volume.len() > 8 || !volume.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let mut root = Root { driver: [0; 16], driver_len: driver.len(), volume: [0; 8], volume_len: volume.len() };
        root.driver[..driver.len()].copy_from_slice(driver);
        root.volume[..volume.len()].copy_from_slice(volume);
        found = Some(root);
    }
    let _ = syscall::sys_munmap(ROOT_CFG_BASE, pages);
    found
}

/// The boot module a system that runs from memory keeps its root in: a whole
/// filesystem, loaded by the bootloader as a file.
const LIVE_MODULE: &[u8] = b"live.img";

/// A number as `0x…`, into `buf`. Returns how much of it was used.
fn hex(mut n: usize, buf: &mut [u8; 18]) -> usize {
    let mut digits = [0u8; 16];
    let mut count = 0;
    loop {
        digits[count] = b"0123456789abcdef"[n & 0xF];
        count += 1;
        n >>= 4;
        if n == 0 {
            break;
        }
    }
    buf[0] = b'0';
    buf[1] = b'x';
    for i in 0..count {
        buf[2 + i] = digits[count - 1 - i];
    }
    2 + count
}

/// Whether a boot module's name begins with `needle`, in either case: the
/// name is a file's on a FAT partition, and comes back in capitals from one
/// this system's own tools wrote.
fn starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

fn to_upper(b: u8) -> u8 {
    if b >= b'a' && b <= b'z' { b - 32 } else { b }
}

/// Match a DirEntry against base+ext (e.g., b"LOGIN", b"ELF").
/// Works with both FAT32 8.3 names and ext2 actual names.
fn name_matches_entry(entry: &vfs::DirEntry, base: &[u8], ext: &[u8]) -> bool {
    let name = entry.name_bytes();
    // Try matching "BASE.EXT" (ext2 format)
    let expected_len = base.len() + 1 + ext.len(); // "LOGIN.ELF"
    if name.len() == expected_len {
        let mut ok = true;
        for i in 0..base.len() {
            if to_upper(name[i]) != to_upper(base[i]) { ok = false; break; }
        }
        if ok && name[base.len()] == b'.' {
            for i in 0..ext.len() {
                if to_upper(name[base.len() + 1 + i]) != to_upper(ext[i]) { ok = false; break; }
            }
            if ok { return true; }
        }
    }
    // Try matching bare name without extension (ext2 lowercase format, e.g., "login")
    if name.len() == base.len() {
        let mut ok = true;
        for i in 0..base.len() {
            if to_upper(name[i]) != to_upper(base[i]) { ok = false; break; }
        }
        if ok { return true; }
    }
    // Try FAT32 8.3 format
    if name.len() >= 11 {
        let mut fat_ok = true;
        for i in 0..8 {
            let expected = if i < base.len() { to_upper(base[i]) } else { b' ' };
            if to_upper(name[i]) != expected { fat_ok = false; break; }
        }
        if fat_ok {
            for i in 0..3 {
                let expected = if i < ext.len() { to_upper(ext[i]) } else { b' ' };
                if to_upper(name[8 + i]) != expected { fat_ok = false; break; }
            }
            if fat_ok { return true; }
        }
    }
    false
}

fn fat_name_to_buf(name: &[u8; 11], buf: &mut [u8; 16]) -> usize {
    let base_len = name[0..8]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |p| p + 1);
    let mut pos = 0;
    for i in 0..base_len {
        if pos < buf.len() {
            buf[pos] = name[i];
            pos += 1;
        }
    }
    let ext_len = name[8..11]
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |p| p + 1);
    if ext_len > 0 {
        if pos < buf.len() {
            buf[pos] = b'.';
            pos += 1;
        }
        for i in 0..ext_len {
            if pos < buf.len() {
                buf[pos] = name[8 + i];
                pos += 1;
            }
        }
    }
    pos
}

/// Check if a FAT 8.3 name is an essential boot service (loaded from boot
/// image). A device's driver is not on the list: whichever of them are in
/// the boot image are the device manager's to start.
fn is_essential_elf(name: &[u8; 11]) -> bool {
    if &name[8..11] != b"ELF" { return false; }
    let base = &name[0..8];
    base == b"NAMESRVR" || base == b"QTTY    " || base == b"KEYBOARD"
        || base == b"INPUT   " || base == b"VFS     "
        || base == b"NET     " || base == b"AUTH    "
        || base == b"SOUND   "
}

/// Where init keeps its capability to the nameserver. Every program it starts
/// is given a copy, in the same slot.
const NS_SLOT: usize = syscall::SLOT_ENDPOINT;
/// Where init keeps its capability to the framebuffer device, which it has to
/// call before the device has registered anywhere.
const FB_SLOT: usize = syscall::SLOT_ENDPOINT_EXTRA;
/// And to the device manager, which it offers drivers to before anybody is
/// let call it.
const DEVMGR_SLOT: usize = 11;

/// Give `tid` the right to call the nameserver.
///
/// That is the only endpoint a program needs to be given. It finds everything
/// else by asking there, and an answer comes with the right to call what it
/// names — so two programs can reach each other only through a name one of
/// them registered.
fn grant_endpoints(tid: usize) {
    let _ = syscall::sys_cap_grant(tid, NS_SLOT, syscall::SLOT_ENDPOINT);
}

/// Mint a cap in a temporary slot, grant it to a child task, then delete it.
/// Uses slot 14 as a scratch slot for minting.
fn mint_and_grant(tid: usize, dest_slot: usize, cap_type: u64, param0: u64, param1: u64) {
    const SCRATCH_SLOT: usize = 14;
    let _ = syscall::sys_cap_mint(SCRATCH_SLOT, cap_type, param0, param1);
    let _ = syscall::sys_cap_grant(tid, SCRATCH_SLOT, dest_slot);
    let _ = syscall::sys_cap_delete(SCRATCH_SLOT);
}

/// Grant capabilities based on FAT 8.3 name using fine-grained object capabilities.
/// Grant a freshly loaded program the capabilities its manifest asks for.
///
/// This used to be a chain of `base == b"KEYBOARD"` comparisons — eleven
/// branches naming every program the tree shipped. A package installed later
/// had no branch and could not be given one without rebuilding init, which is
/// not a thing a distro can live with.
///
/// The program now says what it needs and init reads it out of the image. init
/// holds every capability, so it can satisfy any request; a spawner that holds
/// less simply cannot mint what it does not have, and the kernel enforces that
/// rather than trusting the caller.
fn grant_caps_from_manifest(image: &[u8], tid: usize) {
    // Everything init starts may call the nameserver. Without this no program
    // could even look up a name, since the nameserver is itself an IPC
    // destination.
    grant_endpoints(tid);

    quark_rt::manifest::grant_image(tid, image, MANIFEST_SCRATCH_SLOT);
}

/// Slot in init's own CSpace used to hold a capability while handing it over.
/// Below the slots init keeps its endpoints in, so it cannot tread on them.
const MANIFEST_SCRATCH_SLOT: usize = 12;

// (Disk-based FAT32 reader removed — init now uses VFS for disk files)

// ---------------------------------------------------------------------------
// Framebuffer info handoff to console server
// ---------------------------------------------------------------------------

/// Physical extent of the framebuffer, page aligned.
///
/// Console maps exactly this and nothing else, so this is exactly what it is
/// granted. Deriving it here rather than hardcoding a range keeps the grant
/// correct across whatever mode the bootloader actually set.
fn framebuffer_range() -> (u64, u64) {
    let info = unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) };
    let size = (info.fb_pitch as u64) * (info.fb_height as u64);
    let base = info.fb_addr & !0xFFF;
    let end = (info.fb_addr + size + 0xFFF) & !0xFFF;
    (base, end)
}

fn send_fb_info(console_tid: usize) {
    let info = unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) };

    // Query the kernel console's current cursor position so the
    // user-space console server can continue where the kernel left off.
    let (row, col) = syscall::sys_console_pos();

    // Pack framebuffer info into one IPC message
    // data[0] = physical address
    // data[1] = (width << 32) | height
    // data[2] = (pitch << 32) | bpp
    // data[3] = (red_pos << 16) | (green_pos << 8) | blue_pos
    // data[4] = (cursor_row << 32) | cursor_col
    let msg = Message {
        sender: 0,
        tag: 100, // TAG_FB_INIT
        data: [
            info.fb_addr,
            ((info.fb_width as u64) << 32) | (info.fb_height as u64),
            ((info.fb_pitch as u64) << 32) | (info.fb_bpp as u64),
            ((info.fb_red_pos as u64) << 16) | ((info.fb_green_pos as u64) << 8) | (info.fb_blue_pos as u64),
            ((row as u64) << 32) | (col as u64),
            0,
        ],
    };

    let mut reply = Message::empty();
    if syscall::sys_call(console_tid, &msg, &mut reply).is_err() {
        println!("[init] Failed to send FB info to console");
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Load essential services from boot image
// ---------------------------------------------------------------------------

struct BootContext {
    console_pipe: usize, // pipe handle for stdout/stderr
    input_tid: usize,
    vfs_spawn: Option<Spawned>,
    /// The device manager, if the boot image had one.
    devmgr_tid: usize,
}

/// Offer the driver in `image`, called `name`, to the device manager, which
/// starts it for each device it drives: how many.
fn offer_driver(devmgr_tid: usize, image: &[u8], name: &[u8]) -> Option<u64> {
    let mut words = [0u8; 16];
    let n = name.len().min(16);
    words[..n].copy_from_slice(&name[..n]);
    let msg = Message {
        sender: 0,
        tag: devices::TAG_OFFER,
        data: [
            image.len() as u64,
            u64::from_le_bytes(words[..8].try_into().unwrap_or([0; 8])),
            u64::from_le_bytes(words[8..].try_into().unwrap_or([0; 8])),
            0,
            0,
            0,
        ],
    };
    let mut reply = Message::empty();
    syscall::sys_call_lend(devmgr_tid, &msg, &mut reply, image).ok()?;
    (reply.tag == 0).then_some(reply.data[0])
}

fn load_essentials_from_boot_image(rootfs_phys: usize, rootfs_size: usize, mgr: &mut Manager) -> BootContext {
    println!("[init] Mounting boot image");

    // Map the entire rootfs image
    let rootfs_pages = (rootfs_size + PAGE_SIZE - 1) / PAGE_SIZE;
    if syscall::sys_map_phys(rootfs_phys, BOOT_IMG_BASE, rootfs_pages).is_err() {
        println!("[init] Failed to map boot image");
        return BootContext { console_pipe: 0, input_tid: 0, vfs_spawn: None, devmgr_tid: 0 };
    }

    let rootfs = unsafe { core::slice::from_raw_parts(BOOT_IMG_BASE as *const u8, rootfs_size) };
    let bpb = parse_bpb(rootfs);
    let (entries, count) = scan_root_dir(rootfs, &bpb);

    println!("[init] Files on boot image: {}", count);

    // Pass 1: find and spawn NAMESRVR.ELF first (guarantees TID 2)
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"NAMESRVR" && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        // init made it, so init may mint the right to call it,
                        // and everything below is handed a copy.
                        if syscall::sys_cap_mint(NS_SLOT, syscall::CAP_TYPE_ENDPOINT, info.tid as u64, 0)
                            .is_err()
                        {
                            println!("[init] no capability to the nameserver");
                        }
                        // Every pass has to do this for itself; there is no
                        // shared path that does it for them. The nameserver
                        // asks for no capabilities, but it does ask to be
                        // scheduled as a server, and that arrives the same way.
                        grant_caps_from_manifest(data, info.tid);
                        let _ = spawn::set_args(&info, &[b"nameserver"], &SPAWN_SCRATCH);
                        mgr.boot(b"nameserver", info.tid, Program::Boot, Policy::Never, None);
                        let _ = info.start();
                        println!("[init] Spawned nameserver (TID {})", info.tid);
                    }
                    Err(()) => println!("[init] FAILED to spawn nameserver"),
                }
            }
            break;
        }
    }

    // Pass 1b: spawn FB.ELF, the framebuffer device.
    //
    // Before the console, because the console asks it for the display. The
    // framebuffer is a device with one owner at a time, and this is what
    // decides who: the text console at boot, a compositor when one is run.
    //
    // Being first means there is nowhere for it to write yet — the pipe every
    // other service prints to is the console's, and the console does not
    // exist. Its descriptors are wired below, once there is one.
    let mut fb_tid: usize = 0;
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"FB      " && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        // The framebuffer, and nothing else. Its address comes
                        // from whatever mode the bootloader set, so this is the
                        // one grant a manifest cannot express. It goes into
                        // slot 0 and stays there: everything the device lends
                        // out is derived from it.
                        // A machine whose display draws from memory gave the
                        // bootloader none, and has nothing here: the
                        // display's driver gives the device one later.
                        let (fb_base, fb_end) = framebuffer_range();
                        if fb_end > fb_base && fb_base != 0 {
                            mint_and_grant(info.tid, 0, syscall::CAP_TYPE_PHYS_RANGE, fb_base, fb_end);
                        }
                        grant_caps_from_manifest(data, info.tid);
                        let _ = spawn::set_args(&info, &[b"fb"], &SPAWN_SCRATCH);
                        fb_tid = info.tid;
                        mgr.boot(b"fb", info.tid, Program::Boot, Policy::Never, Some(b"fb"));
                        let _ = info.start();
                        // It learns the mode from init before it registers, so
                        // this is the one call init cannot make with a
                        // capability from a lookup.
                        let _ = syscall::sys_cap_mint(FB_SLOT, syscall::CAP_TYPE_ENDPOINT, info.tid as u64, 0);
                        send_fb_info(info.tid);
                        println!("[init] Spawned fb (TID {})", info.tid);
                    }
                    Err(()) => println!("[init] FAILED to spawn fb"),
                }
            }
            break;
        }
    }

    // Pass 2: spawn QTTY.ELF, the text console
    let mut console_pipe: usize = 0;
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"QTTY    " && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        // No framebuffer grant: the console asks the
                        // framebuffer device for the display, and is lent the
                        // right to map it for as long as it holds it.
                        grant_caps_from_manifest(data, info.tid);
                        let _ = spawn::set_args(&info, &[b"qtty"], &SPAWN_SCRATCH);
                        // Create console pipe and set fds BEFORE starting console
                        // to avoid race where console reaches main loop before fd 0 is set
                        if let Ok(pipe) = syscall::sys_pipe_create() {
                            console_pipe = pipe;
                            let _ = syscall::sys_pipe_fd_set(info.tid, 0, pipe, false);
                            let my_tid = syscall::sys_getpid() as usize;
                            let _ = syscall::sys_pipe_fd_set(my_tid, 1, pipe, true);
                            let _ = syscall::sys_pipe_fd_set(my_tid, 2, pipe, true);
                        }
                        mgr.boot(b"console", info.tid, Program::Boot, Policy::Never, Some(b"console"));
                        let _ = info.start();
                        println!("[init] Spawned console (TID {})", info.tid);
                        // The framebuffer device could not be given a stdout
                        // when it started, because this pipe is what a stdout
                        // is and the console owns the other end of it. It is
                        // the one service whose diagnostics you want when the
                        // display misbehaves, so it should not be the one
                        // service that cannot speak.
                        if console_pipe != 0 && fb_tid != 0 {
                            let _ = syscall::sys_pipe_fd_set(fb_tid, 1, console_pipe, true);
                            let _ = syscall::sys_pipe_fd_set(fb_tid, 2, console_pipe, true);
                        }
                    }
                    Err(()) => println!("[init] FAILED to spawn console"),
                }
            }
            break;
        }
    }

    // Pass 2b: spawn DEVMGR.ELF, the device manager, which holds every
    // device and starts their drivers: the ones below that say which
    // devices they drive are offered to it rather than started here.
    let mut devmgr_tid: usize = 0;
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"DEVMGR  " && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        grant_caps_from_manifest(data, info.tid);
                        if console_pipe != 0 {
                            let _ = syscall::sys_pipe_fd_set(info.tid, 1, console_pipe, true);
                            let _ = syscall::sys_pipe_fd_set(info.tid, 2, console_pipe, true);
                        }
                        let _ = spawn::set_args(&info, &[b"devmgr"], &SPAWN_SCRATCH);
                        // init made it, so init may call it before it has
                        // a name — and nobody else can.
                        mgr.boot(b"devmgr", info.tid, Program::Boot, Policy::Never, Some(devices::NAME));
                        if syscall::sys_cap_mint(DEVMGR_SLOT, syscall::CAP_TYPE_ENDPOINT, info.tid as u64, 0).is_ok()
                            && info.start().is_ok()
                        {
                            devmgr_tid = info.tid;
                            println!("[init] Spawned devmgr (TID {})", info.tid);
                        }
                    }
                    Err(()) => println!("[init] FAILED to spawn devmgr"),
                }
            }
            break;
        }
    }

    // Pass 3: spawn the essential ELFs (KEYBOARD, AUTH) — not INPUT or VFS,
    // which come later — and offer every driver to the device manager.
    let mut spawned_tids = [0usize; 32];
    let mut spawned_count = 0usize;
    for i in 0..count {
        let e = &entries[i];

        // Skip already-spawned
        if &e.name[0..8] == b"NAMESRVR" || (&e.name[0..8] == b"QTTY    " && &e.name[8..11] == b"ELF") {
            continue;
        }
        // Skip INPUT (deferred to after keyboard) and VFS (deferred to after phase 2)
        if (&e.name[0..8] == b"INPUT   " || &e.name[0..8] == b"VFS     ") && &e.name[8..11] == b"ELF" {
            continue;
        }
        // Only spawn .ELF files
        if &e.name[8..11] != b"ELF" {
            continue;
        }
        let mut namebuf = [0u8; 16];
        let namelen = fat_name_to_buf(&e.name, &mut namebuf);
        if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
            // A driver for a device is the device manager's to start, for
            // each device it drives, holding that one.
            if quark_rt::manifest::drives_any(data) {
                let base_len = e.name[0..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
                let mut lbuf = [0u8; 8];
                for j in 0..base_len {
                    lbuf[j] = e.name[j].to_ascii_lowercase();
                }
                let name = core::str::from_utf8(&lbuf[..base_len]).unwrap_or("a driver");
                match (devmgr_tid != 0).then(|| offer_driver(devmgr_tid, data, &lbuf[..base_len])).flatten() {
                    Some(0) => println!("[init] Nothing here for {} to drive", name),
                    Some(n) => println!("[init] The device manager started {} for {} device(s)", name, n),
                    None => println!("[init] No device manager to start {}", name),
                }
                continue;
            }
            // Of the rest, only what is needed before there are files: the
            // others are read from the root.
            if !is_essential_elf(&e.name) {
                continue;
            }
            match spawn::load(data, &SPAWN_SCRATCH) {
                Ok(info) => {
                    let tid = info.tid;
                    grant_caps_from_manifest(data, tid);
                    if console_pipe != 0 {
                        let _ = syscall::sys_pipe_fd_set(tid, 1, console_pipe, true);
                        let _ = syscall::sys_pipe_fd_set(tid, 2, console_pipe, true);
                    }
                    let _ = spawn::set_args(&info, &[&namebuf[..namelen]], &SPAWN_SCRATCH);
                    // Which of them are started again, and from a copy kept
                    // here: what nobody else holds a part of.
                    let base_len = e.name[0..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
                    let mut lbuf = [0u8; 8];
                    for j in 0..base_len {
                        lbuf[j] = e.name[j].to_ascii_lowercase();
                    }
                    let lname = &lbuf[..base_len];
                    let (program, policy, register): (Program, Policy, Option<&[u8]>) = match lname {
                        b"auth" => (Program::Image(data.to_vec()), Policy::OnFailure, Some(quark_rt::auth::NAME)),
                        b"net" => (Program::Image(data.to_vec()), Policy::OnFailure, Some(b"net")),
                        b"sound" => (Program::Image(data.to_vec()), Policy::OnFailure, Some(b"sound")),
                        b"keyboard" => (Program::Boot, Policy::Never, Some(b"keyboard")),
                        _ => (Program::Boot, Policy::Never, None),
                    };
                    mgr.boot(lname, tid, program, policy, register);
                    mgr.boot_argv(lname, &[&namebuf[..namelen]], true);
                    let _ = info.start();
                    if spawned_count < 32 {
                        spawned_tids[spawned_count] = tid;
                        spawned_count += 1;
                    }
                    let base_len = e.name[0..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
                    let mut lbuf = [0u8; 8];
                    for j in 0..base_len { lbuf[j] = e.name[j].to_ascii_lowercase(); }
                    if let Ok(name) = core::str::from_utf8(&lbuf[..base_len]) {
                        println!("[init] Spawned {} (TID {})", name, tid);
                    }
                }
                Err(()) => println!("[init] FAILED to spawn"),
            }
        }
    }

    // Pass 3b: a disk made of memory, if the bootloader brought a root
    // filesystem with it. That is a system running from whatever it was
    // booted off — a CD, a USB stick — without a driver for it: the whole
    // root came as one file, and this serves it as a disk.
    //
    // It is granted the memory the file is in and nothing else, which is
    // the one thing its manifest cannot ask for: only init knows where the
    // file was put.
    if let Some((phys, size)) = find_module(LIVE_MODULE) {
        for i in 0..count {
            let e = &entries[i];
            if &e.name[0..8] != b"RAMDISK " || &e.name[8..11] != b"ELF" {
                continue;
            }
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        let end = (phys + size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
                        mint_and_grant(info.tid, 0, syscall::CAP_TYPE_PHYS_RANGE, phys as u64, end as u64);
                        grant_caps_from_manifest(data, info.tid);
                        if console_pipe != 0 {
                            let _ = syscall::sys_pipe_fd_set(info.tid, 1, console_pipe, true);
                            let _ = syscall::sys_pipe_fd_set(info.tid, 2, console_pipe, true);
                        }
                        let (mut at, mut len) = ([0u8; 18], [0u8; 18]);
                        let (at_len, len_len) = (hex(phys, &mut at), hex(size, &mut len));
                        let _ = spawn::set_args(
                            &info,
                            &[b"ramdisk", b"module", &at[..at_len], &len[..len_len]],
                            &SPAWN_SCRATCH,
                        );
                        mgr.boot(b"ramdisk", info.tid, Program::Boot, Policy::Never, None);
                        let _ = info.start();
                        // And init's own right to that memory goes. It was
                        // given it to read the module, as it is every
                        // module, and it has handed it on; kept, it would be
                        // a task that can map a hundred megabytes it has no
                        // use for, and that is a filesystem.
                        let me = syscall::sys_getpid() as usize;
                        for slot in 0.. {
                            let Ok(cap) = syscall::sys_cap_read(me, slot) else { break };
                            if cap.cap_type == syscall::CAP_TYPE_PHYS_RANGE
                                && cap.param0 == phys as u64
                                && cap.param1 == end as u64
                            {
                                let _ = syscall::sys_cap_delete(slot);
                            }
                        }
                        println!("[init] Spawned ramdisk (TID {}) on {} MiB of live image", info.tid, size >> 20);
                    }
                    Err(()) => println!("[init] FAILED to spawn ramdisk"),
                }
            }
            break;
        }
    }

    // Pass 4: spawn INPUT.ELF (needs keyboard to be running)
    let mut input_tid: usize = 0;
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"INPUT   " && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        input_tid = info.tid;
                        // This pass once granted nothing at all, so input ran
                        // with an empty CSpace — invisible while UID 0 bypassed
                        // every check. Each pass must grant; there is no shared
                        // path that does it for them.
                        grant_caps_from_manifest(data, info.tid);
                        if console_pipe != 0 {
                            let _ = syscall::sys_pipe_fd_set(info.tid, 1, console_pipe, true);
                            let _ = syscall::sys_pipe_fd_set(info.tid, 2, console_pipe, true);
                        }
                        let _ = spawn::set_args(&info, &[b"input"], &SPAWN_SCRATCH);
                        mgr.boot(b"input", info.tid, Program::Boot, Policy::Never, Some(b"input"));
                        let _ = info.start();
                        println!("[init] Spawned input (TID {})", info.tid);
                    }
                    Err(()) => println!("[init] FAILED to spawn input"),
                }
            }
            break;
        }
    }

    // Wire fd 0 (stdin) to input server for all previously spawned tasks
    if input_tid != 0 {
        for i in 0..spawned_count {
            let _ = syscall::sys_fd_set(spawned_tids[i], 0, input_tid, 1); // TAG_READ=1
        }
    }

    // Pass 5: load VFS.ELF but do NOT start it yet (deferred to after phase 2)
    let mut vfs_spawn: Option<Spawned> = None;
    for i in 0..count {
        let e = &entries[i];
        if &e.name[0..8] == b"VFS     " && &e.name[8..11] == b"ELF" {
            if let Ok(data) = read_file_to_buffer(rootfs, &bpb, e.first_cluster, e.file_size) {
                match spawn::load(data, &SPAWN_SCRATCH) {
                    Ok(info) => {
                        grant_caps_from_manifest(data, info.tid);
                        if console_pipe != 0 {
                            let _ = syscall::sys_pipe_fd_set(info.tid, 1, console_pipe, true);
                            let _ = syscall::sys_pipe_fd_set(info.tid, 2, console_pipe, true);
                        }
                        if input_tid != 0 {
                            let _ = syscall::sys_fd_set(info.tid, 0, input_tid, 1);
                        }
                        // Told where the root is, if anything says; left to
                        // find it otherwise. A root that came with the
                        // bootloader is the root, whatever a disk says.
                        let live = find_module(LIVE_MODULE).is_some();
                        let _ = match root_from_config() {
                            _ if live => {
                                println!("[init] The root is the live image, in memory");
                                spawn::set_args(&info, &[b"vfs", b"ram0", b"0"], &SPAWN_SCRATCH)
                            }
                            Some(root) => {
                                println!(
                                    "[init] The root is volume {} of {}",
                                    core::str::from_utf8(root.volume()).unwrap_or("?"),
                                    core::str::from_utf8(root.driver()).unwrap_or("?")
                                );
                                spawn::set_args(&info, &[b"vfs", root.driver(), root.volume()], &SPAWN_SCRATCH)
                            }
                            None => spawn::set_args(&info, &[b"vfs"], &SPAWN_SCRATCH),
                        };
                        println!("[init] Spawned vfs (TID {}, deferred start)", info.tid);
                        vfs_spawn = Some(info);
                    }
                    Err(()) => println!("[init] FAILED to spawn vfs"),
                }
            }
            break;
        }
    }

    BootContext { console_pipe, input_tid, vfs_spawn, devmgr_tid }
}

// ---------------------------------------------------------------------------
// Phase 2: Load remaining programs from disk
// ---------------------------------------------------------------------------

/// `/etc/init.conf`, as text: what the distribution wants done once there
/// are files (`services::Manager::configure` says what the lines are).
fn read_config(vfs_tid: usize) -> Option<alloc::vec::Vec<u8>> {
    let (handle, size, is_dir) = vfs::open(vfs_tid, b"/etc/init.conf").ok()?;
    let mut text = alloc::vec![0u8; (size as usize).min(16384)];
    let got = if is_dir { 0 } else { vfs::read(vfs_tid, handle, &mut text, 0).unwrap_or(0) };
    let _ = vfs::close(vfs_tid, handle);
    text.truncate(got as usize);
    Some(text)
}

/// What the session is when `/etc/init.conf` names none: `login` from
/// `/usr/bin`, or the shell, by whatever name the file has there.
fn default_session(vfs_tid: usize) -> Option<([u8; 48], usize)> {
    let (dir_handle, _, _) = match vfs::open(vfs_tid, b"/usr/bin") {
        Ok(h) => h,
        Err(_) => {
            println!("[init] /usr/bin not found on VFS.");
            return None;
        }
    };
    let mut login_entry: Option<vfs::DirEntry> = None;
    let mut shell_entry: Option<vfs::DirEntry> = None;
    let mut index = 0u32;
    loop {
        match vfs::readdir(vfs_tid, dir_handle, index) {
            Ok(Some(entry)) => {
                if !entry.is_dir && name_matches_entry(&entry, b"LOGIN", b"ELF") {
                    login_entry = Some(entry);
                    break;
                }
                if !entry.is_dir && name_matches_entry(&entry, b"QSH", b"ELF") {
                    shell_entry = Some(entry);
                }
                index += 1;
            }
            Ok(None) | Err(_) => break,
        }
    }
    let _ = vfs::close(vfs_tid, dir_handle);
    let Some(entry) = login_entry.or(shell_entry) else {
        println!("[init] login/shell not found in /usr/bin");
        return None;
    };
    let name = entry.name_bytes();
    let mut path = [0u8; 48];
    let prefix = b"/usr/bin/";
    let len = (prefix.len() + name.len()).min(path.len());
    path[..prefix.len()].copy_from_slice(prefix);
    path[prefix.len()..len].copy_from_slice(&name[..len - prefix.len()]);
    Some((path, len))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("[init] Starting init process.");

    // The kernel is another repository, built at another time. Ask it which
    // ABI it speaks before making any call whose number could have moved:
    // with a different major every number after this one is a guess, and a
    // guess here is a task created where a page was meant to be mapped.
    match syscall::abi_check() {
        Ok((major, minor)) => println!(
            "[init] Kernel ABI {}.{}; this userland was built for {}.{}.",
            major,
            minor,
            syscall::ABI_VERSION_MAJOR,
            syscall::ABI_VERSION_MINOR
        ),
        Err((major, minor)) => {
            println!(
                "[init] Kernel ABI {}.{}, and this userland was built for {}.{}. Stopping:",
                major,
                minor,
                syscall::ABI_VERSION_MAJOR,
                syscall::ABI_VERSION_MINOR
            );
            println!("[init] the kernel and quarkutils have to be built for the same ABI.");
            syscall::sys_exit();
        }
    }
    // Everything else is named by whatever started it. This was started by
    // the kernel, which says nothing of what it started.
    let _ = syscall::sys_program_name_set(syscall::sys_getpid() as usize, &[b"init"]);

    let info = unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) };
    let mod_count = info.module_count as usize;
    let mut mgr = Manager::new();

    // Find boot image module
    let mut found = false;
    for i in 0..mod_count {
        let m = &info.modules[i];
        let name = module_name(&m.name);
        if starts_with(name, b"boot") {
            let phys = m.phys_start as usize;
            let size = (m.phys_end - m.phys_start) as usize;

            // Phase 1: Load essential services from boot image
            let ctx = load_essentials_from_boot_image(phys, size, &mut mgr);

            // Unload boot image — return pages to the physical memory allocator
            let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
            let _ = syscall::sys_phys_free(phys, pages);
            println!("[init] Boot image freed ({} pages).", pages);

            // Phase 2: Start VFS, wait for it to register
            let vfs_tid = if let Some(vfs) = ctx.vfs_spawn {
                println!("[init] Starting VFS (TID {})", vfs.tid);
                mgr.boot(b"vfs", vfs.tid, Program::Boot, Policy::Never, Some(b"vfs"));
                let _ = vfs.start();
                match nameserver::lookup_retry(b"vfs", 50) {
                    Some(tid) => {
                        println!("[init] VFS ready.");
                        Some(tid)
                    }
                    None => {
                        println!("[init] VFS failed to register.");
                        None
                    }
                }
            } else {
                None
            };

            // The device manager's drivers that are not in the boot image,
            // now there are files to read them from: before anything
            // `/etc/init.conf` asks for, which may need them.
            if vfs_tid.is_some() && ctx.devmgr_tid != 0 {
                let msg = Message { sender: 0, tag: devices::TAG_FILES, data: [0; 6] };
                let mut reply = Message::empty();
                if syscall::sys_call(ctx.devmgr_tid, &msg, &mut reply).is_ok() && reply.tag == 0 && reply.data[0] > 0 {
                    println!("[init] The device manager started {} more driver(s)", reply.data[0]);
                }
            }

            // Phase 3: what the root says to start, to run and to log in
            // through, which the service manager does from here on.
            mgr.wire(ctx.console_pipe, ctx.input_tid);
            if let Some(vfs) = vfs_tid {
                // What is mounted, written down where programs that were
                // written for Unix look for it: the root, and nothing else
                // yet. A root that cannot be written goes without.
                let _ = vfs::write_mtab(vfs);
                let config = read_config(vfs).unwrap_or_default();
                if !mgr.configure(vfs, &config) {
                    if let Some((path, len)) = default_session(vfs) {
                        mgr.default_session(&path[..len]);
                    }
                }
            } else {
                println!("[init] No VFS, skipping disk program loading.");
            }

            found = true;
            break;
        }
    }

    if !found {
        println!("[init] ERROR: boot image module not found!");
    }

    // init stays in the drivers' band: it starts services again, and a
    // spawner can give no better band than it is in. It does very little
    // there — it waits for things to end, and for `svc`.
    mgr.serve()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[init] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
