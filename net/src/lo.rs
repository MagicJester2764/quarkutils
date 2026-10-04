//! `lo`: what this machine says to itself, as a device smoltcp drives.
//!
//! smoltcp has one (`phy::Loopback`), whose queue is its own business. This
//! one says whether anything is still in it, which is what lets a turn of
//! the loop go on until a conversation the machine is having with itself
//! has gone as far as it can: a packet one poll sends is received by the
//! next, and what a poll's receiving answers — a reset, an echo's reply —
//! would otherwise wait for a timer nobody set.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// As large as an IP packet can say: a segment over `lo` is never cut up.
const MTU: usize = 65535;

pub struct Lo {
    queue: VecDeque<Vec<u8>>,
}

impl Lo {
    pub fn new() -> Lo {
        Lo { queue: VecDeque::new() }
    }

    /// Whether anything sent has not yet been received.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

pub struct Rx(Vec<u8>);
pub struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl Device for Lo {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = MTU;
        caps.medium = Medium::Ip;
        caps
    }

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        self.queue.pop_front().map(|packet| (Rx(packet), Tx(&mut self.queue)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.queue))
    }
}

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}
