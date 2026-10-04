//! The machine's resolver: DNS on 127.0.0.1:53 and [::1]:53.
//!
//! It is where a C library asks when /etc/resolv.conf names nobody — musl
//! asks 127.0.0.1 — and where this server's own lookups go
//! (`TAG_DNS_RESOLVE`). It forwards: a question is asked of the servers
//! DHCP and routers named, one after another until one answers, and the
//! answer is handed back as it came, with the asker's id. An answer is
//! kept for as long as it says — the least of its TTLs; for a name that is
//! not there, its zone's SOA minimum, and not at all without one (RFC
//! 2308) — and a question asked again in that time is answered from here,
//! its TTLs what is left of them. Only this machine is answered: the
//! socket is `lo`'s.
//!
//! An answer cut short (TC) is handed back cut short and not kept: nothing
//! is asked again over TCP.

use alloc::vec;
use alloc::vec::Vec;
use quark_rt::ipc::Message;
use quark_rt::syscall;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::udp;
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};

use crate::dns::{self, Question};
use crate::stack::{now, Side, Stack};

const PORT: u16 = 53;
/// Answers kept, and questions waiting for one.
const KEPT: usize = 256;
const PENDING: usize = 64;
/// How long one server has to answer before the next is asked, and how long
/// they all have.
const EACH: Duration = Duration::from_secs(2);
const ALL: Duration = Duration::from_secs(6);
/// The longest message taken from anybody.
const MESSAGE_MAX: usize = 4096;
/// Where the questions this resolver asks leave from: below every port a
/// socket is given.
const UPSTREAM_PORTS: (u16, u16) = (20000, 30000);

/// What it has done, asked for: `[asked, forwarded, kept, failed]`.
pub const TAG_RESOLVER: u64 = 21;
const TAG_OK: u64 = 0;
const TAG_ERROR: u64 = u64::MAX;
/// The old protocol's answer for what is not a name.
const NOT_A_NAME: u64 = 1;
/// The old protocol's answers for a name: not one, nothing found, nothing
/// heard.
const NO_ADDRESS: u64 = 2;
const NO_ANSWER: u64 = 3;
const BUSY: u64 = 4;

enum Asker {
    /// A program, over `lo`, which asked with an id of its own.
    Lo { from: IpEndpoint, id: u16 },
    /// A task of the old protocol, waiting for one IPv4 address.
    Task { tid: usize },
}

struct Pending {
    asker: Asker,
    question: Question,
    /// What is sent upstream: the question, with `id`.
    message: Vec<u8>,
    id: u16,
    server: usize,
    sent: Instant,
    began: Instant,
}

struct Kept {
    question: Question,
    answer: Vec<u8>,
    /// Where its TTLs are, to be said again as what is left.
    ttls: Vec<usize>,
    until: Instant,
}

/// What it has done: questions asked of it, asked of a server, answered
/// from what was kept, and failed.
#[derive(Default, Clone, Copy)]
pub struct Counts {
    pub asked: u64,
    pub forwarded: u64,
    pub kept: u64,
    pub failed: u64,
}

pub struct Resolver {
    listen: SocketHandle,
    upstream: SocketHandle,
    pending: Vec<Pending>,
    kept: Vec<Kept>,
    pub counts: Counts,
}

fn random_u16() -> u16 {
    let mut b = [0u8; 2];
    let _ = quark_rt::random::fill(&mut b);
    u16::from_le_bytes(b)
}

/// The servers questions are asked of: DHCP's and the routers', but never
/// this machine, which would be asking itself.
fn servers(net: &Stack) -> Vec<IpAddress> {
    net.dns()
        .into_iter()
        .filter(|a| match a {
            IpAddress::Ipv4(a) => !a.is_loopback(),
            IpAddress::Ipv6(a) => !a.is_loopback(),
        })
        .collect()
}

fn new_udp(packets: usize) -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; packets], vec![0; packets * 512]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; packets], vec![0; packets * 512]),
    )
}

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

/// The name an old protocol's lookup asks for: up to 48 bytes in its six
/// words, to the first nought — if it is a name. One that is not one is not
/// asked of anybody: a question leaves the machine, and whatever is
/// upstream reads it.
pub fn name_of(msg: &Message) -> Option<Vec<u8>> {
    let mut name = Vec::with_capacity(48);
    for w in msg.data {
        name.extend_from_slice(&w.to_le_bytes());
    }
    let len = name.iter().position(|&b| b == 0).unwrap_or(48);
    name.truncate(len);
    quark_rt::net::valid_hostname(&name).then_some(name)
}

pub fn refuse_name(tid: usize) {
    reply(tid, TAG_ERROR, [NOT_A_NAME, 0, 0, 0, 0, 0]);
}

impl Resolver {
    pub fn new(net: &mut Stack) -> Resolver {
        let mut listen = new_udp(16);
        let _ = listen.bind(IpListenEndpoint { addr: None, port: PORT });
        let mut upstream = new_udp(16);
        let span = UPSTREAM_PORTS.1 - UPSTREAM_PORTS.0;
        let _ = upstream.bind(IpListenEndpoint { addr: None, port: UPSTREAM_PORTS.0 + random_u16() % span });
        Resolver {
            listen: net.sockets(Side::Lo).add(listen),
            upstream: net.sockets(Side::Eth).add(upstream),
            pending: Vec::new(),
            kept: Vec::new(),
            counts: Counts::default(),
        }
    }

    /// The kept answer to `q`, said as of now, with `id`.
    fn from_kept(&mut self, q: &Question, id: u16) -> Option<Vec<u8>> {
        let t = now();
        self.kept.retain(|k| k.until > t);
        let k = self.kept.iter().find(|k| k.question == *q)?;
        let mut answer = k.answer.clone();
        dns::set_id(&mut answer, id);
        dns::age(&mut answer, &k.ttls, ((k.until - t).total_millis() / 1000) as u32);
        Some(answer)
    }

    fn keep(&mut self, q: &Question, answer: &[u8]) {
        let Some(secs) = dns::keep_for(answer) else { return };
        if secs == 0 {
            return;
        }
        let ttls = dns::records(answer)
            .map(|rs| rs.into_iter().filter(|(_, r)| r.rtype != dns::TYPE_OPT).map(|(_, r)| r.ttl_at).collect())
            .unwrap_or_default();
        self.kept.retain(|k| k.question != *q);
        if self.kept.len() >= KEPT {
            // The one that would go soonest goes now.
            if let Some(i) = (0..self.kept.len()).min_by_key(|&i| self.kept[i].until) {
                self.kept.swap_remove(i);
            }
        }
        self.kept.push(Kept { question: q.clone(), answer: answer.to_vec(), ttls, until: now() + Duration::from_secs(secs.into()) });
    }

    /// Ask the next server `p`'s question; false if there is none left.
    fn ask(net: &mut Stack, upstream: SocketHandle, p: &mut Pending) -> bool {
        let Some(&server) = servers(net).get(p.server) else { return false };
        p.sent = now();
        let s = net.sockets(Side::Eth).get_mut::<udp::Socket>(upstream);
        let _ = s.send_slice(&p.message, IpEndpoint::new(server, PORT));
        true
    }

    /// A question to be asked upstream, for `asker`; one more than there is
    /// room for is turned away.
    fn forward(&mut self, net: &mut Stack, asker: Asker, question: Question, mut message: Vec<u8>) {
        if self.pending.len() >= PENDING {
            self.answer_failed(net, asker, &message, dns::SERVER_FAILURE, BUSY);
            return;
        }
        let id = random_u16();
        dns::set_id(&mut message, id);
        let t = now();
        let mut p = Pending { asker, question, message, id, server: 0, sent: t, began: t };
        if Self::ask(net, self.upstream, &mut p) {
            self.counts.forwarded += 1;
            self.pending.push(p);
        } else {
            self.answer_failed(net, p.asker, &p.message, dns::SERVER_FAILURE, NO_ANSWER);
        }
    }

    fn answer_failed(&mut self, net: &mut Stack, asker: Asker, asked: &[u8], code: u8, old_code: u64) {
        self.counts.failed += 1;
        match asker {
            Asker::Lo { from, id } => {
                let mut m = dns::failure(asked, code);
                dns::set_id(&mut m, id);
                let _ = net.sockets(Side::Lo).get_mut::<udp::Socket>(self.listen).send_slice(&m, from);
            }
            Asker::Task { tid } => reply(tid, TAG_ERROR, [old_code, 0, 0, 0, 0, 0]),
        }
    }

    fn answer(&mut self, net: &mut Stack, asker: &Asker, answer: &mut Vec<u8>) {
        match *asker {
            Asker::Lo { from, id } => {
                dns::set_id(answer, id);
                let _ = net.sockets(Side::Lo).get_mut::<udp::Socket>(self.listen).send_slice(answer, from);
            }
            Asker::Task { tid } => match dns::first_a(answer) {
                Some(a) => reply(tid, TAG_OK, [u32::from_be_bytes(a) as u64, 0, 0, 0, 0, 0]),
                None => reply(tid, TAG_ERROR, [NO_ADDRESS, 0, 0, 0, 0, 0]),
            },
        }
    }

    /// The old protocol's lookup of `name`'s IPv4 address, for `tid`.
    pub fn resolve_for(&mut self, net: &mut Stack, tid: usize, name: &[u8]) {
        self.counts.asked += 1;
        let message = dns::query(0, name, dns::TYPE_A);
        let Some((question, _)) = dns::question(&message) else {
            return refuse_name(tid);
        };
        if let Some(mut kept) = self.from_kept(&question, 0) {
            self.counts.kept += 1;
            return self.answer(net, &Asker::Task { tid }, &mut kept);
        }
        let _ = syscall::sys_task_watch(tid);
        self.forward(net, Asker::Task { tid }, question, message);
    }

    pub fn counts(&self, tid: usize) {
        let c = self.counts;
        reply(tid, TAG_OK, [c.asked, c.forwarded, c.kept, c.failed, 0, 0]);
    }

    /// `tid` is waiting for nothing here any more.
    pub fn forget(&mut self, tid: usize) {
        self.pending.retain(|p| !matches!(p.asker, Asker::Task { tid: t } if t == tid));
    }

    /// After a turn of the loop: questions this machine asked, answers from
    /// upstream, and servers that have taken too long.
    pub fn settle(&mut self, net: &mut Stack) {
        let mut buf = vec![0u8; MESSAGE_MAX];
        loop {
            let s = net.sockets(Side::Lo).get_mut::<udp::Socket>(self.listen);
            let Ok((n, meta)) = s.recv_slice(&mut buf) else { break };
            let asked = buf[..n].to_vec();
            self.asked(net, meta.endpoint, &asked);
        }
        loop {
            let s = net.sockets(Side::Eth).get_mut::<udp::Socket>(self.upstream);
            let Ok((n, meta)) = s.recv_slice(&mut buf) else { break };
            let mut answer = buf[..n].to_vec();
            self.answered(net, meta.endpoint, &mut answer);
        }
        let t = now();
        let mut i = 0;
        while i < self.pending.len() {
            let p = &mut self.pending[i];
            if t - p.began >= ALL {
                let p = self.pending.swap_remove(i);
                self.answer_failed(net, p.asker, &p.message, dns::SERVER_FAILURE, NO_ANSWER);
                continue;
            }
            if t - p.sent >= EACH {
                // That one has had its turn: the next, or the first again.
                p.server += 1;
                if !Self::ask(net, self.upstream, p) {
                    p.server = 0;
                    Self::ask(net, self.upstream, p);
                }
            }
            i += 1;
        }
    }

    /// A question from a program of this machine.
    fn asked(&mut self, net: &mut Stack, from: IpEndpoint, asked: &[u8]) {
        self.counts.asked += 1;
        let Some(id) = dns::id(asked) else { return };
        if dns::is_answer(asked) {
            return;
        }
        let Some((question, _)) = dns::question(asked) else {
            return self.answer_failed(net, Asker::Lo { from, id }, asked, dns::FORMAT_ERROR, 0);
        };
        if let Some(mut kept) = self.from_kept(&question, id) {
            self.counts.kept += 1;
            return self.answer(net, &Asker::Lo { from, id }, &mut kept);
        }
        self.forward(net, Asker::Lo { from, id }, question, asked.to_vec());
    }

    /// An answer from upstream: to the question it answers, from the server
    /// it was asked of, or nobody's.
    fn answered(&mut self, net: &mut Stack, from: IpEndpoint, answer: &mut Vec<u8>) {
        let Some(id) = dns::id(answer) else { return };
        let servers = servers(net);
        let Some(i) = self.pending.iter().position(|p| {
            p.id == id && servers.get(p.server) == Some(&from.addr) && from.port == PORT
        }) else {
            return;
        };
        if !dns::is_answer(answer) || dns::question(answer).map(|(q, _)| q) != Some(self.pending[i].question.clone()) {
            return;
        }
        let code = dns::rcode(answer);
        if code != dns::NO_ERROR && code != dns::NO_SUCH_NAME {
            // A server that would not answer: the next is asked, if there is one.
            let p = &mut self.pending[i];
            p.server += 1;
            if Self::ask(net, self.upstream, p) {
                return;
            }
        }
        let p = self.pending.swap_remove(i);
        self.keep(&p.question, answer);
        self.answer(net, &p.asker, answer);
    }
}
