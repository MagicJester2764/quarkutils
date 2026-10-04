//! A USB disk, as a thread of this program running `block::serve`: a disk
//! like any other to whoever claims it. Its reads and writes are the
//! controller's thread's to do — it has the controller — so each is put in
//! this disk's place in `SHARED`, the controller's thread is told, and this
//! waits until it is done.

use crate::shared::{Request, DONE, SHARED, TOLD_DISK};
use quark_rt::block::{self, Device, MAX_SECTORS, SECTOR};
use quark_rt::{println, syscall};

struct Disk {
    index: usize,
    generation: u32,
    sectors: u64,
    data: usize,
}

impl Disk {
    fn ask(&mut self, write: bool, lba: u64, count: u32, buf: usize) -> bool {
        let bytes = count as usize * SECTOR;
        if count == 0 || count > MAX_SECTORS || lba.checked_add(count as u64).is_none_or(|end| end > self.sectors) {
            return false;
        }
        let offset = buf.wrapping_sub(self.data);
        if offset + bytes > 4096 {
            return false;
        }
        self.request(Request { write, lba, count, offset })
    }

    /// Hand `request` to the controller's thread and wait for it.
    fn request(&mut self, request: Request) -> bool {
        let main = {
            let mut shared = SHARED.lock();
            let disk = &mut shared.disks[self.index];
            if !disk.present || disk.generation != self.generation {
                return false;
            }
            disk.request = Some(request);
            shared.main
        };
        if syscall::sys_notify(main, TOLD_DISK).is_err() {
            SHARED.lock().disks[self.index].request = None;
            return false;
        }
        DONE[self.index].acquire();
        SHARED.lock().disks[self.index].ok
    }
}

impl Device for Disk {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, count: u32, into: &mut [u8]) -> bool {
        into.len() >= count as usize * SECTOR && self.ask(false, lba, count, into.as_ptr() as usize)
    }

    fn write(&mut self, lba: u64, count: u32, from: &[u8]) -> bool {
        from.len() >= count as usize * SECTOR && self.ask(true, lba, count, from.as_ptr() as usize)
    }

    fn flush(&mut self) -> bool {
        self.request(Request { write: true, lba: 0, count: 0, offset: 0 })
    }

    fn gone(&self) -> bool {
        let shared = SHARED.lock();
        let disk = &shared.disks[self.index];
        !disk.present || disk.generation != self.generation
    }
}

/// A disk's thread: `index` is its place in `SHARED`.
pub extern "C" fn serve(index: usize) -> ! {
    let (generation, sectors, data) = {
        let shared = SHARED.lock();
        let disk = &shared.disks[index];
        (disk.generation, disk.sectors, disk.data)
    };
    match block::register_removable_disk() {
        Some(name) => println!("[usb] Registered a disk as {}.", core::str::from_utf8(&name).unwrap_or("a disk")),
        None => {
            println!("[usb] disk0 to disk3 are all taken; this disk is not served");
            syscall::sys_exit_code(1);
        }
    }
    let mut disk = Disk { index, generation, sectors, data };
    block::serve(&mut disk, data)
}
