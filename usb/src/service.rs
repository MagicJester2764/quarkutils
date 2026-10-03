//! The thread that answers: `input`, for keys and movement, and anybody, for
//! what is plugged in (`quark_rt::usb`). It never waits on the controller,
//! so neither does anybody it answers.
//!
//! It offers this program to `input` as a source of keys and movement
//! (`TAG_INPUT_SOURCE`), which takes it if the device manager says this is a
//! driver it started — `input` is started after the drivers in the boot
//! image, so this asks until there is one to ask. From then on it answers
//! `input` as the i8042's driver does, and nobody else.

use crate::shared::SHARED;
use quark_rt::ipc::{death_notice, Message, TAG_PING, TID_ANY};
use quark_rt::{nameserver, println, syscall, usb};

const TAG_KEY_EVENT: u64 = 2;
const TAG_NO_KEY: u64 = 3;
const TAG_GET_KEY_NB: u64 = 5;
const TAG_GET_MOUSE_NB: u64 = 6;
const TAG_MOUSE_EVENT: u64 = 7;
const TAG_INPUT_SOURCE: u64 = 0x207;
const KEY_PRESS: u64 = 1;
const KEY_RELEASE: u64 = 2;
const TAG_ERROR: u64 = u64::MAX;

/// How often `input` is looked for until it is found, and for how long.
const LOOK_EVERY_NS: u64 = 100_000_000;
const LOOKS: u32 = 600;

/// Offer this program to `input`: its task, if it took it.
fn offer() -> Option<usize> {
    let tid = nameserver::lookup(b"input")?;
    let msg = Message { sender: 0, tag: TAG_INPUT_SOURCE, data: [0; 6] };
    let mut reply = Message::empty();
    if syscall::sys_call_offer_self(tid, &msg, &mut reply).is_err() || reply.tag != 0 {
        return None;
    }
    let _ = syscall::sys_task_watch(tid);
    Some(tid)
}

pub extern "C" fn serve() -> ! {
    let name = (0..4u8).map(|n| [b'u', b's', b'b', b'0' + n]).find(|name| nameserver::register(name).is_ok());
    if let Some(name) = name {
        println!("[usb] Registered as {}.", core::str::from_utf8(&name).unwrap_or("usb"));
    }
    let mut input = 0usize;
    let mut looks = 0u32;
    loop {
        if input == 0 && looks < LOOKS {
            looks += 1;
            if let Some(tid) = offer() {
                input = tid;
                SHARED.lock().input = tid;
            } else if looks == LOOKS {
                println!("[usb] input would not take keys from here; none will be sent");
            }
        }
        let mut msg = Message::empty();
        let received = if input == 0 && looks < LOOKS {
            syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(LOOK_EVERY_NS)).is_ok()
        } else {
            syscall::sys_recv(TID_ANY, &mut msg).is_ok()
        };
        if !received {
            continue;
        }
        if let Some(dead) = death_notice(&msg) {
            if dead == input {
                input = 0;
                looks = 0;
                SHARED.lock().input = 0;
            }
            continue;
        }
        if msg.sender == 0 {
            continue;
        }
        let none = Message { sender: 0, tag: TAG_NO_KEY, data: [0; 6] };
        let reply = match msg.tag {
            TAG_GET_KEY_NB if msg.sender == input => match SHARED.lock().keys.pop() {
                Some(key) => Message {
                    sender: 0,
                    tag: TAG_KEY_EVENT,
                    data: [
                        if key.press { KEY_PRESS } else { KEY_RELEASE },
                        key.ascii as u64,
                        key.code as u64,
                        key.modifiers as u64,
                        0,
                        0,
                    ],
                },
                None => none,
            },
            TAG_GET_MOUSE_NB if msg.sender == input => match SHARED.lock().movements.pop() {
                Some(m) => Message {
                    sender: 0,
                    tag: TAG_MOUSE_EVENT,
                    data: [m.dx as i64 as u64, m.dy as i64 as u64, m.buttons as u64, m.wheel as i64 as u64, 0, 0],
                },
                None => none,
            },
            usb::TAG_DEVICE => {
                let listed = SHARED.lock().listed;
                match listed.iter().flatten().nth(msg.data[0] as usize) {
                    Some(device) => Message { sender: 0, tag: 0, data: device.words() },
                    None => Message { sender: 0, tag: TAG_ERROR, data: [0; 6] },
                }
            }
            TAG_PING => Message { sender: 0, tag: TAG_PING, data: [0; 6] },
            _ => Message { sender: 0, tag: TAG_ERROR, data: [0; 6] },
        };
        let _ = syscall::sys_reply(msg.sender, &reply);
    }
}
