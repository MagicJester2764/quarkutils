//! What the stack is doing, said to anybody; and its filter, changed by a
//! holder of the right to.
//!
//! `TAG_STATUS` (22) writes what the stack is doing, as text, into a
//! buffer the caller lends: the card and its addresses, the ways out, the
//! DNS servers, `lo`, the resolver's counts, every socket and what state it
//! is in, and the filter. `netctl` prints it.
//!
//! `TAG_FILTER` (23) changes the filter: a rule added at the end, one taken
//! out by its number, or all of them. The caller offers its right to run
//! the network with the call (`SYS_CALL_OFFER`, `NetAdmin`); the stack
//! takes it to look at and lets go of it again, and a caller that offered
//! nothing, or something else, is refused (EPERM). The stack keeps nobody's
//! rights.

use alloc::string::String;
use core::fmt::Write;
use quark_rt::ipc::Message;
use quark_rt::syscall;

use crate::filter::{self, Rule, MAX_RULES};
use crate::legacy::Clients;
use crate::resolver::Resolver;
use crate::sockets::Sockets;
use crate::stack::Stack;

pub const TAG_STATUS: u64 = 22;
pub const TAG_FILTER: u64 = 23;
/// `TAG_FILTER`'s operations, in `data[0]`.
const ADD: u64 = 0;
const REMOVE: u64 = 1;
const CLEAR: u64 = 2;
/// A rule, in `data[1]`: drop rather than let in; an address to come from
/// in `data[3..5]`, and whether it is IPv6's; the protocol (bits 8..16) and
/// the bits of the address that must match (16..24). `data[2]` is the first
/// port and the last, `first << 16 | last`.
const RULE_DROP: u64 = 1;
const RULE_FROM: u64 = 2;
const RULE_FROM_V6: u64 = 4;

const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;
const EPERM: u64 = 1;
const EINVAL: u64 = 22;
const ENOSPC: u64 = 28;

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

/// What the stack is doing, for `tid`, into the `room` bytes it lent:
/// `[written, how long it all was]`.
pub fn status(net: &mut Stack, socks: &Sockets, clients: &Clients, resolver: &Resolver, tid: usize, room: usize) {
    let mut out = String::new();
    net.describe(&mut out);
    let c = resolver.counts;
    let _ = writeln!(
        out,
        "resolver  asked {}, of a server {}, from what was kept {}, failed {}",
        c.asked, c.forwarded, c.kept, c.failed
    );
    let _ = writeln!(out, "sockets");
    socks.describe(net, &mut out);
    clients.describe(net, &mut out);
    let f = net.filter.borrow();
    let _ = writeln!(out, "filter  {} rule{}, {} dropped", f.rules.len(), if f.rules.len() == 1 { "" } else { "s" }, f.dropped);
    for (i, r) in f.rules.iter().enumerate() {
        let _ = write!(out, "  {} ", i + 1);
        r.describe(&mut out);
        out.push('\n');
    }
    drop(f);
    let n = out.len().min(room);
    if n > 0 && syscall::sys_lent_write(tid, 0, &out.as_bytes()[..n]).is_err() {
        return reply(tid, TAG_ERROR, [EINVAL, 0, 0, 0, 0, 0]);
    }
    reply(tid, TAG_OK, [n as u64, out.len() as u64, 0, 0, 0, 0]);
}

/// Whether `tid` offered the right to run the network with its call. The
/// capability is taken to be looked at and let go of again.
fn may_change(tid: usize) -> bool {
    let Ok(slot) = syscall::sys_cap_take_any(tid) else { return false };
    let me = syscall::sys_getpid() as usize;
    let right = syscall::sys_cap_read(me, slot).is_ok_and(|c| c.cap_type == syscall::CAP_TYPE_NET_ADMIN && c.valid);
    let _ = syscall::sys_cap_delete(slot);
    right
}

/// A rule out of a request's words, if it is one.
fn rule_of(d: &[u64; 6]) -> Option<Rule> {
    let flags = d[1];
    let protocol = (flags >> 8) as u8;
    let bits = (flags >> 16) as u8;
    let ports = ((d[2] >> 16) as u16, d[2] as u16);
    if !matches!(protocol, filter::ANY | filter::ICMP | filter::TCP | filter::UDP) || ports.0 > ports.1 {
        return None;
    }
    let from = if flags & RULE_FROM != 0 {
        let v6 = flags & RULE_FROM_V6 != 0;
        if bits > if v6 { 128 } else { 32 } {
            return None;
        }
        let mut addr = [0u8; 16];
        addr[..8].copy_from_slice(&d[3].to_le_bytes());
        addr[8..].copy_from_slice(&d[4].to_le_bytes());
        Some((addr, bits, v6))
    } else {
        None
    };
    Some(Rule { drop: flags & RULE_DROP != 0, protocol, ports, from })
}

/// A change to the filter: `[how many rules there are]`.
pub fn change(net: &mut Stack, msg: &Message) {
    let tid = msg.sender;
    if !may_change(tid) {
        return reply(tid, TAG_ERROR, [EPERM, 0, 0, 0, 0, 0]);
    }
    let mut f = net.filter.borrow_mut();
    match msg.data[0] {
        ADD => {
            let Some(rule) = rule_of(&msg.data) else {
                return reply(tid, TAG_ERROR, [EINVAL, 0, 0, 0, 0, 0]);
            };
            if f.rules.len() >= MAX_RULES {
                return reply(tid, TAG_ERROR, [ENOSPC, 0, 0, 0, 0, 0]);
            }
            f.rules.push(rule);
        }
        // By the number `netctl` shows: from one.
        REMOVE => {
            let n = msg.data[1] as usize;
            if n == 0 || n > f.rules.len() {
                return reply(tid, TAG_ERROR, [EINVAL, 0, 0, 0, 0, 0]);
            }
            f.rules.remove(n - 1);
        }
        CLEAR => f.rules.clear(),
        _ => return reply(tid, TAG_ERROR, [EINVAL, 0, 0, 0, 0, 0]),
    }
    reply(tid, TAG_OK, [f.rules.len() as u64, 0, 0, 0, 0, 0]);
}
