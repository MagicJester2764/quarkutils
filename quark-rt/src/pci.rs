//! PCI devices, as a driver and the device manager see them.
//!
//! The kernel finds every device once, at boot, and keeps what it found: its
//! ids, its class, its interrupt line and its BARs, sized. A program reaches
//! a device through a capability for it (`CAP_TYPE_PCI_DEVICE`), which the
//! device manager holds for every device and hands each driver for its own:
//! with it the driver reads and writes the device's configuration, maps its
//! BARs, claims it and has its interrupt. Nothing scans the bus but the
//! kernel, and nothing reaches a device it was not given.

use crate::syscall::{self, PCI_RECORD};

/// A device's address on the first PCI segment: `bus << 8 | device << 3 |
/// function`, as the kernel names one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address(pub u16);

impl Address {
    pub const fn new(bus: u8, device: u8, function: u8) -> Address {
        Address((bus as u16) << 8 | ((device & 0x1F) as u16) << 3 | (function & 7) as u16)
    }

    pub const fn bus(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub const fn device(self) -> u8 {
        (self.0 >> 3) as u8 & 0x1F
    }

    pub const fn function(self) -> u8 {
        self.0 as u8 & 7
    }

    /// As the kernel's calls take it.
    pub const fn raw(self) -> u64 {
        self.0 as u64
    }

    /// `BB:DD.F`, in hexadecimal, as `lspci` writes one.
    pub fn parse(text: &[u8]) -> Option<Address> {
        let hex = |b: &[u8]| -> Option<u8> {
            if b.is_empty() || b.len() > 2 {
                return None;
            }
            b.iter().try_fold(0u8, |n, &c| Some(n * 16 + (c as char).to_digit(16)? as u8))
        };
        let colon = text.iter().position(|&c| c == b':')?;
        let dot = text.iter().position(|&c| c == b'.')?;
        if dot < colon || dot + 2 != text.len() {
            return None;
        }
        let (bus, device, function) = (hex(&text[..colon])?, hex(&text[colon + 1..dot])?, hex(&text[dot + 1..])?);
        (device < 32 && function < 8).then(|| Address::new(bus, device, function))
    }

    /// The same, written into `out`.
    pub fn write(self, out: &mut [u8; 7]) -> &[u8] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let (bus, device) = (self.bus(), self.device());
        *out = [
            DIGITS[(bus >> 4) as usize],
            DIGITS[(bus & 15) as usize],
            b':',
            DIGITS[(device >> 4) as usize],
            DIGITS[(device & 15) as usize],
            b'.',
            DIGITS[self.function() as usize],
        ];
        &out[..]
    }
}

/// What every description of a device begins with: the first three words
/// of the kernel's record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub address: u16,
    /// 0 for a device, 1 for a bridge to another bus.
    pub header_type: u8,
    /// Which of the four interrupt pins it uses, 1 for A; 0 for none.
    pub pin: u8,
    /// The interrupt line the firmware wired that pin to.
    pub line: u8,
    /// Where in its configuration its MSI and MSI-X capabilities are; 0 for
    /// none.
    pub msi: u8,
    pub msix: u8,
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem: u16,
    /// Class, subclass and programming interface, a byte each.
    pub class: u32,
    pub revision: u8,
}

impl Header {
    pub fn from_words(w: [u64; 3]) -> Header {
        Header {
            address: w[0] as u16,
            header_type: (w[0] >> 16) as u8,
            pin: (w[0] >> 24) as u8,
            line: (w[0] >> 32) as u8,
            msi: (w[0] >> 40) as u8,
            msix: (w[0] >> 48) as u8,
            vendor: w[1] as u16,
            device: (w[1] >> 16) as u16,
            subsystem_vendor: (w[1] >> 32) as u16,
            subsystem: (w[1] >> 48) as u16,
            class: w[2] as u32 & 0xFF_FFFF,
            revision: (w[2] >> 24) as u8,
        }
    }

    pub fn words(&self) -> [u64; 3] {
        [
            self.address as u64
                | (self.header_type as u64) << 16
                | (self.pin as u64) << 24
                | (self.line as u64) << 32
                | (self.msi as u64) << 40
                | (self.msix as u64) << 48,
            self.vendor as u64
                | (self.device as u64) << 16
                | (self.subsystem_vendor as u64) << 32
                | (self.subsystem as u64) << 48,
            self.class as u64 | (self.revision as u64) << 24,
        ]
    }

    pub fn address(&self) -> Address {
        Address(self.address)
    }

    /// What a driver's manifest is matched against: vendor, device and
    /// class code (`manifest::CapReq::drives` and the like).
    pub fn key(&self) -> u64 {
        (self.vendor as u64) << 48 | (self.device as u64) << 32 | (self.class as u64) << 8
    }

    /// What kind of device it is, in words, as far as this knows.
    pub fn kind(&self) -> &'static str {
        match self.class >> 8 {
            0x0000 => "Non-VGA unclassified device",
            0x0100 => "SCSI storage controller",
            0x0101 => "IDE interface",
            0x0105 => "ATA controller",
            0x0106 => "SATA controller",
            0x0107 => "Serial Attached SCSI controller",
            0x0108 => "Non-Volatile memory controller",
            0x0180 => "Mass storage controller",
            0x0200 => "Ethernet controller",
            0x0280 => "Network controller",
            0x0300 => "VGA compatible controller",
            0x0302 => "3D controller",
            0x0380 => "Display controller",
            0x0401 => "Multimedia audio controller",
            0x0403 => "Audio device",
            0x0480 => "Multimedia controller",
            0x0500 => "RAM memory",
            0x0600 => "Host bridge",
            0x0601 => "ISA bridge",
            0x0604 => "PCI bridge",
            0x0680 => "Bridge",
            0x0700 => "Serial controller",
            0x0780 => "Communication controller",
            0x0880 => "System peripheral",
            0x0C03 => "USB controller",
            0x0C05 => "SMBus",
            0x00FF => "Unassigned class",
            _ => "Device",
        }
    }
}

/// A BAR's flags, as the kernel says them.
pub const BAR_PORTS: u64 = 1;
pub const BAR_WIDE: u64 = 2;
pub const BAR_PREFETCH: u64 = 4;

/// One of a device's BARs: where the firmware put it and how long it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bar {
    pub base: u64,
    pub size: u64,
    pub flags: u64,
}

impl Bar {
    /// Whether there is one.
    pub fn present(&self) -> bool {
        self.size != 0
    }

    pub fn is_ports(&self) -> bool {
        self.flags & BAR_PORTS != 0
    }
}

/// All the kernel says of a device.
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub header: Header,
    pub bars: [Bar; 6],
}

impl Info {
    pub fn from_record(r: &[u64; PCI_RECORD]) -> Info {
        let mut bars = [Bar::default(); 6];
        for (n, bar) in bars.iter_mut().enumerate() {
            *bar = Bar { base: r[3 + 3 * n], size: r[4 + 3 * n], flags: r[5 + 3 * n] };
        }
        Info { header: Header::from_words([r[0], r[1], r[2]]), bars }
    }
}

/// What the kernel found of `address`, if this program holds it.
pub fn info(address: Address) -> Option<Info> {
    let mut record = [0u64; PCI_RECORD];
    (syscall::sys_pci_device(address.raw(), &mut record)? == address.raw()).then(|| Info::from_record(&record))
}

/// Every device this program holds, in order of address: the device
/// manager's whole machine, a driver's one device.
pub fn devices() -> impl Iterator<Item = Info> {
    let mut from = 0u64;
    core::iter::from_fn(move || {
        let mut record = [0u64; PCI_RECORD];
        let at = syscall::sys_pci_device(from, &mut record)?;
        from = at + 1;
        Some(Info::from_record(&record))
    })
}

/// The device a driver was started for: the device manager gives its
/// address as the first argument.
pub fn this_device() -> Option<Address> {
    crate::args::argv(1).and_then(Address::parse)
}

pub fn read8(at: Address, offset: u16) -> Option<u8> {
    syscall::sys_pci_read(at.raw(), offset as u64, 1).ok().map(|v| v as u8)
}

pub fn read16(at: Address, offset: u16) -> Option<u16> {
    syscall::sys_pci_read(at.raw(), offset as u64, 2).ok().map(|v| v as u16)
}

pub fn read32(at: Address, offset: u16) -> Option<u32> {
    syscall::sys_pci_read(at.raw(), offset as u64, 4).ok()
}

pub fn write16(at: Address, offset: u16, value: u16) -> Result<(), syscall::Refused> {
    syscall::sys_pci_write(at.raw(), offset as u64, 2, value as u32)
}

pub fn write32(at: Address, offset: u16, value: u32) -> Result<(), syscall::Refused> {
    syscall::sys_pci_write(at.raw(), offset as u64, 4, value)
}

/// The command register, and what in it a driver turns on.
pub const COMMAND: u16 = 0x04;
pub const COMMAND_PORTS: u16 = 1 << 0;
pub const COMMAND_MEMORY: u16 = 1 << 1;
pub const COMMAND_MASTER: u16 = 1 << 2;

/// Have the device answer at its ports, at its memory, and copy memory
/// itself, as `bits` says — added to what it does already. Copying memory
/// is refused until the device is this program's ([`claim`]).
pub fn enable(at: Address, bits: u16) -> Result<(), syscall::Refused> {
    let command = read16(at, COMMAND).ok_or(syscall::Refused::NoSuch)?;
    write16(at, COMMAND, command | bits)
}

/// Make the device this program's ([`syscall::sys_device_claim`]): before
/// it copies any memory.
pub fn claim(at: Address) -> Result<bool, syscall::Refused> {
    syscall::sys_device_claim(at.raw())
}

/// An interrupt of the device's own, sent as a message: its number. The
/// kernel aims the device at it; a device with no MSI capability has none
/// to be aimed, and is not given one.
pub fn message(at: Address) -> Option<u8> {
    if info(at)?.header.msi == 0 {
        return None;
    }
    syscall::sys_msi_alloc_for(at.raw()).ok().map(|m| m.irq)
}

/// How a device's interrupt comes, and its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// A message the kernel aimed the device at (MSI).
    Message(u8),
    /// A message written into entry 0 of the device's own table (MSI-X).
    Table(u8),
    /// Its line, which is acknowledged after each (`sys_irq_ack`).
    Line(u8),
}

impl Interrupt {
    pub fn number(self) -> u8 {
        match self {
            Interrupt::Message(n) | Interrupt::Table(n) | Interrupt::Line(n) => n,
        }
    }

    pub fn is_line(self) -> bool {
        matches!(self, Interrupt::Line(_))
    }

    pub fn describe(self) -> &'static str {
        match self {
            Interrupt::Message(_) => "a message of its own",
            Interrupt::Table(_) => "a message of its own, from its table",
            Interrupt::Line(_) => "its line",
        }
    }
}

const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_MASK_ALL: u16 = 1 << 14;

/// The device's interrupt, the best it has. A message the kernel aims it
/// at, where it has MSI. Else entry 0 of its MSI-X table, which is in one
/// of its memory BARs — `map` is told which, and answers where that BAR
/// begins in this program — with a message from the kernel written into
/// it and MSI-X turned on. Else its line, registered; the device manager
/// gave the capability for it.
///
/// A device with MSI is never given MSI-X as well: the kernel turns MSI on
/// as it aims it, and a device with both on does what it likes.
pub fn interrupt(at: Address, map: impl FnOnce(usize) -> Option<usize>) -> Option<Interrupt> {
    let header = info(at)?.header;
    if header.msi != 0 {
        if let Some(irq) = message(at) {
            return Some(Interrupt::Message(irq));
        }
    } else if header.msix != 0 {
        if let Some(irq) = table_entry(at, header.msix as u16, map) {
            return Some(Interrupt::Table(irq));
        }
    }
    let line = header.line;
    (line != 0 && line < 16 && syscall::sys_irq_register(line).is_ok()).then_some(Interrupt::Line(line))
}

/// Entry 0 of the MSI-X table of the device whose capability is at `cap`.
fn table_entry(at: Address, cap: u16, map: impl FnOnce(usize) -> Option<usize>) -> Option<u8> {
    let table = read32(at, cap + 4)?;
    let entry = map((table & 7) as usize)? + (table & !7) as usize;
    let message = syscall::sys_msi_alloc_for(at.raw()).ok()?;
    let words = [message.address, 0, message.data as u32, 0];
    for (i, word) in words.iter().enumerate() {
        unsafe { core::ptr::write_volatile((entry + 4 * i) as *mut u32, *word) };
    }
    let control = read16(at, cap + 2)?;
    write16(at, cap + 2, (control | MSIX_ENABLE) & !MSIX_MASK_ALL).ok()?;
    Some(message.irq)
}

/// Map memory BAR `n` of the device at `virt`, minting the range in this
/// program's slot `slot`: where its first byte is, which is `virt` plus
/// where in its page the BAR begins. The whole of it, in pages.
pub fn map_bar(at: Address, n: usize, virt: usize, slot: usize) -> Option<usize> {
    let bar = info(at)?.bars.get(n).copied().filter(|b| b.present() && !b.is_ports() && b.base != 0)?;
    let first = bar.base & !0xFFF;
    let end = (bar.base + bar.size + 0xFFF) & !0xFFF;
    let _ = syscall::sys_cap_delete(slot);
    syscall::sys_cap_mint(slot, syscall::CAP_TYPE_PHYS_RANGE, first, end).ok()?;
    syscall::sys_map_phys(first as usize, virt, ((end - first) / 4096) as usize).ok()?;
    Some(virt + (bar.base - first) as usize)
}

/// The ports of I/O BAR `n` of the device, minted in this program's slot
/// `slot`: the first, and how many.
pub fn ports(at: Address, n: usize, slot: usize) -> Option<(u16, u16)> {
    let bar = info(at)?.bars.get(n).copied().filter(|b| b.present() && b.is_ports() && b.base != 0)?;
    let _ = syscall::sys_cap_delete(slot);
    syscall::sys_cap_mint(slot, syscall::CAP_TYPE_IOPORT, bar.base, bar.base + bar.size - 1).ok()?;
    Some((bar.base as u16, bar.size as u16))
}
