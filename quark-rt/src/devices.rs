//! What is in the machine, and who drives it: the device manager's protocol.
//!
//! `devmgr` holds every PCI device (`CAP_TYPE_PCI_DEVICE` for all of them)
//! and is the one program that starts a driver for one: a driver says in
//! its manifest which devices it drives (`manifest::CapReq::drives`), and
//! the device manager starts it for each such device that has no driver,
//! holding that device and no other, with the device's address as its first
//! argument. It registers as [`NAME`] and says what it found to anybody who
//! asks — which is all `lspci` is.
//!
//! | Tag | From | Asks | Answer |
//! |---|---|---|---|
//! | [`TAG_OFFER`] | its parent | start this driver, lent with the call (`data[0]` its length, `data[1..3]` its name) | `[how many devices it was started for]` |
//! | [`TAG_FILES`] | its parent | the root is up: start what [`DRIVERS`] has | `[how many devices drivers were started for]` |
//! | [`TAG_DEVICE`] | anybody | the device at index `data[0]` | the first three words of the kernel's record, the driver's task (0 for none), and its name in two words |
//! | [`TAG_BAR`] | anybody | BAR `data[1]` of the device at address `data[0]` | `[where, how long, flags]` |
//! | [`TAG_IS_DRIVER`] | anybody | is the program `data[0]` (a space id, `sys_task_space`) a driver it started? | `[1 if it is, else 0]` |
//!
//! An index past the last device, a BAR there is not and a request from
//! anybody else are answered with tag `u64::MAX`.

use crate::ipc::Message;
use crate::pci::{Address, Bar, Header};
use crate::syscall;

/// The name it registers.
pub const NAME: &[u8] = b"devices";
/// Where the drivers that are not in the boot image are.
pub const DRIVERS: &[u8] = b"/usr/lib/drivers";

pub const TAG_OFFER: u64 = 1;
pub const TAG_FILES: u64 = 2;
pub const TAG_DEVICE: u64 = 3;
pub const TAG_BAR: u64 = 4;
/// Asked by `input` before it takes keys from a program: a program the
/// device manager started for a device is a driver, and nothing else is.
pub const TAG_IS_DRIVER: u64 = 5;

/// A device, and its driver if it has one.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub header: Header,
    /// The driver's task, or 0.
    pub driver: usize,
    name: [u8; 16],
}

impl Device {
    /// The driver's name, as it was started; empty for none.
    pub fn driver_name(&self) -> &[u8] {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        &self.name[..len]
    }

    /// As [`TAG_DEVICE`] answers.
    pub fn words(&self) -> [u64; 6] {
        let w = self.header.words();
        let name = |half: usize| u64::from_le_bytes(self.name[half * 8..half * 8 + 8].try_into().unwrap_or([0; 8]));
        [w[0], w[1], w[2], self.driver as u64, name(0), name(1)]
    }

    pub fn from_words(d: [u64; 6]) -> Device {
        let mut name = [0u8; 16];
        name[..8].copy_from_slice(&d[4].to_le_bytes());
        name[8..].copy_from_slice(&d[5].to_le_bytes());
        Device { header: Header::from_words([d[0], d[1], d[2]]), driver: d[3] as usize, name }
    }

    pub fn new(header: Header, driver: usize, driver_name: &[u8]) -> Device {
        let mut name = [0u8; 16];
        let n = driver_name.len().min(16);
        name[..n].copy_from_slice(&driver_name[..n]);
        Device { header, driver, name }
    }
}

fn ask(manager: usize, tag: u64, data: [u64; 6]) -> Option<[u64; 6]> {
    let msg = Message { sender: 0, tag, data };
    let mut reply = Message::empty();
    (syscall::sys_call(manager, &msg, &mut reply).is_ok() && reply.tag == 0).then_some(reply.data)
}

/// Whether the device manager says task `tid`'s program is a driver it
/// started: what a server asks of a caller before it takes keys or a screen
/// from it, while the caller waits on the call that offered them. A second
/// at most: a device manager busy starting drivers is not one that said yes.
pub fn vouches_for(tid: usize) -> bool {
    let Some(manager) = crate::nameserver::lookup(NAME) else { return false };
    let space = syscall::sys_task_space(tid).unwrap_or(0);
    let msg = Message { sender: 0, tag: TAG_IS_DRIVER, data: [space, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    space != 0
        && matches!(syscall::sys_call_timeout(manager, &msg, &mut reply, 100), syscall::CallOutcome::Replied)
        && reply.tag == 0
        && reply.data[0] == 1
}

/// The device at `index` in the device manager's list, which is in order of
/// address; `None` past the last.
pub fn entry(manager: usize, index: usize) -> Option<Device> {
    ask(manager, TAG_DEVICE, [index as u64, 0, 0, 0, 0, 0]).map(Device::from_words)
}

/// BAR `n` of the device at `address`.
pub fn bar(manager: usize, address: Address, n: usize) -> Option<Bar> {
    ask(manager, TAG_BAR, [address.raw(), n as u64, 0, 0, 0, 0]).map(|d| Bar { base: d[0], size: d[1], flags: d[2] })
}
