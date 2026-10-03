//! USB, as a host controller's driver tells it: what is plugged in, where.
//!
//! Each xHCI controller has a driver of its own (`usb`), started by the
//! device manager. It drives the hubs, keyboards, mice and disks plugged into
//! the controller itself — keys and movement go to `input`, a disk is a
//! `diskN` — and registers as the first of `usb0` to `usb3` nobody has, to
//! answer anybody who asks what there is:
//!
//! | Tag | Asks | Answer |
//! |---|---|---|
//! | [`TAG_DEVICE`] | the device at index `data[0]` | where it is, what it is, and its name ([`Device::words`]) |
//!
//! An index past the last device is answered with tag `u64::MAX`.

use crate::ipc::Message;
use crate::syscall;

pub const TAG_DEVICE: u64 = 1;

// How fast a device runs: the numbers the controller says.
pub const SPEED_FULL: u8 = 1;
pub const SPEED_LOW: u8 = 2;
pub const SPEED_HIGH: u8 = 3;
pub const SPEED_SUPER: u8 = 4;

// What a device is to the driver, a bit each.
pub const ROLE_KEYBOARD: u8 = 1 << 0;
pub const ROLE_MOUSE: u8 = 1 << 1;
pub const ROLE_HUB: u8 = 1 << 2;
pub const ROLE_DISK: u8 = 1 << 3;

/// A device plugged in.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Device {
    /// The controller's number for it.
    pub slot: u8,
    /// The controller's port it is under, and the way from there through
    /// hubs: a hub's port a half-byte each, nearest first.
    pub port: u8,
    pub route: u32,
    pub speed: u8,
    /// How many hubs it is behind.
    pub depth: u8,
    pub vendor: u16,
    pub product: u16,
    /// Its class, where the device says it, else its first interface's.
    pub class: u8,
    /// What it is to the driver: `ROLE_*`.
    pub roles: u8,
    /// What it says it is called, as much as fits.
    pub name: [u8; 24],
}

impl Device {
    pub fn words(&self) -> [u64; 6] {
        let mut name = [0u64; 3];
        for (i, chunk) in self.name.chunks(8).enumerate() {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            name[i] = u64::from_le_bytes(word);
        }
        [
            self.slot as u64
                | (self.port as u64) << 8
                | (self.speed as u64) << 16
                | (self.depth as u64) << 24
                | (self.route as u64) << 32,
            self.vendor as u64 | (self.product as u64) << 16 | (self.class as u64) << 32 | (self.roles as u64) << 40,
            name[0],
            name[1],
            name[2],
            0,
        ]
    }

    pub fn from_words(w: [u64; 6]) -> Device {
        let mut name = [0u8; 24];
        for i in 0..3 {
            name[i * 8..i * 8 + 8].copy_from_slice(&w[2 + i].to_le_bytes());
        }
        Device {
            slot: w[0] as u8,
            port: (w[0] >> 8) as u8,
            speed: (w[0] >> 16) as u8,
            depth: (w[0] >> 24) as u8,
            route: (w[0] >> 32) as u32,
            vendor: w[1] as u16,
            product: (w[1] >> 16) as u16,
            class: (w[1] >> 32) as u8,
            roles: (w[1] >> 40) as u8,
            name,
        }
    }

    pub fn name(&self) -> &[u8] {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        &self.name[..len]
    }

    pub fn speed_name(&self) -> &'static str {
        match self.speed {
            SPEED_LOW => "1.5 Mb/s",
            SPEED_FULL => "12 Mb/s",
            SPEED_HIGH => "480 Mb/s",
            SPEED_SUPER => "5 Gb/s",
            5 => "10 Gb/s",
            _ => "?",
        }
    }
}

/// The device at `index` of the controller whose driver is `server`.
pub fn device(server: usize, index: usize) -> Option<Device> {
    let msg = Message { sender: 0, tag: TAG_DEVICE, data: [index as u64, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if !matches!(syscall::sys_call_timeout(server, &msg, &mut reply, 100), syscall::CallOutcome::Replied) {
        return None;
    }
    (reply.tag == 0).then(|| Device::from_words(reply.data))
}

/// Every controller's driver there is, by name: `usb0`, `usb1`, ...
pub fn controllers() -> impl Iterator<Item = ([u8; 4], usize)> {
    (0..4u8).filter_map(|n| {
        let name = [b'u', b's', b'b', b'0' + n];
        crate::nameserver::lookup(&name).map(|tid| (name, tid))
    })
}
