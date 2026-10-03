#![no_std]
#![no_main]

//! An NVMe disk: a controller on PCI Express that is given commands in
//! queues in memory and answers in others (NVM Express 1.x).
//!
//! Started by the device manager for one, holding it. The controller is
//! reset and given an admin queue pair — where it is told what to do with
//! itself — through which it says what it is and which of its namespaces
//! is the first and how big, and is asked for one I/O queue pair: where a
//! read or a write goes, with the address of the page the sectors are in,
//! and comes back with a phase bit that turns over each time round. Every
//! queue and that page are this program's own memory, so on a machine with
//! an IOMMU they are what the controller reaches. Its interrupt is the
//! best it has (`pci::interrupt`). Everything above the sectors is
//! `quark_rt::block`, as it is for every disk.

use quark_rt::block::{self, Device as Disk, MAX_SECTORS, SECTOR};
use quark_rt::ipc::Message;
use quark_rt::manifest::CapReq;
use quark_rt::{pci, println, syscall};

// A driver's band; an NVMe controller; and frames for its four queues, the
// page sectors go through and the one it identifies itself into.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    CapReq::drives_interface(0x01, 0x08, 0x02),
    CapReq::phys_alloc(8),
]);

/// Where the controller's registers (BAR 0) are mapped, its MSI-X table's
/// BAR when that is another, and the slots their ranges are minted in.
const REGS_AT: usize = 0xA0_0000_0000;
const TABLE_AT: usize = 0xA1_0000_0000;
const REGS_SLOT: usize = 10;
const TABLE_SLOT: usize = 11;
/// The page `block::serve` keeps sectors in; each queue, a page of its own
/// since a queue begins on one; a page to be identified into.
const DATA_AT: usize = 0x86_0000_0000;
const ADMIN_SQ_AT: usize = 0x86_0000_1000;
const ADMIN_CQ_AT: usize = 0x86_0000_2000;
const IO_SQ_AT: usize = 0x86_0000_3000;
const IO_CQ_AT: usize = 0x86_0000_4000;
const IDENTIFY_AT: usize = 0x86_0000_5000;
/// Entries in each queue: 32 submissions of 64 bytes, 32 completions of 16.
const DEPTH: u16 = 32;

// The controller's registers.
const CAP: usize = 0x00;
const CC: usize = 0x14;
const CSTS: usize = 0x1C;
const AQA: usize = 0x24;
const ASQ: usize = 0x28;
const ACQ: usize = 0x30;
const DOORBELLS: usize = 0x1000;
/// The NVM command set, among those it has.
const CAP_NVM: u64 = 1 << 37;
const CC_ENABLE: u32 = 1;
/// Submission entries of 2^6 bytes and completions of 2^4; pages of 4 KiB
/// and the NVM command set, which are noughts.
const CC_SIZES: u32 = 6 << 16 | 4 << 20;
const CSTS_READY: u32 = 1;
const CSTS_FATAL: u32 = 2;

// Commands.
const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;
/// What an identify is for: a namespace, the controller, the namespaces
/// there are.
const IDENTIFY_NAMESPACE: u32 = 0;
const IDENTIFY_CONTROLLER: u32 = 1;
const IDENTIFY_ACTIVE: u32 = 2;

/// How long a command may take: tenths of a second.
const PATIENCE: u64 = 300;

fn read32(at: usize) -> u32 {
    unsafe { core::ptr::read_volatile(at as *const u32) }
}
fn write32(at: usize, v: u32) {
    unsafe { core::ptr::write_volatile(at as *mut u32, v) }
}
fn read64(at: usize) -> u64 {
    read32(at) as u64 | (read32(at + 4) as u64) << 32
}
fn write64(at: usize, v: u64) {
    write32(at, v as u32);
    write32(at + 4, (v >> 32) as u32);
}

/// A queue pair: submissions go in at the tail, completions are read at
/// the head while their phase bit is the one expected.
struct Queues {
    sq: usize,
    cq: usize,
    tail: u16,
    head: u16,
    phase: u16,
    sq_bell: usize,
    cq_bell: usize,
    next_id: u16,
}

impl Queues {
    /// Submit command `opcode` for namespace `nsid`, its data at `prp1`
    /// and its own words ten to fifteen in `cdw`, and ring the doorbell:
    /// the id it was given.
    fn submit(&mut self, opcode: u8, nsid: u32, prp1: u64, cdw: [u32; 6]) -> u16 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let entry = self.sq + self.tail as usize * 64;
        unsafe { core::ptr::write_bytes(entry as *mut u8, 0, 64) };
        write32(entry, opcode as u32 | (id as u32) << 16);
        write32(entry + 4, nsid);
        write64(entry + 24, prp1);
        for (i, w) in cdw.iter().enumerate() {
            write32(entry + 40 + 4 * i, *w);
        }
        self.tail = (self.tail + 1) % DEPTH;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write32(self.sq_bell, self.tail as u32);
        id
    }

    /// The next completion, if there is one: its command's id and status.
    fn complete(&mut self) -> Option<(u16, u16)> {
        let entry = self.cq + self.head as usize * 16;
        let dw3 = read32(entry + 12);
        if ((dw3 >> 16) & 1) as u16 != self.phase {
            return None;
        }
        let (id, status) = (dw3 as u16, ((dw3 >> 17) & 0x7FFF) as u16);
        self.head = (self.head + 1) % DEPTH;
        if self.head == 0 {
            self.phase ^= 1;
        }
        write32(self.cq_bell, self.head as u32);
        Some((id, status))
    }
}

struct Nvme {
    admin: Queues,
    io: Queues,
    nsid: u32,
    sectors: u64,
    data: u64,
    interrupt: pci::Interrupt,
}

impl Nvme {
    /// Wait for command `id` to finish, on the I/O queues or the admin
    /// ones: whether it did, and well.
    fn wait(&mut self, io: bool, id: u16) -> bool {
        for _ in 0..PATIENCE * 10 {
            let queues = if io { &mut self.io } else { &mut self.admin };
            while let Some((done, status)) = queues.complete() {
                if done == id {
                    if status != 0 {
                        println!("[nvme] a command came back with status {:#x}", status);
                    }
                    return status == 0;
                }
            }
            let mut msg = Message::empty();
            if syscall::sys_recv_timeout(0, &mut msg, syscall::ns(10_000_000)).is_ok() {
                if msg.sender == 0 && msg.tag == self.interrupt.number() as u64 {
                    if self.interrupt.is_line() {
                        syscall::sys_irq_ack(self.interrupt.number());
                    }
                } else {
                    // A death, most likely: block::serve's to hear.
                    quark_rt::ipc::keep(&msg);
                }
            }
        }
        println!("[nvme] the controller has not answered a command in {} seconds", PATIENCE / 10);
        false
    }

    fn admin(&mut self, opcode: u8, nsid: u32, prp1: u64, cdw: [u32; 6]) -> bool {
        let id = self.admin.submit(opcode, nsid, prp1, cdw);
        self.wait(false, id)
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
        // Inside one page, so one address and no list of them.
        let opcode = if write { IO_WRITE } else { IO_READ };
        let cdw = [lba as u32, (lba >> 32) as u32, count - 1, 0, 0, 0];
        let id = self.io.submit(opcode, self.nsid, self.data + offset as u64, cdw);
        self.wait(true, id)
    }
}

impl Disk for Nvme {
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
    println!("[nvme] {}", why);
    syscall::sys_exit_code(1);
}

/// A page of this program's own memory at `at`: where it is.
fn page(at: usize) -> Option<u64> {
    let frame = syscall::sys_phys_alloc(1).ok()?;
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

fn byte(at: usize) -> u8 {
    unsafe { core::ptr::read_volatile(at as *const u8) }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let Some(device) = pci::this_device().filter(|&d| pci::info(d).is_some()) else {
        stop("started without a controller: the device manager starts this, for an NVMe controller.");
    };
    let Some(regs) = pci::map_bar(device, 0, REGS_AT, REGS_SLOT) else {
        stop("may not map the controller's registers");
    };
    if pci::claim(device).is_err() {
        stop("the controller is another program's");
    }
    if pci::enable(device, pci::COMMAND_MEMORY | pci::COMMAND_MASTER).is_err() {
        stop("the controller may not be turned on");
    }
    let cap = read64(regs + CAP);
    if cap & CAP_NVM == 0 || (cap >> 48) & 0xF != 0 {
        stop("the controller has no NVM commands, or no pages as small as 4 KiB");
    }
    if (cap & 0xFFFF) + 1 < DEPTH as u64 {
        stop("the controller's queues are shorter than this asks for");
    }
    let stride = 4usize << ((cap >> 32) & 0xF);
    // How long it may take to be ready: units of half a second.
    let ready_ms = ((cap >> 24) & 0xFF).max(1) * 500;

    // Off, given its admin queues, and on.
    write32(regs + CC, read32(regs + CC) & !CC_ENABLE);
    if !until(ready_ms, || read32(regs + CSTS) & CSTS_READY == 0) {
        stop("the controller does not turn off");
    }
    let pages = [DATA_AT, ADMIN_SQ_AT, ADMIN_CQ_AT, IO_SQ_AT, IO_CQ_AT, IDENTIFY_AT].map(page);
    let [Some(data), Some(admin_sq), Some(admin_cq), Some(io_sq), Some(io_cq), Some(identify)] = pages else {
        stop("no memory for the controller's queues");
    };
    let entries = (DEPTH - 1) as u32;
    write32(regs + AQA, entries | entries << 16);
    write64(regs + ASQ, admin_sq);
    write64(regs + ACQ, admin_cq);
    write32(regs + CC, CC_SIZES | CC_ENABLE);
    if !until(ready_ms, || read32(regs + CSTS) & (CSTS_READY | CSTS_FATAL) != 0)
        || read32(regs + CSTS) & CSTS_FATAL != 0
    {
        stop("the controller does not come on");
    }
    let bell = |queue: usize, completion: bool| regs + DOORBELLS + (2 * queue + completion as usize) * stride;
    let queues = |sq: usize, cq: usize, id: usize| Queues {
        sq,
        cq,
        tail: 0,
        head: 0,
        phase: 1,
        sq_bell: bell(id, false),
        cq_bell: bell(id, true),
        next_id: 1,
    };
    let interrupt = pci::interrupt(device, |bar| {
        if bar == 0 { Some(regs) } else { pci::map_bar(device, bar, TABLE_AT, TABLE_SLOT) }
    })
    .unwrap_or_else(|| stop("no interrupt to be had for the controller"));
    let mut nvme = Nvme {
        admin: queues(ADMIN_SQ_AT, ADMIN_CQ_AT, 0),
        io: queues(IO_SQ_AT, IO_CQ_AT, 1),
        nsid: 1,
        sectors: 0,
        data,
        interrupt,
    };

    // What it is; its first namespace — the first in the list of those
    // there are, where it keeps one, else the first there can be; and how
    // many blocks that has, and how big.
    if !nvme.admin(ADMIN_IDENTIFY, 0, identify, [IDENTIFY_CONTROLLER, 0, 0, 0, 0, 0]) {
        stop("the controller does not say what it is");
    }
    let mut model = [0u8; 40];
    for (i, b) in model.iter_mut().enumerate() {
        *b = byte(IDENTIFY_AT + 24 + i);
    }
    let model_len = model.iter().rposition(|&b| b != b' ' && b != 0).map_or(0, |p| p + 1);
    if nvme.admin(ADMIN_IDENTIFY, 0, identify, [IDENTIFY_ACTIVE, 0, 0, 0, 0, 0]) {
        let first = read32(IDENTIFY_AT);
        if first == 0 {
            stop("the controller has no namespace");
        }
        nvme.nsid = first;
    }
    if !nvme.admin(ADMIN_IDENTIFY, nvme.nsid, identify, [IDENTIFY_NAMESPACE, 0, 0, 0, 0, 0]) {
        stop("the controller does not say what its namespace is");
    }
    let size = read64(IDENTIFY_AT);
    let format = IDENTIFY_AT + 128 + 4 * (byte(IDENTIFY_AT + 26) & 0xF) as usize;
    let metadata = read32(format) & 0xFFFF;
    if byte(format + 2) != 9 || metadata != 0 {
        stop("the namespace's blocks are not 512 bytes and nothing else, and only those are read here");
    }
    nvme.sectors = size;

    // One I/O queue pair: completions with an interrupt, the first of the
    // controller's; submissions to them.
    let qsize = ((DEPTH - 1) as u32) << 16;
    let contiguous = 1;
    let with_interrupt = 1 << 1;
    let cq = [1 | qsize, contiguous | with_interrupt, 0, 0, 0, 0];
    let sq = [1 | qsize, contiguous | 1 << 16, 0, 0, 0, 0];
    if !nvme.admin(ADMIN_CREATE_CQ, 0, io_cq, cq) || !nvme.admin(ADMIN_CREATE_SQ, 0, io_sq, sq) {
        stop("the controller would not make a queue to read and write with");
    }
    println!(
        "[nvme] {}, namespace {}: {} sectors ({} MiB), interrupt {} ({})",
        core::str::from_utf8(&model[..model_len]).unwrap_or("a disk"),
        nvme.nsid,
        nvme.sectors,
        nvme.sectors / 2048,
        interrupt.number(),
        interrupt.describe()
    );
    match block::register_disk() {
        Some(name) => println!("[nvme] Registered as {}.", core::str::from_utf8(&name).unwrap_or("a disk")),
        None => stop("disk0 to disk3 are all taken"),
    }
    block::serve(&mut nvme, DATA_AT)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[nvme] PANIC: {}", info);
    syscall::sys_exit_code(1);
}
