//! DNS messages, as much of them as a forwarder reads (RFC 1035): the
//! question, and the time each record may be kept.

use alloc::vec::Vec;

pub const HEADER: usize = 12;
/// A record of an IPv4 address; the start of a zone, which an answer that a
/// name is not there carries; and EDNS's, whose TTL is not one.
pub const TYPE_A: u16 = 1;
const TYPE_SOA: u16 = 6;
pub const TYPE_OPT: u16 = 41;
pub const CLASS_IN: u16 = 1;
/// Answers' codes.
pub const NO_ERROR: u8 = 0;
pub const FORMAT_ERROR: u8 = 1;
pub const SERVER_FAILURE: u8 = 2;
pub const NO_SUCH_NAME: u8 = 3;
/// The longest a name is, in its wire form.
const NAME_MAX: usize = 255;

pub fn be16(m: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(at)?, *m.get(at + 1)?]))
}

fn be32(m: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([*m.get(at)?, *m.get(at + 1)?, *m.get(at + 2)?, *m.get(at + 3)?]))
}

pub fn id(m: &[u8]) -> Option<u16> {
    be16(m, 0)
}

pub fn set_id(m: &mut [u8], id: u16) {
    if m.len() >= 2 {
        m[..2].copy_from_slice(&id.to_be_bytes());
    }
}

/// Whether a message is an answer rather than a question.
pub fn is_answer(m: &[u8]) -> bool {
    m.get(2).is_some_and(|f| f & 0x80 != 0)
}

pub fn rcode(m: &[u8]) -> u8 {
    m.get(3).map_or(SERVER_FAILURE, |f| f & 0x0F)
}

/// What is asked: the name in its wire form, lower case, and the type and
/// class. One question, as every resolver asks; a name with a pointer in it
/// is not one a question has.
#[derive(Clone, PartialEq, Eq)]
pub struct Question {
    pub name: Vec<u8>,
    pub qtype: u16,
    pub qclass: u16,
}

/// The question of a message, and where it ends.
pub fn question(m: &[u8]) -> Option<(Question, usize)> {
    if be16(m, 4)? != 1 {
        return None;
    }
    let mut at = HEADER;
    let mut name = Vec::new();
    loop {
        let len = usize::from(*m.get(at)?);
        if len & 0xC0 != 0 {
            return None;
        }
        name.push(len as u8);
        at += 1;
        if len == 0 {
            break;
        }
        for &b in m.get(at..at + len)? {
            name.push(b.to_ascii_lowercase());
        }
        at += len;
        if name.len() > NAME_MAX {
            return None;
        }
    }
    let q = Question { name, qtype: be16(m, at)?, qclass: be16(m, at + 2)? };
    Some((q, at + 4))
}

/// Past a name that may end in a pointer.
fn skip_name(m: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *m.get(at)?;
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        if len & 0xC0 != 0 {
            return None;
        }
        at += 1 + usize::from(len);
        if len == 0 {
            return Some(at);
        }
    }
}

/// A record: its type, its TTL and where the TTL is, and its data.
pub struct Record {
    pub rtype: u16,
    pub ttl: u32,
    pub ttl_at: usize,
    pub data: core::ops::Range<usize>,
}

/// Every record after the question — answers, authority, additional — and
/// in which of those it is (0, 1, 2); nothing if the message is not whole.
pub fn records(m: &[u8]) -> Option<Vec<(usize, Record)>> {
    let (_, mut at) = question(m)?;
    let counts = [be16(m, 6)?, be16(m, 8)?, be16(m, 10)?];
    let mut out = Vec::new();
    for (section, &count) in counts.iter().enumerate() {
        for _ in 0..count {
            at = skip_name(m, at)?;
            let rtype = be16(m, at)?;
            let ttl = be32(m, at + 4)?;
            let len = usize::from(be16(m, at + 8)?);
            let data = at + 10..at + 10 + len;
            if data.end > m.len() {
                return None;
            }
            out.push((section, Record { rtype, ttl, ttl_at: at + 4, data: data.clone() }));
            at = data.end;
        }
    }
    Some(out)
}

/// For how long an answer may be kept, in seconds: the least TTL of what it
/// says, and of a name that is not there, of the zone's SOA and its minimum
/// (RFC 2308). Nothing for an answer not to be kept: a failure, one cut
/// short, and one that a name is not there that gives no SOA, which RFC 2308
/// says is not to be.
pub fn keep_for(m: &[u8]) -> Option<u32> {
    if m.get(2).is_some_and(|f| f & 0x02 != 0) {
        return None;
    }
    let code = rcode(m);
    if code != NO_ERROR && code != NO_SUCH_NAME {
        return None;
    }
    let recs = records(m)?;
    let answers: Vec<&Record> = recs.iter().filter(|(s, r)| *s == 0 && r.rtype != TYPE_OPT).map(|(_, r)| r).collect();
    if code == NO_ERROR && !answers.is_empty() {
        return answers.iter().map(|r| r.ttl).min();
    }
    let soa = recs.iter().find(|(s, r)| *s == 1 && r.rtype == TYPE_SOA).map(|(_, r)| r)?;
    // The minimum is the SOA's last word.
    if soa.data.len() < 4 {
        return None;
    }
    Some(soa.ttl.min(be32(m, soa.data.end - 4)?))
}

/// The first IPv4 address an answer gives.
pub fn first_a(m: &[u8]) -> Option<[u8; 4]> {
    records(m)?
        .into_iter()
        .find(|(s, r)| *s == 0 && r.rtype == TYPE_A && r.data.len() == 4)
        .map(|(_, r)| [m[r.data.start], m[r.data.start + 1], m[r.data.start + 2], m[r.data.start + 3]])
}

/// A question for `name`, of `qtype`, asking for recursion.
pub fn query(id: u16, name: &[u8], qtype: u16) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split(|&b| b == b'.').filter(|l| !l.is_empty()) {
        m.push(label.len() as u8);
        m.extend(label.iter().map(|b| b.to_ascii_lowercase()));
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&CLASS_IN.to_be_bytes());
    m
}

/// An answer to `asked` that says only `code`: its header and its question.
pub fn failure(asked: &[u8], code: u8) -> Vec<u8> {
    let end = question(asked).map_or(HEADER, |(_, end)| end).min(asked.len());
    let mut m = asked[..end.max(HEADER.min(asked.len()))].to_vec();
    m.resize(m.len().max(HEADER), 0);
    m[2] = 0x80 | (m[2] & 0x79);
    m[3] = 0x80 | code;
    let qd: u16 = if end > HEADER { 1 } else { 0 };
    m[4..6].copy_from_slice(&qd.to_be_bytes());
    m[6..12].fill(0);
    m
}

/// Every TTL in an answer made `left` seconds, as it is handed out of the
/// cache: what an answer kept a while says of itself is what is left of it.
pub fn age(m: &mut [u8], ttls: &[usize], left: u32) {
    for &at in ttls {
        if at + 4 <= m.len() {
            m[at..at + 4].copy_from_slice(&left.to_be_bytes());
        }
    }
}
