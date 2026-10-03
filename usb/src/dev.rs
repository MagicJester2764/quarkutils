//! What is plugged in: a device given an address, asked what it is and
//! configured, and what is done with each kind of thing it can be — a
//! keyboard's or a mouse's reports, a hub's ports, a disk's commands.
//! (xHCI 4.3–4.6; USB 2.0 chapters 9 and 11; HID 1.11 appendix B; mass
//! storage bulk-only transport 1.0; SCSI's block commands.)
//!
//! All of it is the controller's thread's. A request on a device's
//! endpoint 0 or a disk's command waits for its own completion; reports,
//! ports that changed and disks' requests are kept meanwhile and seen to by
//! the loop ([`Usb::see_to`]).

use crate::hc::{self, Hc, MAX_SLOTS};
use crate::mem::{self, Page};
use crate::shared::{Key, Movement, Request, DONE, MAX_DISKS, NOTIFY_INPUT, SHARED, TOLD_DISK};
use crate::trb::{self, Ring, Trb};
use quark_rt::keys::{self, Modifiers};
use quark_rt::usb::{self as list, Device as Listed};
use quark_rt::{println, syscall, thread};

const MAX_FUNCTIONS: usize = 2;
/// Hubs within hubs: the route has room for five.
const MAX_DEPTH: u8 = 5;

// Requests (USB 2.0 9.4, 11.24; HID 7.2; bulk-only 3).
const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const GET_DESCRIPTOR: u8 = 6;
const SET_CONFIGURATION: u8 = 9;
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const GET_MAX_LUN: u8 = 0xFE;
const MASS_STORAGE_RESET: u8 = 0xFF;
// Descriptors.
const DEVICE: u16 = 1;
const CONFIGURATION: u16 = 2;
const STRING: u16 = 3;
const INTERFACE: u8 = 4;
const ENDPOINT: u8 = 5;
const HUB: u16 = 0x29;
// A hub's port features.
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_RESET: u16 = 20;
const ENDPOINT_HALT: u16 = 0;
// An endpoint context's kinds.
const EP_BULK_OUT: u32 = 2;
const EP_CONTROL: u32 = 4;
const EP_BULK_IN: u32 = 6;
const EP_INTERRUPT_IN: u32 = 7;
// A disk's commands (SCSI).
const TEST_UNIT_READY: u8 = 0x00;
const REQUEST_SENSE: u8 = 0x03;
const INQUIRY: u8 = 0x12;
const READ_CAPACITY: u8 = 0x25;
const READ_10: u8 = 0x28;
const WRITE_10: u8 = 0x2A;
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
/// Where in a disk's command page the status goes.
const CSW_AT: usize = 64;

/// How long a key is held before it is typed again, and how often after.
const REPEAT_AFTER_NS: u64 = 500_000_000;
const REPEAT_EVERY_NS: u64 = 33_000_000;

/// Where the capability to wake a disk's thread is minted.
const WAKE_SLOT: usize = 41;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Keyboard,
    Mouse,
    Hub,
    Disk,
}

/// One interface of a device, as this drives it.
struct Function {
    role: Role,
    interface: u8,
    /// The endpoint reports or sectors come in on: its context's index,
    /// its address and how much it takes at a time.
    dci_in: u8,
    addr_in: u8,
    mps_in: u16,
    /// A disk's endpoint out.
    dci_out: u8,
    addr_out: u8,
    ring_in: Ring,
    ring_out: Option<Ring>,
    /// Reports, or a disk's command and status.
    page: Page,
    /// A disk's sectors, and its place in `SHARED`.
    data: Option<Page>,
    disk: usize,
    /// A keyboard's last report.
    previous: [u8; 8],
    modifiers: Modifiers,
    tag: u32,
}

struct Device {
    slot: u8,
    /// The root port it is under, the way through hubs, how many hubs.
    port: u8,
    route: u32,
    depth: u8,
    speed: u8,
    /// The hub it is plugged into, and which port; 0 for a root port.
    parent: u8,
    parent_port: u8,
    /// The high-speed hub whose translator a slow device's transfers go
    /// through, and its port: (0, 0) for none.
    tt: (u8, u8),
    input: Page,
    output: Page,
    ep0: Ring,
    /// What a request on endpoint 0 moves, in or out.
    buffer: Page,
    /// The highest endpoint context configured.
    entries: u8,
    /// A hub's ports, and the think time of its translator.
    hub_ports: u8,
    hub_think: u8,
    listed: Listed,
    functions: [Option<Function>; MAX_FUNCTIONS],
}

/// An endpoint as a configuration says it.
#[derive(Clone, Copy, Default)]
struct Endpoint {
    address: u8,
    attributes: u8,
    mps: u16,
    interval: u8,
}

impl Endpoint {
    fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    fn is_bulk(&self) -> bool {
        self.attributes & 3 == 2
    }

    fn is_interrupt(&self) -> bool {
        self.attributes & 3 == 3
    }

    /// Its context's index: twice its number, and one more going in.
    fn dci(&self) -> u8 {
        (self.address & 0xF) * 2 + self.is_in() as u8
    }
}

#[derive(Clone, Copy, Default)]
struct Interface {
    number: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    endpoints: [Endpoint; 4],
    count: usize,
}

/// An endpoint to be configured.
struct Setup {
    dci: u8,
    kind: u32,
    mps: u16,
    interval: u8,
    ring: u64,
    burst: u8,
}

pub struct Usb {
    pub hc: Hc,
    devices: [Option<Device>; MAX_SLOTS + 1],
    /// Hub ports that said they changed: the hub's slot, and the port.
    hub_work: [(u8, u8); 64],
    nhub_work: usize,
    /// Interrupt endpoints to start again after an error.
    stalled: [(u8, u8); 16],
    nstalled: usize,
    /// The key held, to be typed again, and when.
    repeat: Option<(Key, u64)>,
    /// Keys or movement went to `input` since it was last told.
    tell_input: bool,
}

impl Usb {
    pub fn new(hc: Hc) -> Usb {
        Usb {
            hc,
            devices: [const { None }; MAX_SLOTS + 1],
            hub_work: [(0, 0); 64],
            nhub_work: 0,
            stalled: [(0, 0); 16],
            nstalled: 0,
            repeat: None,
            tell_input: false,
        }
    }

    /// Every root port with something on it, as if it had said it changed.
    pub fn scan(&mut self) {
        for port in 1..=self.hc.ports {
            if self.hc.portsc(port) & hc::PORT_CONNECTED != 0 {
                self.hc.changed[port as usize / 64] |= 1 << (port % 64);
            }
        }
    }

    /// Whether there is anything to see to before waiting again.
    pub fn busy(&self) -> bool {
        self.hc.nunclaimed > 0
            || self.hc.changed.iter().any(|&w| w != 0)
            || self.nhub_work > 0
            || self.nstalled > 0
            || self.hc.told & TOLD_DISK != 0
    }

    /// How long the loop may wait before something here is due.
    pub fn next_wait(&self) -> u64 {
        match self.repeat {
            Some((_, due)) => due.saturating_sub(syscall::sys_clock()).max(1_000_000),
            None => 1_000_000_000,
        }
    }

    /// See to what the controller and this program's threads have said.
    pub fn see_to(&mut self) {
        self.hc.pump();
        self.reports();
        self.ports();
        self.hubs();
        self.restart_stalled();
        if self.hc.told & TOLD_DISK != 0 {
            self.hc.told &= !TOLD_DISK;
            self.disks();
        }
        self.type_again();
        if self.tell_input {
            self.tell_input = false;
            let input = SHARED.lock().input;
            if input != 0 {
                let _ = syscall::sys_notify(input, NOTIFY_INPUT);
            }
        }
    }

    // ---- Ports ---------------------------------------------------------

    fn ports(&mut self) {
        for word in 0..4 {
            while self.hc.changed[word] != 0 {
                let bit = self.hc.changed[word].trailing_zeros() as usize;
                self.hc.changed[word] &= !(1 << bit);
                let port = word * 64 + bit;
                if port >= 1 && port <= self.hc.ports as usize {
                    self.root_port(port as u8);
                }
            }
        }
    }

    /// The device on root port `port`, if there is one.
    fn on_root_port(&self, port: u8) -> Option<u8> {
        self.devices.iter().flatten().find(|d| d.parent == 0 && d.port == port).map(|d| d.slot)
    }

    fn on_hub_port(&self, hub: u8, port: u8) -> Option<u8> {
        self.devices.iter().flatten().find(|d| d.parent == hub && d.parent_port == port).map(|d| d.slot)
    }

    /// Root port `port` changed: a device plugged in is given an address,
    /// one pulled out is let go.
    fn root_port(&mut self, port: u8) {
        let status = self.hc.port_seen(port);
        let here = self.on_root_port(port);
        if status & hc::PORT_CONNECTED == 0 {
            if let Some(slot) = here {
                self.detach(slot);
            }
            return;
        }
        if here.is_some() && status & hc::PORT_CONNECT_CHANGE == 0 {
            return;
        }
        if let Some(slot) = here {
            self.detach(slot);
        }
        // A connection is given a tenth of a second to settle (7.1.7.3).
        syscall::sleep_ms(100);
        if !self.hc.port_reset(port) {
            println!("[usb] port {}: the device would not be enabled", port);
            return;
        }
        let speed = self.hc.port_speed(port);
        self.attach(port, speed, 0, 0, 0, 0, (0, 0));
    }

    // ---- A device ------------------------------------------------------

    fn device(&mut self, slot: u8) -> Option<&mut Device> {
        self.devices.get_mut(slot as usize).and_then(|d| d.as_mut())
    }

    /// Give the device on `port` (the root port it is under), at `route`
    /// through hubs, an address; ask it what it is; configure it, and set
    /// up what of it this drives. Its slot.
    #[allow(clippy::too_many_arguments)]
    fn attach(&mut self, port: u8, speed: u8, route: u32, depth: u8, parent: u8, parent_port: u8, tt: (u8, u8)) -> Option<u8> {
        let slot = match self.hc.command(Trb::new(trb::ENABLE_SLOT, 0, 0, 0)) {
            Ok(done) => done.slot(),
            Err(code) => {
                println!("[usb] port {}: no slot for the device ({})", port, code);
                return None;
            }
        };
        if slot == 0 || slot as usize > MAX_SLOTS {
            let _ = self.hc.command(Trb::new(trb::DISABLE_SLOT, 0, 0, (slot as u32) << 24));
            return None;
        }
        let pages = [mem::page(), mem::page(), mem::page(), mem::page()];
        let [Some(input), Some(output), Some(ep0), Some(buffer)] = pages else {
            pages.into_iter().flatten().for_each(mem::give);
            let _ = self.hc.command(Trb::new(trb::DISABLE_SLOT, 0, 0, (slot as u32) << 24));
            println!("[usb] port {}: no memory for another device", port);
            return None;
        };
        self.hc.set_context(slot, output.phys);
        let device = Device {
            slot,
            port,
            route,
            depth,
            speed,
            parent,
            parent_port,
            tt,
            input,
            output,
            ep0: Ring::new(ep0),
            buffer,
            entries: 1,
            hub_ports: 0,
            hub_think: 0,
            listed: Listed { slot, port, route, speed, depth, ..Listed::default() },
            functions: [const { None }; MAX_FUNCTIONS],
        };
        // Endpoint 0 takes eight bytes at a time until the device says how
        // many it takes, which is what it is asked first.
        let mut mps = match speed {
            list::SPEED_LOW | list::SPEED_FULL => 8,
            list::SPEED_HIGH => 64,
            _ => 512,
        };
        let c = self.hc.context;
        device.input.write32(4, 0b11);
        write_slot(&device, c);
        write_endpoint(&device.input, c, 1, EP_CONTROL, mps, 0, device.ep0.dequeue(), 0);
        let address = Trb::new(trb::ADDRESS_DEVICE, device.input.phys, 0, (slot as u32) << 24);
        self.devices[slot as usize] = Some(device);
        if let Err(code) = self.hc.command(address) {
            println!("[usb] port {}: the device would not take an address ({})", port, code);
            self.detach(slot);
            return None;
        }
        // A device is given two milliseconds to take its address.
        syscall::sleep_ms(2);
        if self.control(slot, 0x80, GET_DESCRIPTOR, DEVICE << 8, 0, 8).is_err() {
            println!("[usb] port {}: the device does not say what it is", port);
            self.detach(slot);
            return None;
        }
        let said = self.device(slot)?.buffer.read8(7);
        let takes = if speed >= list::SPEED_SUPER { 1u16 << said.min(9) } else { said as u16 };
        if takes != mps && takes >= 8 {
            mps = takes;
            let d = self.device(slot)?;
            d.input.zero();
            d.input.write32(4, 0b10);
            write_endpoint(&d.input, c, 1, EP_CONTROL, mps, 0, d.ep0.dequeue(), 0);
            let evaluate = Trb::new(trb::EVALUATE_CONTEXT, d.input.phys, 0, (slot as u32) << 24);
            if self.hc.command(evaluate).is_err() {
                self.detach(slot);
                return None;
            }
        }
        if self.control(slot, 0x80, GET_DESCRIPTOR, DEVICE << 8, 0, 18) != Ok(18) {
            self.detach(slot);
            return None;
        }
        let d = self.device(slot)?;
        let class = d.buffer.read8(4);
        d.listed.vendor = d.buffer.read16(8);
        d.listed.product = d.buffer.read16(10);
        d.listed.class = class;
        let product = d.buffer.read8(15);

        // Its first configuration: nine bytes to say how long it all is.
        if self.control(slot, 0x80, GET_DESCRIPTOR, CONFIGURATION << 8, 0, 9).is_err() {
            self.detach(slot);
            return None;
        }
        let total = self.device(slot)?.buffer.read16(2).min(4096);
        let Ok(got) = self.control(slot, 0x80, GET_DESCRIPTOR, CONFIGURATION << 8, 0, total) else {
            self.detach(slot);
            return None;
        };
        let (value, interfaces, count) = parse(&self.device(slot)?.buffer, got);
        let name = if product != 0 { self.string(slot, product) } else { [0; 24] };
        if self.control(slot, 0x00, SET_CONFIGURATION, value as u16, 0, 0).is_err() {
            println!("[usb] port {}: the device would not be configured", port);
            self.detach(slot);
            return None;
        }
        if let Some(d) = self.device(slot) {
            d.listed.name = name;
            if class == 0 && count > 0 {
                d.listed.class = interfaces[0].class;
            }
        }
        for interface in &interfaces[..count] {
            match (interface.class, interface.subclass, interface.protocol) {
                (3, 1, 1) => self.hid(slot, interface, Role::Keyboard),
                (3, 1, 2) => self.hid(slot, interface, Role::Mouse),
                (8, 6, 0x50) => self.disk(slot, interface),
                (9, _, _) => self.hub(slot, interface),
                _ => {}
            }
        }
        let d = self.device(slot)?;
        let listed = d.listed;
        let mut shared = SHARED.lock();
        if let Some(free) = shared.listed.iter_mut().find(|l| l.is_none()) {
            *free = Some(listed);
        }
        drop(shared);
        println!(
            "[usb] port {}{}: {:04x}:{:04x} {}, {}",
            port,
            if depth > 0 { " behind a hub" } else { "" },
            listed.vendor,
            listed.product,
            core::str::from_utf8(listed.name()).unwrap_or("?"),
            listed.speed_name()
        );
        Some(slot)
    }

    /// Let the device in `slot` go, and everything behind it.
    fn detach(&mut self, slot: u8) {
        while let Some(child) = self.devices.iter().flatten().find(|d| d.parent == slot && d.slot != slot).map(|d| d.slot) {
            self.detach(child);
        }
        let Some(device) = self.devices[slot as usize].take() else { return };
        for function in device.functions.into_iter().flatten() {
            match function.role {
                Role::Disk => disk_gone(function.disk),
                Role::Keyboard => self.repeat = None,
                _ => {}
            }
            mem::give(function.ring_in.page);
            mem::give(function.page);
            if let Some(ring) = function.ring_out {
                mem::give(ring.page);
            }
            // A disk's page of sectors is its thread's until the thread has
            // gone, which is after this: it is not given to anything else.
        }
        let _ = self.hc.command(Trb::new(trb::DISABLE_SLOT, 0, 0, (slot as u32) << 24));
        self.hc.set_context(slot, 0);
        for page in [device.input, device.output, device.ep0.page, device.buffer] {
            mem::give(page);
        }
        let mut shared = SHARED.lock();
        for listed in shared.listed.iter_mut() {
            if listed.is_some_and(|l| l.slot == slot) {
                *listed = None;
            }
        }
        drop(shared);
        if device.listed.vendor != 0 {
            println!(
                "[usb] port {}: {:04x}:{:04x} {} has gone",
                device.port,
                device.listed.vendor,
                device.listed.product,
                core::str::from_utf8(device.listed.name()).unwrap_or("?")
            );
        }
    }

    /// A request on endpoint 0 of `slot`, `length` bytes in or out of its
    /// buffer page: how many moved.
    fn control(&mut self, slot: u8, kind: u8, request: u8, value: u16, index: u16, length: u16) -> Result<usize, u8> {
        let Some(device) = self.devices.get_mut(slot as usize).and_then(|d| d.as_mut()) else {
            return Err(0);
        };
        let inward = kind & 0x80 != 0;
        let setup = kind as u64 | (request as u64) << 8 | (value as u64) << 16 | (index as u64) << 32 | (length as u64) << 48;
        let stage = if length == 0 { 0 } else if inward { 3 } else { 2 };
        let mut trbs = [Trb::default(); 3];
        let mut n = 0;
        trbs[n] = Trb::new(trb::SETUP, setup, 8, trb::IMMEDIATE | stage << 16);
        n += 1;
        if length > 0 {
            let direction = if inward { trb::DIRECTION_IN } else { 0 };
            trbs[n] = Trb::new(trb::DATA, device.buffer.phys, length as u32, trb::SHORT_IS_FINE | direction);
            n += 1;
        }
        let status_in = if length == 0 || !inward { trb::DIRECTION_IN } else { 0 };
        trbs[n] = Trb::new(trb::STATUS, 0, 0, trb::ON_COMPLETION | status_in);
        n += 1;
        match self.hc.transfer(slot, 1, &mut device.ep0, &trbs[..n], 5) {
            Ok(residual) => Ok((length as u32).saturating_sub(residual) as usize),
            Err(code) => {
                // A stall is how a device says no to a request; the next
                // setup starts it again, once the controller has been told.
                self.hc.recover(slot, 1, &device.ep0, code == trb::STALL);
                Err(code)
            }
        }
    }

    /// String `index` of `slot`, in its first language, as ASCII.
    fn string(&mut self, slot: u8, index: u8) -> [u8; 24] {
        let mut name = [0u8; 24];
        if self.control(slot, 0x80, GET_DESCRIPTOR, STRING << 8, 0, 4).is_err() {
            return name;
        }
        let language = self.device(slot).map_or(0, |d| d.buffer.read16(2));
        let Ok(got) = self.control(slot, 0x80, GET_DESCRIPTOR, STRING << 8 | index as u16, language, 255) else {
            return name;
        };
        let Some(d) = self.device(slot) else { return name };
        let len = (d.buffer.read8(0) as usize).min(got);
        for (i, at) in (2..len).step_by(2).take(name.len()).enumerate() {
            let c = d.buffer.read16(at);
            name[i] = if (0x20..0x7F).contains(&c) { c as u8 } else { b'?' };
        }
        name
    }

    /// Configure `endpoints` of `slot` — and say it is a hub of `hub`
    /// ports, with that think time, if it is one.
    fn configure(&mut self, slot: u8, endpoints: &[Setup], hub: Option<(u8, u8)>) -> bool {
        let c = self.hc.context;
        let Some(d) = self.devices.get_mut(slot as usize).and_then(|d| d.as_mut()) else { return false };
        if let Some((ports, think)) = hub {
            d.hub_ports = ports;
            d.hub_think = think;
        }
        let mut add = 1u32;
        for e in endpoints {
            add |= 1 << e.dci;
            d.entries = d.entries.max(e.dci);
        }
        d.input.zero();
        d.input.write32(4, add);
        write_slot(d, c);
        for e in endpoints {
            write_endpoint(&d.input, c, e.dci, e.kind, e.mps, e.interval, e.ring, e.burst);
        }
        let command = Trb::new(trb::CONFIGURE_ENDPOINT, d.input.phys, 0, (slot as u32) << 24);
        self.hc.command(command).is_ok()
    }

    // ---- Keyboards and mice --------------------------------------------

    fn hid(&mut self, slot: u8, interface: &Interface, role: Role) {
        let Some(endpoint) = interface.endpoints[..interface.count].iter().find(|e| e.is_interrupt() && e.is_in()).copied() else {
            return;
        };
        let (Some(ring), Some(page)) = (mem::page(), mem::page()) else { return };
        let ring = Ring::new(ring);
        // Reports as a BIOS reads them, and from a keyboard only when they
        // change.
        let number = interface.number as u16;
        let _ = self.control(slot, 0x21, SET_PROTOCOL, 0, number, 0);
        if role == Role::Keyboard {
            let _ = self.control(slot, 0x21, SET_IDLE, 0, number, 0);
        }
        let speed = self.device(slot).map_or(0, |d| d.speed);
        let mps = endpoint.mps & 0x7FF;
        let burst = if speed == list::SPEED_HIGH { ((endpoint.mps >> 11) & 3) as u8 } else { 0 };
        let setup = Setup {
            dci: endpoint.dci(),
            kind: EP_INTERRUPT_IN,
            mps,
            interval: interval(speed, endpoint.interval),
            ring: ring.dequeue(),
            burst,
        };
        if !self.configure(slot, &[setup], None) {
            mem::give(ring.page);
            mem::give(page);
            return;
        }
        let function = Function {
            role,
            interface: interface.number,
            dci_in: endpoint.dci(),
            addr_in: endpoint.address,
            mps_in: mps,
            dci_out: 0,
            addr_out: 0,
            ring_in: ring,
            ring_out: None,
            page,
            data: None,
            disk: 0,
            previous: [0; 8],
            modifiers: Modifiers::default(),
            tag: 0,
        };
        self.add_function(slot, function, if role == Role::Keyboard { list::ROLE_KEYBOARD } else { list::ROLE_MOUSE });
    }

    /// Keep `function` with its device, give its endpoint something to
    /// fill if it reports, and say what the device is.
    fn add_function(&mut self, slot: u8, mut function: Function, role: u8) {
        let Some(d) = self.devices.get_mut(slot as usize).and_then(|d| d.as_mut()) else { return };
        if function.role != Role::Disk {
            arm(&mut self.hc, slot, &mut function);
        }
        d.listed.roles |= role;
        match d.functions.iter_mut().find(|f| f.is_none()) {
            Some(free) => *free = Some(function),
            None => {
                mem::give(function.ring_in.page);
                mem::give(function.page);
            }
        }
    }

    /// Reports the endpoints have filled.
    fn reports(&mut self) {
        let n = self.hc.nunclaimed;
        let events = self.hc.unclaimed;
        self.hc.nunclaimed = 0;
        for event in &events[..n] {
            self.report(*event);
        }
    }

    fn report(&mut self, event: Trb) {
        let (slot, dci) = (event.slot(), event.endpoint());
        let Some(d) = self.devices.get_mut(slot as usize).and_then(|d| d.as_mut()) else { return };
        let ports = d.hub_ports;
        let Some(f) = d.functions.iter_mut().flatten().find(|f| f.dci_in == dci && f.role != Role::Disk) else {
            return;
        };
        if !event.went_well() {
            if self.nstalled < self.stalled.len() {
                self.stalled[self.nstalled] = (slot, dci);
                self.nstalled += 1;
            }
            return;
        }
        let len = ((f.mps_in as u32).saturating_sub(event.residual()) as usize).min(8);
        let mut report = [0u8; 8];
        f.page.copy_out(0, &mut report[..len]);
        match f.role {
            Role::Keyboard => {
                if keyboard(f, &report[..len], &mut self.repeat) {
                    self.tell_input = true;
                }
            }
            Role::Mouse => {
                if mouse(&report[..len]) {
                    self.tell_input = true;
                }
            }
            Role::Hub => {
                for port in 1..=ports.min(63) {
                    let changed = report[port as usize / 8] & (1 << (port % 8)) != 0;
                    if changed && self.nhub_work < self.hub_work.len() {
                        self.hub_work[self.nhub_work] = (slot, port);
                        self.nhub_work += 1;
                    }
                }
            }
            Role::Disk => {}
        }
        arm(&mut self.hc, slot, f);
    }

    /// Interrupt endpoints that stopped on an error, started again.
    fn restart_stalled(&mut self) {
        while self.nstalled > 0 {
            self.nstalled -= 1;
            let (slot, dci) = self.stalled[self.nstalled];
            let Some(d) = self.devices.get_mut(slot as usize).and_then(|d| d.as_mut()) else { continue };
            let Some(f) = d.functions.iter_mut().flatten().find(|f| f.dci_in == dci) else { continue };
            let address = f.addr_in;
            if self.hc.recover(slot, dci, &f.ring_in, true) {
                arm(&mut self.hc, slot, f);
            }
            let _ = self.control(slot, 0x02, CLEAR_FEATURE, ENDPOINT_HALT, address as u16, 0);
        }
    }

    /// The key held down, typed again when it is due.
    fn type_again(&mut self) {
        let Some((key, due)) = self.repeat else { return };
        let now = syscall::sys_clock();
        if now < due {
            return;
        }
        SHARED.lock().keys.push(key);
        self.tell_input = true;
        self.repeat = Some((key, now + REPEAT_EVERY_NS));
    }

    // ---- Hubs ----------------------------------------------------------

    fn hub(&mut self, slot: u8, interface: &Interface) {
        let Some(endpoint) = interface.endpoints[..interface.count].iter().find(|e| e.is_interrupt() && e.is_in()).copied() else {
            return;
        };
        let Some((speed, depth)) = self.device(slot).map(|d| (d.speed, d.depth)) else { return };
        if depth + 1 >= MAX_DEPTH || speed >= list::SPEED_SUPER {
            println!("[usb] a hub too deep, or a USB 3 hub: what is behind it is not driven");
            return;
        }
        if self.control(slot, 0xA0, GET_DESCRIPTOR, HUB << 8, 0, 9).is_err() {
            return;
        }
        let Some(buffer) = self.device(slot).map(|d| d.buffer) else { return };
        let ports = buffer.read8(2).min(31);
        let characteristics = buffer.read16(3);
        let power_good_ms = buffer.read8(5) as u64 * 2;
        let think = if speed == list::SPEED_HIGH { ((characteristics >> 5) & 3) as u8 } else { 0 };
        let (Some(ring), Some(page)) = (mem::page(), mem::page()) else { return };
        let ring = Ring::new(ring);
        let mps = endpoint.mps & 0x7FF;
        let setup = Setup {
            dci: endpoint.dci(),
            kind: EP_INTERRUPT_IN,
            mps,
            interval: interval(speed, endpoint.interval),
            ring: ring.dequeue(),
            burst: 0,
        };
        if !self.configure(slot, &[setup], Some((ports, think))) {
            mem::give(ring.page);
            mem::give(page);
            return;
        }
        let function = Function {
            role: Role::Hub,
            interface: interface.number,
            dci_in: endpoint.dci(),
            addr_in: endpoint.address,
            mps_in: mps,
            dci_out: 0,
            addr_out: 0,
            ring_in: ring,
            ring_out: None,
            page,
            data: None,
            disk: 0,
            previous: [0; 8],
            modifiers: Modifiers::default(),
            tag: 0,
        };
        self.add_function(slot, function, list::ROLE_HUB);
        // Power to every port, and then a look at each.
        for port in 1..=ports {
            let _ = self.control(slot, 0x23, SET_FEATURE, PORT_POWER, port as u16, 0);
        }
        syscall::sleep_ms(power_good_ms.max(20));
        for port in 1..=ports {
            if self.nhub_work < self.hub_work.len() {
                self.hub_work[self.nhub_work] = (slot, port);
                self.nhub_work += 1;
            }
        }
    }

    fn hubs(&mut self) {
        while self.nhub_work > 0 {
            let (slot, port) = self.hub_work[0];
            self.hub_work.copy_within(1..self.nhub_work, 0);
            self.nhub_work -= 1;
            if self.device(slot).is_some() {
                self.hub_port(slot, port);
            }
        }
    }

    /// A hub's port and what changed there: the port's status and change.
    fn hub_status(&mut self, hub: u8, port: u8) -> Option<(u16, u16)> {
        if self.control(hub, 0xA3, GET_STATUS, 0, port as u16, 4) != Ok(4) {
            return None;
        }
        let buffer = self.device(hub)?.buffer;
        Some((buffer.read16(0), buffer.read16(2)))
    }

    fn hub_port(&mut self, hub: u8, port: u8) {
        let Some((status, change)) = self.hub_status(hub, port) else { return };
        for bit in 0..5 {
            if change & (1 << bit) != 0 {
                let _ = self.control(hub, 0x23, CLEAR_FEATURE, C_PORT_CONNECTION + bit, port as u16, 0);
            }
        }
        let here = self.on_hub_port(hub, port);
        if status & 1 == 0 {
            if let Some(slot) = here {
                self.detach(slot);
            }
            return;
        }
        if here.is_some() && change & 1 == 0 {
            return;
        }
        if let Some(slot) = here {
            self.detach(slot);
        }
        syscall::sleep_ms(100);
        if self.control(hub, 0x23, SET_FEATURE, PORT_RESET, port as u16, 0).is_err() {
            return;
        }
        let mut reset = None;
        for _ in 0..50 {
            syscall::sleep_ms(10);
            match self.hub_status(hub, port) {
                Some((status, change)) if change & (1 << 4) != 0 => {
                    let _ = self.control(hub, 0x23, CLEAR_FEATURE, C_PORT_RESET, port as u16, 0);
                    reset = Some(status);
                    break;
                }
                Some(_) => {}
                None => return,
            }
        }
        let Some(status) = reset.filter(|s| s & 2 != 0) else {
            println!("[usb] a hub's port {}: the device would not be enabled", port);
            return;
        };
        syscall::sleep_ms(10);
        let speed = if status & (1 << 9) != 0 {
            list::SPEED_LOW
        } else if status & (1 << 10) != 0 {
            list::SPEED_HIGH
        } else {
            list::SPEED_FULL
        };
        let Some(h) = self.device(hub) else { return };
        let route = h.route | ((port.min(15) as u32) << (4 * h.depth as u32));
        let slow = speed == list::SPEED_LOW || speed == list::SPEED_FULL;
        let tt = if slow && h.speed == list::SPEED_HIGH { (hub, port) } else { h.tt };
        let (root, depth) = (h.port, h.depth + 1);
        self.attach(root, speed, route, depth, hub, port, tt);
    }

    // ---- Disks ---------------------------------------------------------

    fn disk(&mut self, slot: u8, interface: &Interface) {
        let endpoints = &interface.endpoints[..interface.count];
        let (Some(bulk_in), Some(bulk_out)) = (
            endpoints.iter().find(|e| e.is_bulk() && e.is_in()).copied(),
            endpoints.iter().find(|e| e.is_bulk() && !e.is_in()).copied(),
        ) else {
            return;
        };
        let index = {
            let shared = SHARED.lock();
            shared.disks.iter().position(|d| !d.present)
        };
        let Some(index) = index else {
            println!("[usb] a disk past the {} there may be", MAX_DISKS);
            return;
        };
        let pages = [mem::page(), mem::page(), mem::page(), mem::page()];
        let [Some(ring_in), Some(ring_out), Some(page), Some(data)] = pages else {
            pages.into_iter().flatten().for_each(mem::give);
            return;
        };
        let (ring_in, ring_out) = (Ring::new(ring_in), Ring::new(ring_out));
        let setups = [
            Setup { dci: bulk_out.dci(), kind: EP_BULK_OUT, mps: bulk_out.mps & 0x7FF, interval: 0, ring: ring_out.dequeue(), burst: 0 },
            Setup { dci: bulk_in.dci(), kind: EP_BULK_IN, mps: bulk_in.mps & 0x7FF, interval: 0, ring: ring_in.dequeue(), burst: 0 },
        ];
        if !self.configure(slot, &setups, None) {
            for p in [ring_in.page, ring_out.page, page, data] {
                mem::give(p);
            }
            return;
        }
        let function = Function {
            role: Role::Disk,
            interface: interface.number,
            dci_in: bulk_in.dci(),
            addr_in: bulk_in.address,
            mps_in: bulk_in.mps & 0x7FF,
            dci_out: bulk_out.dci(),
            addr_out: bulk_out.address,
            ring_in,
            ring_out: Some(ring_out),
            page,
            data: Some(data),
            disk: index,
            previous: [0; 8],
            modifiers: Modifiers::default(),
            tag: 0,
        };
        self.add_function(slot, function, 0);

        // Asked how many units it has, which some disks stall; then until
        // it is ready, what it is and how big.
        let _ = self.control(slot, 0xA1, GET_MAX_LUN, 0, interface.number as u16, 1);
        let mut ready = false;
        for _ in 0..20 {
            if self.scsi(slot, &[TEST_UNIT_READY, 0, 0, 0, 0, 0], 0, true).is_ok() {
                ready = true;
                break;
            }
            let _ = self.scsi(slot, &[REQUEST_SENSE, 0, 0, 0, 18, 0], 18, true);
            syscall::sleep_ms(100);
        }
        let mut name = [0u8; 24];
        if self.scsi(slot, &[INQUIRY, 0, 0, 0, 36, 0], 36, true).is_ok() {
            // Vendor and product, eight bytes and sixteen, padded with spaces.
            let mut said = [0u8; 24];
            data.copy_out(8, &mut said);
            let (vendor, product) = said.split_at(8);
            let trim = |b: &[u8]| b.iter().rposition(|&c| c != b' ' && c != 0).map_or(0, |p| p + 1);
            let mut n = 0;
            for &b in vendor[..trim(vendor)].iter().chain(b" ").chain(&product[..trim(product)]) {
                if n < name.len() && (0x20..0x7F).contains(&b) {
                    name[n] = b;
                    n += 1;
                }
            }
        }
        let capacity = self.scsi(slot, &[READ_CAPACITY, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8, true).is_ok();
        let last = u32::from_be_bytes([data.read8(0), data.read8(1), data.read8(2), data.read8(3)]);
        let block = u32::from_be_bytes([data.read8(4), data.read8(5), data.read8(6), data.read8(7)]);
        if let Some(d) = self.device(slot) {
            if name[0] != 0 {
                d.listed.name = name;
            }
            d.listed.roles |= list::ROLE_DISK;
        }
        if !ready || !capacity || block != 512 {
            println!(
                "[usb] a disk that {}",
                if !ready || !capacity { "does not say how big it is" } else { "is not of 512-byte blocks, which only are read here" }
            );
            return;
        }
        {
            let mut shared = SHARED.lock();
            let d = &mut shared.disks[index];
            d.present = true;
            d.generation = d.generation.wrapping_add(1);
            d.sectors = last as u64 + 1;
            d.data = data.virt;
            d.slot = slot;
            d.request = None;
        }
        match thread::spawn_with_arg(crate::disk::serve, index, 8) {
            Ok(t) => SHARED.lock().disks[index].thread = t.tid(),
            Err(()) => {
                SHARED.lock().disks[index].present = false;
                println!("[usb] no thread for a disk");
            }
        }
    }

    /// The disk function of `slot`, by its index among its functions.
    fn disk_function(&self, slot: u8) -> Option<usize> {
        let d = self.devices.get(slot as usize)?.as_ref()?;
        d.functions.iter().position(|f| f.as_ref().is_some_and(|f| f.role == Role::Disk))
    }

    /// One SCSI command to the disk in `slot`, `length` bytes moved in
    /// (`inward`) or out at the start of its page of sectors: how much of
    /// it was not moved.
    fn scsi(&mut self, slot: u8, command: &[u8], length: u32, inward: bool) -> Result<u32, ()> {
        let fi = self.disk_function(slot).ok_or(())?;
        let phys = self.devices[slot as usize].as_ref().and_then(|d| d.functions[fi].as_ref()?.data).ok_or(())?.phys;
        self.bot(slot, fi, command, (length > 0).then_some((phys, length, inward)))
    }

    /// A command by bulk-only transport: the command block out, the data
    /// in or out, the status in. A stall on the data is answered and the
    /// status read all the same; anything else wrong is a reset of the
    /// disk's transport.
    fn bot(&mut self, slot: u8, fi: usize, command: &[u8], data: Option<(u64, u32, bool)>) -> Result<u32, ()> {
        let tag;
        let page;
        {
            let d = self.devices[slot as usize].as_mut().ok_or(())?;
            let f = d.functions[fi].as_mut().ok_or(())?;
            f.tag = f.tag.wrapping_add(1);
            tag = f.tag;
            page = f.page;
            page.zero();
            page.write32(0, CBW_SIGNATURE);
            page.write32(4, tag);
            page.write32(8, data.map_or(0, |(_, len, _)| len));
            page.write8(12, if data.is_some_and(|(_, _, inward)| inward) { 0x80 } else { 0 });
            page.write8(13, 0);
            page.write8(14, command.len() as u8);
            for (i, &b) in command.iter().enumerate() {
                page.write8(15 + i, b);
            }
        }
        let cbw = Trb::new(trb::NORMAL, page.phys, 31, trb::ON_COMPLETION);
        if self.bulk(slot, fi, false, cbw, 5).is_err() {
            self.reset_disk(slot, fi);
            return Err(());
        }
        let mut residue = 0;
        if let Some((phys, len, inward)) = data {
            let transfer = Trb::new(trb::NORMAL, phys, len, trb::ON_COMPLETION | trb::SHORT_IS_FINE);
            match self.bulk(slot, fi, inward, transfer, 20) {
                Ok(left) => residue = left,
                Err(trb::STALL) => self.clear_halt(slot, fi, inward),
                Err(_) => {
                    self.reset_disk(slot, fi);
                    return Err(());
                }
            }
        }
        let csw = Trb::new(trb::NORMAL, page.phys + CSW_AT as u64, 13, trb::ON_COMPLETION | trb::SHORT_IS_FINE);
        let mut status = self.bulk(slot, fi, true, csw, 5);
        if status == Err(trb::STALL) {
            self.clear_halt(slot, fi, true);
            status = self.bulk(slot, fi, true, csw, 5);
        }
        if status.is_err() || page.read32(CSW_AT) != CSW_SIGNATURE || page.read32(CSW_AT + 4) != tag {
            self.reset_disk(slot, fi);
            return Err(());
        }
        match page.read8(CSW_AT + 12) {
            0 => Ok(page.read32(CSW_AT + 8).max(residue)),
            1 => Err(()),
            _ => {
                self.reset_disk(slot, fi);
                Err(())
            }
        }
    }

    /// One transfer on a disk's bulk endpoint in or out.
    fn bulk(&mut self, slot: u8, fi: usize, inward: bool, transfer: Trb, seconds: u64) -> Result<u32, u8> {
        let d = self.devices[slot as usize].as_mut().ok_or(0u8)?;
        let f = d.functions[fi].as_mut().ok_or(0u8)?;
        let (dci, ring) = if inward { (f.dci_in, &mut f.ring_in) } else { (f.dci_out, f.ring_out.as_mut().ok_or(0u8)?) };
        let result = self.hc.transfer(slot, dci, ring, &[transfer], seconds);
        if let Err(code) = result {
            if code != trb::STALL {
                // Given up on: the controller is told to go on from here.
                self.hc.recover(slot, dci, ring, false);
            }
        }
        result
    }

    /// A disk's endpoint stalled: started again, at the controller and at
    /// the disk.
    fn clear_halt(&mut self, slot: u8, fi: usize, inward: bool) {
        let address = {
            let Some(d) = self.devices[slot as usize].as_mut() else { return };
            let Some(f) = d.functions[fi].as_mut() else { return };
            let (dci, ring, address) =
                if inward { (f.dci_in, &f.ring_in, f.addr_in) } else { (f.dci_out, f.ring_out.as_ref().unwrap_or(&f.ring_in), f.addr_out) };
            self.hc.recover(slot, dci, ring, true);
            address
        };
        let _ = self.control(slot, 0x02, CLEAR_FEATURE, ENDPOINT_HALT, address as u16, 0);
    }

    /// Bulk-only reset recovery (5.3.4): the transport reset, and both
    /// endpoints started again.
    fn reset_disk(&mut self, slot: u8, fi: usize) {
        let Some(interface) = self.devices[slot as usize].as_ref().and_then(|d| d.functions[fi].as_ref()).map(|f| f.interface) else {
            return;
        };
        let _ = self.control(slot, 0x21, MASS_STORAGE_RESET, 0, interface as u16, 0);
        self.clear_halt(slot, fi, true);
        self.clear_halt(slot, fi, false);
    }

    /// Disks' threads' requests, done.
    fn disks(&mut self) {
        for index in 0..MAX_DISKS {
            let job = {
                let mut shared = SHARED.lock();
                let d = &mut shared.disks[index];
                if d.present { d.request.take().map(|r| (r, d.slot)) } else { None }
            };
            let Some((request, slot)) = job else { continue };
            let ok = self.disk_io(slot, request);
            SHARED.lock().disks[index].ok = ok;
            DONE[index].release();
        }
    }

    fn disk_io(&mut self, slot: u8, r: Request) -> bool {
        // READ(10) and WRITE(10) say a block's number in thirty-two bits.
        if r.lba + r.count as u64 > 1 << 32 {
            return false;
        }
        let lba = (r.lba as u32).to_be_bytes();
        let count = (r.count as u16).to_be_bytes();
        let op = if r.write { WRITE_10 } else { READ_10 };
        let command = [op, 0, lba[0], lba[1], lba[2], lba[3], 0, count[0], count[1], 0];
        let Some(fi) = self.disk_function(slot) else { return false };
        let Some(data) = self.devices[slot as usize].as_ref().and_then(|d| d.functions[fi].as_ref()?.data) else {
            return false;
        };
        let transfer = (data.phys + r.offset as u64, r.count * 512, !r.write);
        self.bot(slot, fi, &command, Some(transfer)) == Ok(0)
    }
}

/// Give `f`'s endpoint in a buffer to fill with its next report.
fn arm(hc: &mut Hc, slot: u8, f: &mut Function) {
    f.ring_in.push(Trb::new(trb::NORMAL, f.page.phys, f.mps_in as u32, trb::ON_COMPLETION | trb::SHORT_IS_FINE));
    hc.ring(slot, f.dci_in);
}

/// A keyboard's report, as keys pressed and let go since its last: whether
/// there were any.
fn keyboard(f: &mut Function, report: &[u8], repeat: &mut Option<(Key, u64)>) -> bool {
    // Too short, or the keyboard saying more keys are down than it can say.
    if report.len() < 3 || report[2] == 1 {
        return false;
    }
    let mut now = [0u8; 8];
    now[..report.len()].copy_from_slice(report);
    let was = f.previous;
    let mut any = false;
    for (bit, &code) in keys::FROM_MODIFIER_BIT.iter().enumerate() {
        let mask = 1 << bit;
        if (now[0] ^ was[0]) & mask != 0 {
            any |= key(f, code, now[0] & mask != 0, repeat);
        }
    }
    for &usage in &was[2..] {
        if usage > 3 && !now[2..].contains(&usage) {
            any |= key(f, keys::from_usage(usage), false, repeat);
        }
    }
    for &usage in &now[2..] {
        if usage > 3 && !was[2..].contains(&usage) {
            any |= key(f, keys::from_usage(usage), true, repeat);
        }
    }
    f.previous = now;
    any
}

fn key(f: &mut Function, code: u8, press: bool, repeat: &mut Option<(Key, u64)>) -> bool {
    if code == 0 {
        return false;
    }
    let modifiers = f.modifiers.key(code, press);
    let k = Key { press, ascii: keys::ascii(code, modifiers), code, modifiers };
    SHARED.lock().keys.push(k);
    let modifier = keys::FROM_MODIFIER_BIT.contains(&code) || code == keys::CAPS_LOCK;
    if press && !modifier {
        *repeat = Some((k, syscall::sys_clock() + REPEAT_AFTER_NS));
    } else if !press && repeat.is_some_and(|(held, _)| held.code == code) {
        *repeat = None;
    }
    true
}

/// A boot mouse's report: buttons, how far across and down, and its wheel,
/// which turns the other way to the i8042's. Whether there was one.
fn mouse(report: &[u8]) -> bool {
    if report.len() < 3 {
        return false;
    }
    let m = Movement {
        dx: report[1] as i8 as i32,
        dy: report[2] as i8 as i32,
        buttons: report[0] & 7,
        wheel: if report.len() >= 4 { -(report[3] as i8 as i32) } else { 0 },
    };
    let mut shared = SHARED.lock();
    // A movement with the buttons as they were and no wheel joins the last
    // one not yet taken: a mouse says far more than anything asks for.
    if let Some(last) = shared.movements.last_mut() {
        if last.buttons == m.buttons && last.wheel == 0 && m.wheel == 0 {
            last.dx += m.dx;
            last.dy += m.dy;
            return true;
        }
    }
    shared.movements.push(m);
    true
}

/// A disk pulled out: what its thread waits on is failed, and the thread
/// woken, to end (`block::Device::gone`).
fn disk_gone(index: usize) {
    let (thread, waiting) = {
        let mut shared = SHARED.lock();
        let d = &mut shared.disks[index];
        d.present = false;
        let waiting = d.request.take().is_some();
        if waiting {
            d.ok = false;
        }
        let thread = d.thread;
        d.thread = 0;
        (thread, waiting)
    };
    if waiting {
        DONE[index].release();
    }
    if thread != 0 {
        let _ = syscall::sys_cap_delete(WAKE_SLOT);
        if syscall::sys_cap_mint(WAKE_SLOT, syscall::CAP_TYPE_ENDPOINT, thread as u64, 0).is_ok() {
            let _ = syscall::sys_notify(thread, 1);
        }
        let _ = syscall::sys_cap_delete(WAKE_SLOT);
    }
}

/// How often an interrupt endpoint is asked, as the controller counts it:
/// two to the power of this, in eighths of a millisecond (6.2.3.6).
fn interval(speed: u8, said: u8) -> u8 {
    match speed {
        list::SPEED_LOW | list::SPEED_FULL => {
            let eighths = said.max(1) as u32 * 8;
            (31 - eighths.leading_zeros()).clamp(3, 10) as u8
        }
        _ => said.clamp(1, 16) - 1,
    }
}

/// The slot's context, in the device's input context.
fn write_slot(d: &Device, c: usize) {
    let hub = d.hub_ports != 0;
    d.input.write32(c, d.route & 0xF_FFFF | (d.speed as u32) << 20 | (hub as u32) << 26 | (d.entries as u32) << 27);
    d.input.write32(c + 4, (d.port as u32) << 16 | (d.hub_ports as u32) << 24);
    d.input.write32(c + 8, d.tt.0 as u32 | (d.tt.1 as u32) << 8 | (d.hub_think as u32 & 3) << 16);
    d.input.write32(c + 12, 0);
}

/// Endpoint `dci`'s context, in an input context.
#[allow(clippy::too_many_arguments)]
fn write_endpoint(input: &Page, c: usize, dci: u8, kind: u32, mps: u16, interval: u8, ring: u64, burst: u8) {
    let at = c * (1 + dci as usize);
    let esit = mps as u32 * (burst as u32 + 1);
    let periodic = kind == EP_INTERRUPT_IN;
    input.write32(at, (interval as u32) << 16 | if periodic { (esit >> 16) << 24 } else { 0 });
    input.write32(at + 4, 3 << 1 | kind << 3 | (burst as u32) << 8 | (mps as u32) << 16);
    input.write64(at + 8, ring);
    let average = match kind {
        EP_CONTROL => 8,
        EP_INTERRUPT_IN => mps as u32,
        _ => 3072,
    };
    input.write32(at + 16, average | if periodic { (esit & 0xFFFF) << 16 } else { 0 });
}

/// A configuration, `len` bytes of it in `page`: its value, and its
/// interfaces' first settings, as many as are kept.
fn parse(page: &Page, len: usize) -> (u8, [Interface; 4], usize) {
    let mut interfaces = [Interface::default(); 4];
    let mut count = 0;
    let mut current: Option<usize> = None;
    let value = page.read8(5);
    let mut at = 0;
    while at + 2 <= len {
        let size = page.read8(at) as usize;
        if size < 2 || at + size > len {
            break;
        }
        match page.read8(at + 1) {
            INTERFACE if size >= 9 => {
                current = None;
                if page.read8(at + 3) == 0 && count < interfaces.len() {
                    interfaces[count] = Interface {
                        number: page.read8(at + 2),
                        class: page.read8(at + 5),
                        subclass: page.read8(at + 6),
                        protocol: page.read8(at + 7),
                        ..Interface::default()
                    };
                    current = Some(count);
                    count += 1;
                }
            }
            ENDPOINT if size >= 7 => {
                if let Some(i) = current {
                    let interface = &mut interfaces[i];
                    if interface.count < interface.endpoints.len() {
                        interface.endpoints[interface.count] = Endpoint {
                            address: page.read8(at + 2),
                            attributes: page.read8(at + 3),
                            mps: page.read16(at + 4),
                            interval: page.read8(at + 6),
                        };
                        interface.count += 1;
                    }
                }
            }
            _ => {}
        }
        at += size;
    }
    (value, interfaces, count)
}
