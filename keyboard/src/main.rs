#![no_std]
#![no_main]

use quark_rt::ipc::{death_notice, Message, TID_ANY};
use quark_rt::keys::{self, Modifiers};
use quark_rt::nameserver;
use quark_rt::{println, syscall};

// PS/2 controller data and status ports, and the keyboard interrupt line.
quark_rt::manifest!([
    quark_rt::manifest::CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
    quark_rt::manifest::CapReq::ioport(0x60, 0x64),
    quark_rt::manifest::CapReq::irq(1),
    quark_rt::manifest::CapReq::irq(12),
]);

// Keyboard IPC tags
const TAG_GET_KEY: u64 = 1;
const TAG_KEY_EVENT: u64 = 2;
const TAG_NO_KEY: u64 = 3;
/// Take the keyboard, with the right to notify the claimant on offer. From
/// then on nobody else is answered, until the claimant dies: keys are for the
/// input server to hand out, and a program reading them here would be reading
/// whatever anybody types.
const TAG_KBD_CLAIM: u64 = 4;
/// What anybody but the claimant is answered with, in `data[0]` of an error.
const ERR_NOT_CLAIMANT: u64 = 5;
const TAG_ERROR: u64 = u64::MAX;
/// What the claimant is notified of: a key arrived, and it was Ctrl-C.
const NOTIFY_KEY: u64 = 2;
const NOTIFY_CTRL_C: u64 = 1;
/// Take a key if one is waiting, but do not wait for one.
///
/// [`TAG_GET_KEY`] parks the caller until something is typed, which is right
/// for a line discipline and wrong for anything with a screen to redraw. A
/// server that has to stay answerable — the input server while a compositor
/// holds the keyboard — asks this instead and gets [`TAG_NO_KEY`] when there
/// is nothing.
///
/// A keyboard driver that predates this answers `TAG_NO_KEY` through its
/// default arm, so the failure is "no keys ever" rather than a caller that
/// hangs on an unrecognised tag.
const TAG_GET_KEY_NB: u64 = 5;
/// Take a pointer movement if one is waiting, but do not wait for one.
///
/// `data[0] = dx`, `data[1] = dy` as signed values widened to u64,
/// `data[2] = buttons`, bit 0 left, bit 1 right, bit 2 middle, and
/// `data[3] = wheel`, detents since the last packet, positive towards the
/// user. Answered with [`TAG_NO_KEY`] when there is nothing, so a caller that
/// asks a driver predating the mouse gets "no movement ever" rather than
/// hanging — and one that asks a mouse without a wheel gets a zero there
/// forever, which is the same thing said about the wheel.
const TAG_GET_MOUSE_NB: u64 = 6;
const TAG_MOUSE_EVENT: u64 = 7;

// Key event types
const KEY_PRESS: u64 = 1;
const KEY_RELEASE: u64 = 2;

// Ring buffer for key events
const KEY_BUF_SIZE: usize = 64;

struct KeyEvent {
    press: bool,
    ascii: u8,
    scancode: u8,
    modifiers: u8,
}

/// The i8042's status port bits this driver acts on.
///
/// Bit 0 says a byte is waiting to be read. Bit 1 says the controller has not
/// yet taken the last byte written to it. Bit 5 says the waiting byte came from
/// the *auxiliary* device — the mouse — rather than the keyboard.
///
/// That last bit is why there is one driver here and not two. A PS/2 mouse is
/// not a second device with a second port: it is the same controller answering
/// on the same data port, and two tasks reading 0x60 would take each other's
/// bytes. Which interrupt fired is not the answer either, because a byte for
/// one device can be waiting when the other's interrupt arrives.
const STATUS_OUTPUT_FULL: u64 = 1 << 0;
const STATUS_INPUT_FULL: u64 = 1 << 1;
const STATUS_FROM_MOUSE: u64 = 1 << 5;

const PORT_DATA: u16 = 0x60;
const PORT_STATUS: u16 = 0x64;
const PORT_CMD: u16 = 0x64;

/// Controller commands.
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
/// "The next byte written to the data port is for the mouse, not the keyboard."
const CMD_TO_MOUSE: u8 = 0xD4;

/// Mouse commands, and the byte it answers them with.
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_REPORTING: u8 = 0xF4;
const MOUSE_SET_SAMPLE_RATE: u8 = 0xF3;
const MOUSE_GET_DEVICE_ID: u8 = 0xF2;
const MOUSE_ACK: u8 = 0xFA;

/// What the device says it is, once asked.
///
/// A plain PS/2 mouse is 0 and sends three bytes. A mouse that has been shown
/// the knock below answers 3 (IMPS/2, a wheel) or 4 (IMEX, a wheel and two
/// more buttons), and sends four.
const MOUSE_ID_WHEEL: u8 = 3;
const MOUSE_ID_IMEX: u8 = 4;

/// Configuration byte bits: bit 1 lets the auxiliary device raise IRQ 12, and
/// bit 5 *disables* its clock, so it has to be cleared.
const CONFIG_AUX_IRQ: u8 = 1 << 1;
const CONFIG_AUX_CLOCK_OFF: u8 = 1 << 5;

/// One movement, as the compositor will want it.
///
/// `wheel` is detents since the previous packet, positive towards the user —
/// which is the sign the hardware reports and the sign `wl_pointer.axis`
/// wants, so nothing between here and a client has to flip it.
#[derive(Clone, Copy)]
struct MouseEvent {
    dx: i32,
    dy: i32,
    buttons: u8,
    wheel: i32,
}

const MOUSE_BUF_SIZE: usize = 32;

/// Movements waiting to be collected.
///
/// Coalescing would be wrong here even though it is tempting: a click at the
/// end of a fast movement must not arrive at a position the pointer had already
/// left, and only the consumer knows which movements it can afford to merge.
struct MouseBuffer {
    buf: [MouseEvent; MOUSE_BUF_SIZE],
    head: usize,
    tail: usize,
}

impl MouseBuffer {
    const fn new() -> Self {
        const EMPTY: MouseEvent = MouseEvent { dx: 0, dy: 0, buttons: 0, wheel: 0 };
        MouseBuffer { buf: [EMPTY; MOUSE_BUF_SIZE], head: 0, tail: 0 }
    }

    fn push(&mut self, ev: MouseEvent) {
        let next = (self.head + 1) % MOUSE_BUF_SIZE;
        if next != self.tail {
            self.buf[self.head] = ev;
            self.head = next;
        }
    }

    fn pop(&mut self) -> Option<MouseEvent> {
        if self.head == self.tail {
            return None;
        }
        let ev = self.buf[self.tail];
        self.tail = (self.tail + 1) % MOUSE_BUF_SIZE;
        Some(ev)
    }
}

/// Assembles the controller's packets, three bytes long or four.
///
/// Bit 3 of the first byte is always set, which is the only synchronisation
/// signal there is: a stream that has lost its place is found by a first byte
/// without it, and skipping that byte is how it is recovered.
///
/// The length is the device's own answer to [`MOUSE_GET_DEVICE_ID`] and not a
/// guess: reading four bytes from a mouse sending three takes the next
/// packet's first byte as this one's wheel and loses synchronisation for good.
struct MouseDecoder {
    bytes: [u8; 4],
    have: usize,
    id: u8,
}

impl MouseDecoder {
    const fn new(id: u8) -> Self {
        MouseDecoder { bytes: [0; 4], have: 0, id }
    }

    const fn packet_len(&self) -> usize {
        if self.id >= MOUSE_ID_WHEEL { 4 } else { 3 }
    }

    fn feed(&mut self, b: u8) -> Option<MouseEvent> {
        if self.have == 0 && b & 0x08 == 0 {
            return None; // out of step; this cannot be a first byte
        }
        self.bytes[self.have] = b;
        self.have += 1;
        if self.have < self.packet_len() {
            return None;
        }
        self.have = 0;
        let flags = self.bytes[0];
        // Overflow means the controller gave up counting, and the magnitude it
        // reports is meaningless. Dropping the movement is better than jumping
        // the pointer across the screen; the buttons in it are still current
        // and are reported with no movement.
        let (dx, dy) = if flags & 0xC0 != 0 {
            (0, 0)
        } else {
            (sign_extend(self.bytes[1], flags & 0x10 != 0),
             sign_extend(self.bytes[2], flags & 0x20 != 0))
        };
        Some(MouseEvent {
            dx,
            // The mouse counts upwards and the screen counts downwards.
            dy: -dy,
            buttons: flags & 0x07,
            wheel: self.wheel(),
        })
    }

    /// The fourth byte, which two devices spell differently.
    ///
    /// IMPS/2 puts a signed byte there. IMEX keeps the wheel in the low four
    /// bits and puts the fourth and fifth buttons above it, which this system
    /// has nowhere to send, so they are dropped rather than read as a wheel
    /// spun eight detents at once.
    fn wheel(&self) -> i32 {
        match self.id {
            MOUSE_ID_WHEEL => self.bytes[3] as i8 as i32,
            MOUSE_ID_IMEX => {
                let v = (self.bytes[3] & 0x0F) as i32;
                if v & 0x08 != 0 { v - 16 } else { v }
            }
            _ => 0,
        }
    }
}

/// A nine-bit signed quantity, split across a byte and a sign bit in the flags.
fn sign_extend(value: u8, negative: bool) -> i32 {
    if negative { value as i32 - 256 } else { value as i32 }
}

/// Wait for the controller's status to say `ready`: a few hundred looks,
/// which is all a controller that is there takes, and then a millisecond
/// between looks, a tenth of a second at most.
///
/// Bounded: a controller that never clears the bit must not become a driver
/// that never returns, and on a machine with no mouse that is exactly what
/// would happen during start-up. And mostly asleep: this is in the drivers'
/// band, and every look it spins on is a look nothing below it gets to run
/// in — a hundred thousand of them a wait, on a machine whose controller
/// was not there, kept the file server from starting.
fn wait_status(ready: impl Fn(u64) -> bool) -> bool {
    for look in 0..500 {
        if ready(syscall::sys_ioport_read(PORT_STATUS)) {
            return true;
        }
        if look >= 400 {
            syscall::sleep_ms(1);
        }
    }
    false
}

/// Wait for the controller to take what was last written to it.
fn wait_writable() -> bool {
    wait_status(|status| status & STATUS_INPUT_FULL == 0)
}

fn wait_readable() -> bool {
    wait_status(|status| status & STATUS_OUTPUT_FULL != 0)
}

fn command(byte: u8) {
    if wait_writable() {
        syscall::sys_ioport_write(PORT_CMD, byte);
    }
}

/// Send a byte to the mouse and collect its acknowledgement.
fn mouse_command(byte: u8) -> bool {
    command(CMD_TO_MOUSE);
    if !wait_writable() {
        return false;
    }
    syscall::sys_ioport_write(PORT_DATA, byte);
    if !wait_readable() {
        return false;
    }
    syscall::sys_ioport_read(PORT_DATA) as u8 == MOUSE_ACK
}

/// Ask the mouse for a wheel, and find out whether it has one.
///
/// The knock is the whole of it: three sample rates, 200, 100 and 80, which no
/// program would set on purpose, and a device that recognises the sequence
/// starts calling itself 3 and sending a fourth byte. A device that does not
/// keeps answering 0, which is not a failure — it is a mouse without a wheel.
fn ask_for_wheel() -> u8 {
    for rate in [200u8, 100, 80] {
        if !mouse_command(MOUSE_SET_SAMPLE_RATE) || !mouse_command(rate) {
            return 0;
        }
    }
    if !mouse_command(MOUSE_GET_DEVICE_ID) || !wait_readable() {
        return 0;
    }
    syscall::sys_ioport_read(PORT_DATA) as u8
}

/// Turn the auxiliary device on and ask it to report.
///
/// Returns the device id, or `None` when there is no mouse. A machine without
/// one is not an error — it is most machines this has ever run on — so the
/// failure is quiet and the driver carries on being a keyboard.
fn enable_mouse() -> Option<u8> {
    command(CMD_ENABLE_AUX);

    command(CMD_READ_CONFIG);
    if !wait_readable() {
        return None;
    }
    let mut config = syscall::sys_ioport_read(PORT_DATA) as u8;
    config |= CONFIG_AUX_IRQ;
    config &= !CONFIG_AUX_CLOCK_OFF;
    command(CMD_WRITE_CONFIG);
    if !wait_writable() {
        return None;
    }
    syscall::sys_ioport_write(PORT_DATA, config);

    // Defaults first, so that whatever the firmware left behind — a different
    // sample rate, a different resolution — is not inherited. The knock comes
    // after it for the same reason, and reporting last: a device that is
    // already sending packets would answer the knock in the middle of one.
    if !mouse_command(MOUSE_SET_DEFAULTS) {
        return None;
    }
    let id = ask_for_wheel();
    if !mouse_command(MOUSE_ENABLE_REPORTING) {
        return None;
    }
    Some(id)
}

struct KeyBuffer {
    buf: [KeyEvent; KEY_BUF_SIZE],
    head: usize,
    tail: usize,
}

impl KeyBuffer {
    const fn new() -> Self {
        const EMPTY: KeyEvent = KeyEvent {
            press: false,
            ascii: 0,
            scancode: 0,
            modifiers: 0,
        };
        KeyBuffer {
            buf: [EMPTY; KEY_BUF_SIZE],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, ev: KeyEvent) {
        let next = (self.head + 1) % KEY_BUF_SIZE;
        if next != self.tail {
            self.buf[self.head] = ev;
            self.head = next;
        }
        // Drop if full
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.head == self.tail {
            return None;
        }
        let ev = KeyEvent {
            press: self.buf[self.tail].press,
            ascii: self.buf[self.tail].ascii,
            scancode: self.buf[self.tail].scancode,
            modifiers: self.buf[self.tail].modifiers,
        };
        self.tail = (self.tail + 1) % KEY_BUF_SIZE;
        Some(ev)
    }
}


#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("[keyboard] Started.");

    // Nothing at the ports reads all ones, which no controller says of
    // itself — every bit, the two that are errors among them. A machine with
    // no i8042 has its keys from USB, and nothing here to drive.
    if syscall::sys_ioport_read(PORT_STATUS) & 0xFF == 0xFF {
        println!("[keyboard] No i8042 here; keys come from USB.");
        syscall::sys_exit_code(0);
    }

    // Register for IRQ 1 (keyboard)
    if syscall::sys_irq_register(1).is_err() {
        println!("[keyboard] Failed to register IRQ 1!");
        syscall::sys_exit();
    }
    // And IRQ 12, the same controller's other device. Registered before the
    // mouse is enabled, so that the first packet it sends has somewhere to go.
    let mouse_irq = syscall::sys_irq_register(12).is_ok();
    if !mouse_irq {
        println!("[keyboard] No IRQ 12; the mouse will not be heard.");
    }

    // Register with nameserver as "keyboard"
    if nameserver::register(b"keyboard").is_ok() {
        println!("[keyboard] Registered with nameserver.");
    } else {
        println!("[keyboard] Failed to register with nameserver.");
    }

    let mut keybuf = KeyBuffer::new();
    let mut modifiers = Modifiers::default();
    let mut extended = false;
    let mut waiting_client: Option<usize> = None;
    // Who holds the keyboard, and where the capability to notify it is.
    let mut claimant: usize = 0;
    let mut claimant_slot: usize = 0;

    let mut mousebuf = MouseBuffer::new();
    let mouse_id = if mouse_irq { enable_mouse() } else { None };
    let have_mouse = mouse_id.is_some();
    let mut mouse = MouseDecoder::new(mouse_id.unwrap_or(0));
    match mouse_id {
        Some(id) if id >= MOUSE_ID_WHEEL => {
            println!("[keyboard] Mouse enabled on the auxiliary port, with a wheel (id {}).", id)
        }
        Some(_) => println!("[keyboard] Mouse enabled on the auxiliary port; no wheel."),
        None => println!("[keyboard] No mouse; keys only."),
    }

    loop {
        let mut msg = Message::empty();
        if syscall::sys_recv(TID_ANY, &mut msg).is_err() {
            continue;
        }

        if let Some(dead) = death_notice(&msg) {
            if dead == claimant {
                println!("[keyboard] claimant tid {} has gone", dead);
                let _ = syscall::sys_cap_delete(claimant_slot);
                claimant = 0;
                claimant_slot = 0;
                waiting_client = None;
            }
            continue;
        }

        if msg.sender == 0 {
            // Take everything the controller has, not one byte per
            // notification. The kernel's per-IRQ queue is eight deep and drops
            // silently when it is full, so a burst of typing on a busy machine
            // loses notifications — and with one byte each, that lost every
            // keystroke behind them. Draining means a lost notification costs
            // nothing: the next wake-up collects what the last one left.
            //
            // Bit 0 of the status port says there is a byte waiting, and it is
            // asked before the first read as well as before each one after it.
            // The drain takes everything the controller has, so when two
            // notifications are queued the first one's loop consumes both
            // bytes and the second arrives to an empty buffer — and reading
            // 0x60 when it is empty hands back the last byte again. That is
            // one duplicated key per burst of typing: invisible in a shell
            // that echoes, and obvious to a Wayland client counting presses
            // against releases.
            //
            // Which device a byte came from is read from the status, not from
            // which interrupt fired: a keyboard byte can be waiting when IRQ 12
            // arrives, and feeding one to the packet decoder loses the mouse's
            // place in its three-byte stream.
            let mut budget = 64;
            loop {
                let status = syscall::sys_ioport_read(PORT_STATUS);
                if status & STATUS_OUTPUT_FULL == 0 {
                    break;
                }
                let raw = syscall::sys_ioport_read(PORT_DATA) as u8;
                if status & STATUS_FROM_MOUSE != 0 {
                    if let Some(ev) = mouse.feed(raw) {
                        mousebuf.push(ev);
                    }
                } else {
                    handle_scancode(
                        raw,
                        &mut extended,
                        &mut modifiers,
                        &mut keybuf,
                        claimant,
                        &mut waiting_client,
                    );
                }
                budget -= 1;
                // The bound is in case a controller lies about bit 0, so a
                // wedged keyboard cannot become a wedged system.
                if budget == 0 {
                    break;
                }
            }
            // Both lines, because one notification can carry bytes for either
            // device and an unacknowledged line stops delivering for good.
            syscall::sys_irq_ack(1);
            if have_mouse {
                syscall::sys_irq_ack(12);
            }
        } else if msg.tag == quark_rt::ipc::TAG_PING {
            // Whether it is alive is anybody's business.
            let reply = Message { sender: 0, tag: quark_rt::ipc::TAG_PING, data: [0; 6] };
            let _ = syscall::sys_reply(msg.sender, &reply);
        } else if msg.tag == TAG_KBD_CLAIM {
            // Only with the right to tell it things: the claim offers one,
            // and without it there is nobody this could notify.
            let taken = if claimant == 0 || claimant == msg.sender {
                syscall::sys_cap_take_any(msg.sender).ok()
            } else {
                None
            };
            let reply = match taken {
                Some(slot) => {
                    if claimant_slot != 0 {
                        let _ = syscall::sys_cap_delete(claimant_slot);
                    }
                    claimant = msg.sender;
                    claimant_slot = slot;
                    let _ = syscall::sys_task_watch(claimant);
                    println!("[keyboard] claimed by tid {}", claimant);
                    Message { sender: 0, tag: 0, data: [0; 6] }
                }
                None => Message { sender: 0, tag: TAG_ERROR, data: [ERR_NOT_CLAIMANT, 0, 0, 0, 0, 0] },
            };
            let _ = syscall::sys_reply(msg.sender, &reply);
        } else if msg.sender != claimant {
            let reply = Message { sender: 0, tag: TAG_ERROR, data: [ERR_NOT_CLAIMANT, 0, 0, 0, 0, 0] };
            let _ = syscall::sys_reply(msg.sender, &reply);
        } else {
            // The claimant's requests. One that asks anything has stopped
            // waiting for a key it asked for before: a task is in one call
            // at a time.
            if waiting_client == Some(msg.sender) {
                waiting_client = None;
            }
            match msg.tag {
                TAG_GET_KEY => {
                    if let Some(ev) = keybuf.pop() {
                        let reply = make_key_reply(&ev);
                        let _ = syscall::sys_reply(msg.sender, &reply);
                    } else {
                        // No key available — save client to reply later
                        waiting_client = Some(msg.sender);
                    }
                }
                TAG_GET_KEY_NB => {
                    let reply = match keybuf.pop() {
                        Some(ev) => make_key_reply(&ev),
                        None => Message { sender: 0, tag: TAG_NO_KEY, data: [0; 6] },
                    };
                    let _ = syscall::sys_reply(msg.sender, &reply);
                }
                TAG_GET_MOUSE_NB => {
                    let reply = match mousebuf.pop() {
                        Some(ev) => Message {
                            sender: 0,
                            tag: TAG_MOUSE_EVENT,
                            data: [
                                ev.dx as i64 as u64,
                                ev.dy as i64 as u64,
                                ev.buttons as u64,
                                ev.wheel as i64 as u64,
                                0,
                                0,
                            ],
                        },
                        None => Message { sender: 0, tag: TAG_NO_KEY, data: [0; 6] },
                    };
                    let _ = syscall::sys_reply(msg.sender, &reply);
                }
                _ => {
                    let reply = Message {
                        sender: 0,
                        tag: TAG_NO_KEY,
                        data: [0; 6],
                    };
                    let _ = syscall::sys_reply(msg.sender, &reply);
                }
            }
        }
    }
}

/// Translate one scancode and deliver or buffer the event it makes.
fn handle_scancode(
    raw: u8,
    extended: &mut bool,
    modifiers: &mut Modifiers,
    keybuf: &mut KeyBuffer,
    claimant: usize,
    waiting_client: &mut Option<usize>,
) {
    if raw == 0xE0 {
        *extended = true;
        return;
    }
    let press = raw & 0x80 == 0;
    let code = if *extended {
        *extended = false;
        // The keys set 1 says with a prefix: the arrows and the block above
        // them, and the right-hand Ctrl and Alt. They were dropped, so no
        // program ever saw an arrow key.
        //
        // Each is reported under its Linux evdev code. For an unprefixed key
        // the scancode already *is* that code, which is what a compositor
        // sends its clients; these are the ones where the two differ, and
        // their codes are above every unprefixed one, so nothing collides.
        match raw & 0x7F {
            0x1C => 96,  // keypad Enter
            0x1D => 97,  // right Ctrl
            0x35 => 98,  // keypad /
            0x38 => 100, // right Alt
            0x47 => 102, // Home
            0x48 => 103, // Up
            0x49 => 104, // Page Up
            0x4B => 105, // Left
            0x4D => 106, // Right
            0x4F => 107, // End
            0x50 => 108, // Down
            0x51 => 109, // Page Down
            0x52 => 110, // Insert
            0x53 => 111, // Delete
            // A fake shift the controller sends around some of the above,
            // and keys this has no name for.
            _ => return,
        }
    } else {
        raw & 0x7F
    };
    let modifiers = modifiers.key(code, press);
    let ascii = keys::ascii(code, modifiers);
    deliver(KeyEvent { press, ascii, scancode: code, modifiers }, keybuf, claimant, waiting_client);
}

/// Hand a key to whoever is waiting for one, or keep it until somebody asks.
fn deliver(
    ev: KeyEvent,
    keybuf: &mut KeyBuffer,
    claimant: usize,
    waiting_client: &mut Option<usize>,
) {
    // If a client is blocked waiting, reply immediately
    if ev.press {
        if let Some(client_tid) = waiting_client.take() {
            let reply = make_key_reply(&ev);
            let _ = syscall::sys_reply(client_tid, &reply);
            return;
        }
    }

    let ctrl_c = ev.press && ev.ascii == 0x03;
    keybuf.push(ev);

    // Told after the key is there to take: the claimant wakes and asks.
    if claimant != 0 {
        let _ = syscall::sys_notify(claimant, NOTIFY_KEY | if ctrl_c { NOTIFY_CTRL_C } else { 0 });
    }
}

fn make_key_reply(ev: &KeyEvent) -> Message {
    Message {
        sender: 0,
        tag: TAG_KEY_EVENT,
        data: [
            if ev.press { KEY_PRESS } else { KEY_RELEASE },
            ev.ascii as u64,
            ev.scancode as u64,
            ev.modifiers as u64,
            0,
            0,
        ],
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[keyboard] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
