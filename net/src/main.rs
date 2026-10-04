#![no_std]
#![no_main]

extern crate alloc;

mod card;
mod control;
mod dns;
mod filter;
mod legacy;
mod lo;
mod ndp;
mod resolver;
mod sockets;
mod stack;

use quark_rt::ipc::{death_notice, fd_released_notice, Message, TAG_FD_READ, TAG_FD_WRITE, TID_ANY};
use quark_rt::manifest::CapReq;
use quark_rt::{nameserver, nic, println, syscall};

// The network stack, and nothing else: above whatever card its driver
// serves as `eth0` (`quark_rt::nic`), it holds no device, no port and no
// memory of anybody's — a client lends its data with the call, and the
// card's driver is lent each frame. It was the RTL8139's driver too once,
// holding every port on the machine and every interrupt line.
//
// The protocols are smoltcp's (`stack`), unpatched; what is this server's is
// the card under them (`card`), what routers say (`ndp`), the machine's
// resolver (`resolver`), what is let in (`filter`), what it is doing and
// who may change that (`control`), and what programs ask of them: sockets
// that are descriptors (`sockets`), and the calls programs made before
// there were (`legacy`).
//
// In the drivers' band all the same, as it was then: in the servers' it
// waited for `init`, which starts the system in the drivers' band, and had
// the network up only after somebody had been asked to log in.
quark_rt::manifest!([
    CapReq::priority(quark_rt::syscall::PRIO_DRIVER),
]);

/// How long to wait for the first card to be registered: its driver is
/// started beside this, and has a card to reset first.
const CARD_WAIT_MS: u64 = 30_000;
/// How long DHCP has to answer before the address QEMU's user network
/// would have given is taken, and anybody is told the network is here.
const DHCP_WAIT_MS: u64 = 5_000;
/// The longest the loop sleeps when nothing is due: the card is asked
/// whether frames came at least this often, in case its saying so was
/// missed.
const MAX_SLEEP_MS: u64 = 200;

fn ms() -> u64 {
    syscall::sys_clock() / 1_000_000
}

/// Wait for a message for up to `wait_ms`: what the kernel or the card
/// says, or a request, which is `Some`.
fn wait(net: &mut stack::Stack, wait_ms: u64) -> Option<Message> {
    let mut msg = Message::empty();
    if syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(wait_ms.max(1) * 1_000_000)).is_err() {
        net.card.stirred = true;
        return None;
    }
    if msg.sender == 0 {
        // The card saying frames have come, or the kernel saying anything
        // else: look either way.
        net.card.stirred = true;
    }
    Some(msg)
}

/// Sockets whose last descriptor has gone, collected and closed. One notice
/// says there are some, however many.
fn reap(socks: &mut sockets::Sockets, net: &mut stack::Stack) {
    while let Some(cookie) = syscall::sys_fd_reap() {
        if sockets::Sockets::is_ours(cookie) {
            socks.closed(net, cookie);
        }
    }
}

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    println!("[net] Started.");

    // The first card, once its driver has said it is there.
    let mut waited = 0;
    let (link, mac) = loop {
        if let Some(found) = nic::Link::claim(b"eth0") {
            break found;
        }
        if waited >= CARD_WAIT_MS {
            println!("[net] No network card. Exiting.");
            syscall::sys_exit();
        }
        // Often: the whole of the rest of the boot can take less than a
        // tenth of a second, and the network is up before anybody is asked
        // to log in only if it starts as soon as the card is there.
        syscall::sleep_ms(10);
        waited += 10;
    };
    println!(
        "[net] eth0, address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    let mut net = stack::Stack::new(link, mac);
    let mut clients = legacy::Clients::new();
    let mut socks = sockets::Sockets::new();
    let mut resolver = resolver::Resolver::new(&mut net);

    // An address first, before anybody can ask for anything.
    let began = ms();
    while !net.configured() && ms() - began < DHCP_WAIT_MS {
        net.poll();
        let _ = wait(&mut net, 10);
    }
    if !net.configured() {
        println!("[net] DHCP timeout, using static config.");
        net.fallback();
    }

    // Said before it registers, and nothing after: the service manager
    // starts the session once this has registered, and a line printed after
    // the login prompt pushes the prompt off its line.
    let ip = net.ipv4_word().to_be_bytes();
    println!("[net] IP {}.{}.{}.{} — ready.", ip[0], ip[1], ip[2], ip[3]);
    if nameserver::register(b"net").is_err() {
        println!("[net] Failed to register with nameserver.");
    }

    loop {
        net.poll();
        clients.settle(&mut net);
        socks.settle(&mut net);
        resolver.settle(&mut net);
        let due = net.poll_delay().map_or(MAX_SLEEP_MS, |d| d.total_millis()).min(MAX_SLEEP_MS);
        let Some(msg) = wait(&mut net, due) else { continue };
        // A client has died: its connections go, and anything it was
        // waiting for. Nobody is waiting for an answer to this; the same tag
        // from anybody else is an unknown request.
        if let Some(dead) = death_notice(&msg) {
            clients.gone(&mut net, dead);
            socks.forget(dead);
            resolver.forget(dead);
            continue;
        }
        if fd_released_notice(&msg) {
            reap(&mut socks, &mut net);
            continue;
        }
        if msg.sender == 0 {
            continue;
        }
        net.poll();
        // A task is in one call at a time: one asking anything now is not
        // waiting for what it asked before, of either kind.
        clients.abandon(&mut net, msg.sender);
        socks.forget(msg.sender);
        resolver.forget(msg.sender);
        if matches!(msg.tag, TAG_FD_READ | TAG_FD_WRITE) && sockets::Sockets::is_ours(msg.data[0]) {
            socks.io(&mut net, &msg);
        } else if msg.tag == legacy::TAG_DNS_RESOLVE {
            match resolver::name_of(&msg) {
                Some(name) => resolver.resolve_for(&mut net, msg.sender, &name),
                None => resolver::refuse_name(msg.sender),
            }
        } else if msg.tag == resolver::TAG_RESOLVER {
            resolver.counts(msg.sender);
        } else if msg.tag == control::TAG_STATUS {
            control::status(&mut net, &socks, &clients, &resolver, msg.sender, msg.data[0] as usize);
        } else if msg.tag == control::TAG_FILTER {
            control::change(&mut net, &msg);
        } else if msg.tag == sockets::TAG_SOCKET {
            // A socket closed a moment ago is gone before anything is asked
            // of another: what it had may be what is asked for.
            reap(&mut socks, &mut net);
            socks.request(&mut net, &msg);
        } else {
            clients.request(&mut net, &msg);
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[net] PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}
