//! The card, as the protocols see a device: a frame is lent to its driver
//! to be sent, and asked of it to be received (`quark_rt::nic`).
//!
//! Asking costs a call to the driver, so it is asked only when there may be
//! something: when the driver has said frames came, or now and then in case
//! the saying was missed, until it answers that there is none.

use quark_rt::nic;
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

pub struct Card {
    pub link: nic::Link,
    /// Frames may have come since the driver was last asked.
    pub stirred: bool,
    frame: [u8; nic::FRAME],
}

impl Card {
    pub fn new(link: nic::Link) -> Card {
        Card { link, stirred: true, frame: [0; nic::FRAME] }
    }
}

pub struct Rx<'a>(&'a [u8]);
pub struct Tx<'a>(&'a nic::Link);

impl phy::RxToken for Rx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = [0u8; nic::FRAME];
        let len = len.min(frame.len());
        let r = f(&mut frame[..len]);
        let _ = self.0.send(&frame[..len]);
        r
    }
}

impl Device for Card {
    type RxToken<'a> = Rx<'a> where Self: 'a;
    type TxToken<'a> = Tx<'a> where Self: 'a;

    fn receive(&mut self, _: Instant) -> Option<(Rx<'_>, Tx<'_>)> {
        if !self.stirred {
            return None;
        }
        let n = self.link.receive(&mut self.frame);
        if n == 0 {
            self.stirred = false;
            return None;
        }
        Some((Rx(&self.frame[..n]), Tx(&self.link)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        Some(Tx(&self.link))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = nic::FRAME;
        caps
    }
}
