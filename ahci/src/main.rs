#![no_std]
#![no_main]

//! A SATA disk behind an AHCI controller: what QEMU's q35 machine has, and
//! nearly every PC made since there was SATA.
//!
//! Started by the device manager for a controller, holding it. A controller
//! has up to thirty-two ports, and this drives the first that has a disk on
//! it (not a CD, whose commands are another language) — which a port says
//! only once it has somewhere to put what the disk sends it, so each port
//! with something on it is given this program's page in turn and asked. A
//! command is a
//! header in a list, a table with the command and where its data is, and a
//! bit set to say go: the controller copies the sectors itself, straight
//! between the disk and the page `block::serve` keeps them in, which is this
//! program's own memory — so on a machine with an IOMMU it reaches that and
//! the port's own page and nothing else. Everything above the sectors is
//! `quark_rt::block`, as it is for every disk.

use quark_rt::block::{self, Device as Disk, MAX_SECTORS, SECTOR};
use quark_rt::ipc::Message;
use quark_rt::manifest::CapReq;
use quark_rt::{pci, println, syscall};

// A driver's band; an AHCI controller; and frames for the port's lists and
// the page sectors go through.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives_interface(0x01, 0x06, 0x01),
    CapReq::phys_alloc(4),
]);

/// Where the controller's registers (BAR 5) are mapped, and the slot the
/// range is minted in.
const ABAR_AT: usize = 0xA0_0000_0000;
const ABAR_SLOT: usize = 10;
/// The page `block::serve` keeps sectors in, and the port's page: its
/// command list, the FISes it receives, a command table and room to
/// identify the disk into.
const DATA_AT: usize = 0x86_0000_0000;
const PORT_AT: usize = 0x86_0000_1000;
const LIST: usize = 0x000;
const RECEIVED: usize = 0x400;
const TABLE: usize = 0x500;
const IDENTIFY_AT: usize = 0x800;

// The controller's registers.
const CAP: usize = 0x00;
const GHC: usize = 0x04;
const IS: usize = 0x08;
const PI: usize = 0x0C;
const CAP2: usize = 0x24;
const BOHC: usize = 0x28;
const CAP_64BIT: u32 = 1 << 31;
/// The firmware may still be using the controller, and says when it has
/// stopped (`BOHC`).
const CAP2_HANDOFF: u32 = 1 << 0;
const BOHC_FIRMWARE_OWNS: u32 = 1 << 0;
const BOHC_OS_OWNS: u32 = 1 << 1;
const BOHC_FIRMWARE_BUSY: u32 = 1 << 4;
const GHC_RESET: u32 = 1 << 0;
const GHC_INTERRUPTS: u32 = 1 << 1;
const GHC_AHCI: u32 = 1 << 31;

// A port's, from 0x100 + 0x80 each.
const P_CLB: usize = 0x00;
const P_CLBU: usize = 0x04;
const P_FB: usize = 0x08;
const P_FBU: usize = 0x0C;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SCTL: usize = 0x2C;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;
const CMD_START: u32 = 1 << 0;
const CMD_SPIN_UP: u32 = 1 << 1;
const CMD_FIS_RECEIVE: u32 = 1 << 4;
const CMD_FIS_RUNNING: u32 = 1 << 14;
const CMD_LIST_RUNNING: u32 = 1 << 15;
const TFD_BUSY: u32 = 0x80;
const TFD_DRQ: u32 = 0x08;
const TFD_ERROR: u32 = 0x01;
const IS_TASK_FILE_ERROR: u32 = 1 << 30;
/// A disk's signature; a CD's is another, and a port that has heard nothing
/// yet says all ones.
const SIGNATURE_ATA: u32 = 0x0000_0101;
const SIGNATURE_NONE: u32 = u32::MAX;
/// A link with a device on it, talking.
const DET_PRESENT: u32 = 3;
/// The interrupts a port is asked for: a command done, and an error.
const IE_WANTED: u32 = (1 << 0) | IS_TASK_FILE_ERROR;

const IDENTIFY: u8 = 0xEC;
const READ_DMA_EXT: u8 = 0x25;
const WRITE_DMA_EXT: u8 = 0x35;

/// How long a command may take before the disk is given up on: tenths of
/// a second.
const PATIENCE: u64 = 300;

fn read32(at: usize) -> u32 {
    unsafe { core::ptr::read_volatile(at as *const u32) }
}
fn write32(at: usize, v: u32) {
    unsafe { core::ptr::write_volatile(at as *mut u32, v) }
}
fn write8(at: usize, v: u8) {
    unsafe { core::ptr::write_volatile(at as *mut u8, v) }
}

struct Ahci {
    hba: usize,
    port: usize,
    page: u64,
    data: u64,
    sectors: u64,
    irq: u8,
    by_message: bool,
    /// What the port has said since the command was issued, cleared or
    /// not: an interrupt is answered by clearing what the port says, and
    /// an error must not be answered away before the command looks.
    seen: core::cell::Cell<u32>,
}

impl Ahci {
    /// Wait up to about `tenths` tenths of a second for `done`, sleeping on
    /// the controller's interrupt.
    fn wait(&self, tenths: u64, done: impl Fn() -> bool) -> bool {
        for _ in 0..tenths * 10 {
            if done() {
                return true;
            }
            let mut msg = Message::empty();
            if syscall::sys_recv_timeout(0, &mut msg, syscall::ns(10_000_000)).is_ok()
                && msg.sender == 0
                && msg.tag == self.irq as u64
            {
                self.settle();
            }
        }
        done()
    }

    /// Say the port's interrupt has been dealt with: the port's, the
    /// controller's, and the line's where it has one.
    fn settle(&self) {
        let port_is = read32(self.port + P_IS);
        write32(self.port + P_IS, port_is);
        self.seen.set(self.seen.get() | port_is);
        let n = (self.port - self.hba - 0x100) / 0x80;
        write32(self.hba + IS, 1 << n);
        if !self.by_message {
            syscall::sys_irq_ack(self.irq);
        }
    }

    /// Run one command in slot 0: `command`, `count` sectors at `lba`, its
    /// data `bytes` long at physical `buf`, written to the disk if `write`.
    fn run(&self, command: u8, lba: u64, count: u32, buf: u64, bytes: u32, write: bool) -> bool {
        let page = PORT_AT;
        let table = self.page + TABLE as u64;
        // The header: a command FIS five words long, written or read, one
        // entry of where its data is.
        write32(page + LIST, 5 | if write { 1 << 6 } else { 0 } | 1 << 16);
        write32(page + LIST + 4, 0);
        write32(page + LIST + 8, table as u32);
        write32(page + LIST + 12, (table >> 32) as u32);
        // The command, as a register FIS from the host.
        let t = page + TABLE;
        unsafe { core::ptr::write_bytes(t as *mut u8, 0, 0x90) };
        let fis = [
            0x27, 0x80, command, 0,
            lba as u8, (lba >> 8) as u8, (lba >> 16) as u8, 0x40,
            (lba >> 24) as u8, (lba >> 32) as u8, (lba >> 40) as u8, 0,
            count as u8, (count >> 8) as u8, 0, 0,
        ];
        for (i, &b) in fis.iter().enumerate() {
            write8(t + i, b);
        }
        // Where its data is: one stretch, and an interrupt when it is done.
        write32(t + 0x80, buf as u32);
        write32(t + 0x84, (buf >> 32) as u32);
        write32(t + 0x88, 0);
        write32(t + 0x8C, (bytes - 1) | 1 << 31);

        let port = self.port;
        if !self.wait(10, || read32(port + P_TFD) & (TFD_BUSY | TFD_DRQ) == 0) {
            println!("[ahci] the disk is busy and does not stop being");
            return false;
        }
        write32(port + P_IS, u32::MAX);
        self.seen.set(0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write32(port + P_CI, 1);
        let failed = || (self.seen.get() | read32(port + P_IS)) & IS_TASK_FILE_ERROR != 0;
        let finished = self.wait(PATIENCE, || read32(port + P_CI) & 1 == 0 || failed());
        let error = failed() || read32(port + P_TFD) & TFD_ERROR != 0;
        self.settle();
        if !finished {
            println!("[ahci] the disk has not answered a command in {} seconds", PATIENCE / 10);
        }
        if !finished || error {
            self.recover();
        }
        finished && !error
    }

    /// After an error, or a command that never finished, the port takes no
    /// more until it has been stopped and started again — and the disk's
    /// link reset, if the disk is still busy with what went wrong.
    fn recover(&self) {
        let port = self.port;
        write32(port + P_CMD, read32(port + P_CMD) & !CMD_START);
        until(500, || read32(port + P_CMD) & CMD_LIST_RUNNING == 0);
        write32(port + P_SERR, u32::MAX);
        write32(port + P_IS, u32::MAX);
        if read32(port + P_TFD) & (TFD_BUSY | TFD_DRQ) != 0 {
            write32(port + P_SCTL, (read32(port + P_SCTL) & !0xF) | 1);
            syscall::sleep_ns(2_000_000);
            write32(port + P_SCTL, read32(port + P_SCTL) & !0xF);
            until(1000, || read32(port + P_SSTS) & 0xF == DET_PRESENT);
            write32(port + P_SERR, u32::MAX);
            until(5000, || read32(port + P_TFD) & (TFD_BUSY | TFD_DRQ) == 0);
            write32(port + P_IS, u32::MAX);
        }
        write32(port + P_CMD, read32(port + P_CMD) | CMD_START);
    }

    fn transfer(&mut self, write: bool, lba: u64, count: u32, buf: usize) -> bool {
        let bytes = count as usize * SECTOR;
        if count == 0 || count > MAX_SECTORS || lba.checked_add(count as u64).is_none_or(|end| end > self.sectors) {
            return false;
        }
        let offset = buf.wrapping_sub(DATA_AT);
        if offset + bytes > 4096 {
            return false;
        }
        let command = if write { WRITE_DMA_EXT } else { READ_DMA_EXT };
        self.run(command, lba, count, self.data + offset as u64, bytes as u32, write)
    }
}

impl Disk for Ahci {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool {
        into.len() >= count as usize * SECTOR && self.transfer(false, lba, count, into.as_ptr() as usize)
    }

    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool {
        from.len() >= count as usize * SECTOR && self.transfer(true, lba, count, from.as_ptr() as usize)
    }
}

fn stop(why: &str) -> ! {
    println!("[ahci] {}", why);
    syscall::sys_exit_code(1);
}

/// A page of this program's own memory at `at`, below four gigabytes if the
/// controller cannot address more: where it is.
fn page(at: usize, low: bool) -> Option<u64> {
    let frame = if low { syscall::sys_phys_alloc_low(1) } else { syscall::sys_phys_alloc(1) }.ok()?;
    syscall::sys_map_phys(frame, at, 1).ok()?;
    unsafe { core::ptr::write_bytes(at as *mut u8, 0, 4096) };
    Some(frame as u64)
}

/// Wait up to `ms` milliseconds for `done`, one at a time.
fn until(ms: u64, done: impl Fn() -> bool) -> bool {
    for _ in 0..ms {
        if done() {
            return true;
        }
        syscall::sleep_ns(1_000_000);
    }
    done()
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some((device, info)) = pci::this_device().and_then(|d| Some((d, pci::info(d)?))) else {
        stop("started without a controller: the device manager starts this, for an AHCI controller.");
    };
    let Some(hba) = pci::map_bar(device, 5, ABAR_AT, ABAR_SLOT) else {
        stop("may not map the controller's registers");
    };
    if pci::claim(device).is_err() {
        stop("the controller is another program's");
    }
    if pci::enable(device, pci::COMMAND_MEMORY | pci::COMMAND_MASTER).is_err() {
        stop("the controller may not be turned on");
    }
    // The firmware's, until it says it has finished with it.
    if read32(hba + CAP2) & CAP2_HANDOFF != 0 {
        write32(hba + BOHC, read32(hba + BOHC) | BOHC_OS_OWNS);
        if !until(25, || read32(hba + BOHC) & BOHC_FIRMWARE_OWNS == 0) && read32(hba + BOHC) & BOHC_FIRMWARE_BUSY != 0 {
            until(2000, || read32(hba + BOHC) & BOHC_FIRMWARE_OWNS == 0);
        }
    }
    // From nothing: reset, and AHCI's own registers rather than IDE's.
    write32(hba + GHC, read32(hba + GHC) | GHC_RESET);
    if !until(1000, || read32(hba + GHC) & GHC_RESET == 0) {
        stop("the controller does not come out of its reset");
    }
    write32(hba + GHC, GHC_AHCI);
    let low = read32(hba + CAP) & CAP_64BIT == 0;
    let (Some(port_page), Some(data)) = (page(PORT_AT, low), page(DATA_AT, low)) else {
        stop("no memory for the port");
    };

    // Every port spun up, and a second for links to come up after the
    // reset; then each with something on it asked what that is, until one
    // is a disk.
    let implemented = read32(hba + PI);
    let ports = || (0..32usize).filter(move |n| implemented & (1 << n) != 0).map(|n| (n, hba + 0x100 + n * 0x80));
    for (_, port) in ports() {
        write32(port + P_CMD, read32(port + P_CMD) | CMD_SPIN_UP);
    }
    let linked = |port: usize| read32(port + P_SSTS) & 0xF == DET_PRESENT;
    until(1000, || ports().any(|(_, port)| linked(port)));
    let found = ports().filter(|&(_, port)| linked(port)).find(|&(_, port)| {
        // Stopped, given the page, and listening: what the disk says first
        // is its signature.
        write32(port + P_CMD, read32(port + P_CMD) & !CMD_START);
        if !until(500, || read32(port + P_CMD) & CMD_LIST_RUNNING == 0) {
            return false;
        }
        write32(port + P_CMD, read32(port + P_CMD) & !CMD_FIS_RECEIVE);
        if !until(500, || read32(port + P_CMD) & CMD_FIS_RUNNING == 0) {
            return false;
        }
        write32(port + P_CLB, (port_page + LIST as u64) as u32);
        write32(port + P_CLBU, ((port_page + LIST as u64) >> 32) as u32);
        write32(port + P_FB, (port_page + RECEIVED as u64) as u32);
        write32(port + P_FBU, ((port_page + RECEIVED as u64) >> 32) as u32);
        write32(port + P_SERR, u32::MAX);
        write32(port + P_IS, u32::MAX);
        write32(port + P_CMD, read32(port + P_CMD) | CMD_FIS_RECEIVE);
        // A disk that has spun up says so at once; one still spinning, in
        // a few seconds.
        until(10_000, || {
            read32(port + P_SIG) != SIGNATURE_NONE && read32(port + P_TFD) & (TFD_BUSY | TFD_DRQ) == 0
        });
        if read32(port + P_SIG) == SIGNATURE_ATA {
            return true;
        }
        write32(port + P_CMD, read32(port + P_CMD) & !CMD_FIS_RECEIVE);
        until(500, || read32(port + P_CMD) & CMD_FIS_RUNNING == 0);
        false
    });
    let Some((n, port)) = found else {
        stop("no disk on any of the controller's ports");
    };
    write32(hba + IS, u32::MAX);

    // Its interrupt: a message of its own, which the kernel aims; or its
    // line.
    let (irq, by_message) = match pci::message(device) {
        Some(irq) => (irq, true),
        None => {
            let line = info.header.line;
            if line == 0 || line >= 16 || syscall::sys_irq_register(line).is_err() {
                stop("no interrupt to be had for the controller");
            }
            (line, false)
        }
    };
    write32(port + P_IE, IE_WANTED);
    write32(hba + GHC, read32(hba + GHC) | GHC_INTERRUPTS);
    write32(port + P_CMD, read32(port + P_CMD) | CMD_START);

    let mut ahci = Ahci { hba, port, page: port_page, data, sectors: 0, irq, by_message, seen: core::cell::Cell::new(0) };
    if !ahci.run(IDENTIFY, 0, 0, port_page + IDENTIFY_AT as u64, 512, false) {
        stop("the disk does not say what it is");
    }
    let words = unsafe { core::slice::from_raw_parts((PORT_AT + IDENTIFY_AT) as *const u16, 256) };
    let lba48 = words[83] & (1 << 10) != 0;
    ahci.sectors = if lba48 {
        words[100] as u64 | (words[101] as u64) << 16 | (words[102] as u64) << 32 | (words[103] as u64) << 48
    } else {
        words[60] as u64 | (words[61] as u64) << 16
    };
    let mut model = [0u8; 40];
    for i in 0..20 {
        model[i * 2] = (words[27 + i] >> 8) as u8;
        model[i * 2 + 1] = words[27 + i] as u8;
    }
    let model_len = model.iter().rposition(|&b| b != b' ' && b != 0).map_or(0, |p| p + 1);
    println!(
        "[ahci] port {}: {}, {} sectors ({} MiB), interrupt {} ({})",
        n,
        core::str::from_utf8(&model[..model_len]).unwrap_or("a disk"),
        ahci.sectors,
        ahci.sectors / 2048,
        irq,
        if by_message { "a message of its own" } else { "its line" }
    );
    match block::register_disk() {
        Some(name) => println!("[ahci] Registered as {}.", core::str::from_utf8(&name).unwrap_or("a disk")),
        None => stop("disk0 to disk3 are all taken"),
    }
    block::serve(&mut ahci, DATA_AT)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[ahci] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
