//! FAT's directories, with the names VFAT gives them.
//!
//! An entry is thirty-two bytes: an 8.3 name, the attributes, the times, the
//! first cluster and the length. A name that is not 8.3 — longer, with
//! lower case and upper, with characters 8.3 has no room for — is kept as a
//! run of entries just before that one, thirteen UTF-16 characters each,
//! the last piece first, each carrying a checksum of the 8.3 name they
//! belong to: a system that knows nothing of them sees the 8.3 name and
//! steps over them, and one that does sees the name it was given. The 8.3
//! name is still made for every file, and is what names the entry here
//! (`Entry::short`): it is the one thing in a directory that is unique and
//! fits in a word.
//!
//! A directory is a chain of clusters, but FAT12's and FAT16's root, which
//! is a region of its own after the tables, fixed in size; it is called
//! cluster 0 here, as a subdirectory's `..` calls it.

use crate::{read_u16, read_u32, DiskState, FatKind, ERR_EXISTS, ERR_INVALID_PATH, ERR_IO, ERR_NAME_TOO_LONG, ERR_NO_SPACE, SECTOR_CACHE};

pub const ATTR_READ_ONLY: u8 = 0x01;
pub const ATTR_VOLUME: u8 = 0x08;
pub const ATTR_DIR: u8 = 0x10;
pub const ATTR_ARCHIVE: u8 = 0x20;
/// The attributes a piece of a long name has, and nothing else does.
const ATTR_LONG: u8 = 0x0F;
/// Where the NT case byte says a short name's base or extension is in
/// lower case.
const LOWER_BASE: u8 = 0x08;
const LOWER_EXT: u8 = 0x10;

/// The most UTF-16 characters a long name has, and the most pieces it is.
const MAX_UNITS: usize = 255;
const MAX_PIECES: usize = 20;
/// The longest a name is here, as UTF-8.
pub const MAX_NAME: usize = 3 * MAX_UNITS;
/// Where in a piece its thirteen characters are.
const UNIT_AT: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// Where an entry is: the sector, and which of its sixteen.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Place {
    pub lba: u32,
    pub index: usize,
}

/// An entry of a directory.
#[derive(Clone, Copy)]
pub struct Entry {
    pub short: [u8; 11],
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
    /// The whole 8.3 entry, as it is on the disk.
    pub raw: [u8; 32],
    pub place: Place,
    /// The pieces of its long name, if it has one.
    pub pieces: [Place; MAX_PIECES],
    pub piece_count: usize,
    /// Its name, as a program is told it: the long name, or the 8.3 one
    /// written as a name, in the case the entry says.
    pub name: [u8; MAX_NAME],
    pub name_len: usize,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attr & ATTR_DIR != 0
    }

    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    pub fn is_dot(&self) -> bool {
        self.short[0] == b'.'
    }
}

/// The checksum a long name's pieces carry of the 8.3 name they belong to.
pub fn checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &c| (sum >> 1 | sum << 7).wrapping_add(c))
}

/// An 8.3 name written as a name: `HELLO   ELF` is `HELLO.ELF`, and in lower
/// case where the entry says so.
fn short_display(short: &[u8; 11], case: u8, out: &mut [u8]) -> usize {
    let base = short[..8].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    let ext = short[8..].iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    let mut n = 0;
    for (i, &b) in short[..base].iter().enumerate() {
        // A first byte of 0x05 stands for 0xE5, which would say "deleted".
        let b = if i == 0 && b == 0x05 { 0xE5 } else { b };
        out[n] = if case & LOWER_BASE != 0 { b.to_ascii_lowercase() } else { b };
        n += 1;
    }
    if ext > 0 {
        out[n] = b'.';
        n += 1;
        for &b in &short[8..8 + ext] {
            out[n] = if case & LOWER_EXT != 0 { b.to_ascii_lowercase() } else { b };
            n += 1;
        }
    }
    n
}

/// Whether two names are the same name to FAT, which does not tell upper
/// case from lower: by ASCII's cases, and every other byte as it is.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// UTF-16 to UTF-8, `units` to `out`: how many bytes, or `None` for what
/// is not UTF-16.
fn utf8_of(units: &[u16], out: &mut [u8]) -> Option<usize> {
    let mut n = 0;
    let mut i = 0;
    while i < units.len() {
        let u = units[i] as u32;
        i += 1;
        let c = if (0xD800..0xDC00).contains(&u) {
            let low = *units.get(i)? as u32;
            if !(0xDC00..0xE000).contains(&low) {
                return None;
            }
            i += 1;
            0x10000 + ((u - 0xD800) << 10) + (low - 0xDC00)
        } else if (0xDC00..0xE000).contains(&u) {
            return None;
        } else {
            u
        };
        let ch = char::from_u32(c)?;
        n += ch.encode_utf8(&mut out[n..]).len();
    }
    Some(n)
}

/// UTF-8 to UTF-16, `name` to `out`: how many units, or `None` for what is
/// not UTF-8, or is longer than a long name may be.
fn utf16_of(name: &[u8], out: &mut [u16; MAX_UNITS]) -> Option<usize> {
    let text = core::str::from_utf8(name).ok()?;
    let mut n = 0;
    for ch in text.chars() {
        let mut pair = [0u16; 2];
        for &u in ch.encode_utf16(&mut pair).iter() {
            if n == MAX_UNITS {
                return None;
            }
            out[n] = u;
            n += 1;
        }
    }
    Some(n)
}

/// Each sector of directory `dir`, in order, until `f` says to stop:
/// whether it said to. The root of a FAT12 or FAT16 volume is its region;
/// everything else a chain of clusters.
fn sectors(disk: &DiskState, dir: u32, mut f: impl FnMut(u32) -> Result<bool, u64>) -> Result<bool, u64> {
    if dir == 0 && disk.bpb.kind != FatKind::Fat32 {
        let start = disk.root_region();
        for s in 0..disk.bpb.root_sectors {
            if s % 8 == 0 {
                disk.prefetch_sectors(start + s, (disk.bpb.root_sectors - s).min(8));
            }
            if !f(start + s)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let spc = disk.bpb.sectors_per_cluster;
    let end = disk.cluster_count() + 2;
    let mut cluster = if dir == 0 { disk.bpb.root_cluster } else { dir };
    // No chain is longer than the volume: one that loops is damage.
    for _ in 0..end {
        if cluster < 2 || cluster >= end {
            return Err(ERR_IO);
        }
        let start = disk.cluster_start_lba(cluster);
        disk.prefetch_sectors(start, spc);
        for s in 0..spc {
            if !f(start + s)? {
                return Ok(true);
            }
        }
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => return Ok(false),
        }
    }
    Err(ERR_IO)
}

/// What a walk has heard of a long name so far: the pieces seen, in the
/// order they are on the disk, last piece first.
struct Long {
    units: [u16; MAX_PIECES * 13],
    /// How many pieces the name is, how many have been seen, and the
    /// checksum they carry; nought pieces for no name being heard.
    pieces: usize,
    seen: usize,
    sum: u8,
    places: [Place; MAX_PIECES],
}

impl Long {
    fn new() -> Long {
        Long { units: [0; MAX_PIECES * 13], pieces: 0, seen: 0, sum: 0, places: [Place::default(); MAX_PIECES] }
    }

    /// A piece, at `place`: the next in order, or the first of a new name.
    fn piece(&mut self, e: &[u8], place: Place) {
        let ordinal = (e[0] & 0x1F) as usize;
        if e[0] & 0x40 != 0 {
            if ordinal == 0 || ordinal > MAX_PIECES {
                self.pieces = 0;
                return;
            }
            self.pieces = ordinal;
            self.seen = 0;
            self.sum = e[13];
        } else if self.pieces == 0 || ordinal != self.pieces - self.seen || e[13] != self.sum {
            // Out of order, or another name's: nothing heard.
            self.pieces = 0;
            return;
        }
        for (i, &at) in UNIT_AT.iter().enumerate() {
            self.units[(ordinal - 1) * 13 + i] = read_u16(e, at);
        }
        self.places[self.seen] = place;
        self.seen += 1;
    }

    /// The name, if a whole one was heard for the 8.3 entry `short`: into
    /// `out`, how long.
    fn name_for(&self, short: &[u8; 11], out: &mut [u8]) -> Option<usize> {
        if self.pieces == 0 || self.seen != self.pieces || self.sum != checksum(short) {
            return None;
        }
        let all = &self.units[..self.pieces * 13];
        let len = all.iter().position(|&u| u == 0).unwrap_or(all.len());
        if len == 0 {
            return None;
        }
        utf8_of(&all[..len], out)
    }
}

/// Every entry of directory `dir`, in order, `.` and `..` included, until
/// `f` says it has found what it wanted: that entry, if it did. Deleted
/// entries, pieces of long names and the volume's label are not entries.
pub(crate) fn walk(disk: &DiskState, dir: u32, mut f: impl FnMut(&Entry) -> bool) -> Result<Option<Entry>, u64> {
    let mut long = Long::new();
    let mut found: Option<Entry> = None;
    sectors(disk, dir, |lba| {
        let mut sector = [0u8; 512];
        sector.copy_from_slice(disk.cached_read_sector(lba).map_err(|_| ERR_IO)?);
        for index in 0..16 {
            let e = &sector[index * 32..index * 32 + 32];
            let place = Place { lba, index };
            match e[0] {
                0x00 => return Ok(false),
                0xE5 => {
                    long.pieces = 0;
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr & 0x3F == ATTR_LONG {
                long.piece(e, place);
                continue;
            }
            if attr & ATTR_VOLUME != 0 {
                long.pieces = 0;
                continue;
            }
            let mut entry = Entry {
                short: [0; 11],
                attr,
                cluster: (read_u16(e, 20) as u32) << 16 | read_u16(e, 26) as u32,
                size: read_u32(e, 28),
                raw: [0; 32],
                place,
                pieces: long.places,
                piece_count: 0,
                name: [0; MAX_NAME],
                name_len: 0,
            };
            entry.short.copy_from_slice(&e[..11]);
            entry.raw.copy_from_slice(e);
            // FAT16's and FAT12's first cluster has no top half.
            if disk.bpb.kind != FatKind::Fat32 {
                entry.cluster &= 0xFFFF;
            }
            match long.name_for(&entry.short, &mut entry.name) {
                Some(len) => {
                    entry.name_len = len;
                    entry.piece_count = long.pieces;
                }
                None => entry.name_len = short_display(&entry.short, e[12], &mut entry.name),
            }
            long.pieces = 0;
            if f(&entry) {
                found = Some(entry);
                return Ok(false);
            }
        }
        Ok(true)
    })?;
    Ok(found)
}

/// The entry of `dir` called `name`, by its long name or its 8.3 one,
/// without regard to case.
pub(crate) fn lookup(disk: &DiskState, dir: u32, name: &[u8]) -> Result<Option<Entry>, u64> {
    let mut short = [0u8; 11];
    let as_short = short_name(name, &mut short).is_some();
    walk(disk, dir, |e| same(e.name(), name) || (as_short && e.short == short))
}

/// The entry of `dir` whose 8.3 name is `short`.
pub(crate) fn by_short(disk: &DiskState, dir: u32, short: &[u8; 11]) -> Result<Option<Entry>, u64> {
    walk(disk, dir, |e| &e.short == short)
}

/// Whether a byte may be in an 8.3 name as it is.
fn short_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"$%'-_@~`!(){}^#&".contains(&b) || b >= 0x80
}

/// `name` as an 8.3 name, if it is one exactly — at most eight and three,
/// nothing an 8.3 name may not hold, and each part all one case: the
/// entry's NT case byte, which says which parts are lower case.
pub fn short_name(name: &[u8], out: &mut [u8; 11]) -> Option<u8> {
    if name == b"." || name == b".." {
        *out = *b"           ";
        out[..name.len()].copy_from_slice(name);
        return Some(0);
    }
    let (base, ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, &[][..]),
    };
    let ok = |part: &[u8], most: usize| {
        !part.is_empty() && part.len() <= most && part.iter().all(|&b| short_char(b) && b < 0x80)
    };
    if !ok(base, 8) || !(ext.is_empty() || ok(ext, 3)) {
        return None;
    }
    let case = |part: &[u8], flag: u8| -> Option<u8> {
        let lower = part.iter().any(|b| b.is_ascii_lowercase());
        let upper = part.iter().any(|b| b.is_ascii_uppercase());
        match (lower, upper) {
            (true, true) => None,
            (true, false) => Some(flag),
            _ => Some(0),
        }
    };
    let flags = case(base, LOWER_BASE)? | case(ext, LOWER_EXT)?;
    *out = *b"           ";
    for (i, &b) in base.iter().enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, &b) in ext.iter().enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    Some(flags)
}

/// Whether `name` may be a name in a FAT directory at all.
pub fn valid_name(name: &[u8]) -> Result<(), u64> {
    if name.is_empty() || name == b"." || name == b".." {
        return Err(ERR_INVALID_PATH);
    }
    // As long as VFAT allows, and as a name is anywhere else.
    let mut units = [0u16; MAX_UNITS];
    if name.len() > 255 {
        return Err(ERR_NAME_TOO_LONG);
    }
    utf16_of(name, &mut units).ok_or(ERR_NAME_TOO_LONG)?;
    // What FAT keeps out of every name, and a name that ends in a dot or a
    // space, which Windows would show as another name.
    if name.iter().any(|&b| b < 0x20 || br#""*/:<>?\|"#.contains(&b)) || matches!(name.last(), Some(b'.' | b' ')) {
        return Err(ERR_INVALID_PATH);
    }
    Ok(())
}

/// An 8.3 name for a long name that is not one, unique in `dir`: its first
/// six characters an 8.3 name may hold, a tilde and a number, and its last
/// extension's first three.
fn make_short(disk: &DiskState, dir: u32, name: &[u8]) -> Result<[u8; 11], u64> {
    let (base, ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot + 1..]),
        _ => (name, &[][..]),
    };
    let fold = |part: &[u8], out: &mut [u8], most: usize| -> usize {
        let mut n = 0;
        for &b in part {
            if n == most {
                break;
            }
            // Spaces and dots go; what an 8.3 name cannot hold is a line.
            if b == b' ' || b == b'.' {
                continue;
            }
            if b >= 0x80 && b & 0xC0 == 0x80 {
                continue; // the rest of a character already written as a line
            }
            out[n] = if b < 0x80 && short_char(b) { b.to_ascii_uppercase() } else { b'_' };
            n += 1;
        }
        n
    };
    let mut stem = [0u8; 8];
    let stem_len = fold(base, &mut stem, 8).max(1);
    if stem[0] == 0 {
        stem[0] = b'_';
    }
    let mut extension = [b' '; 3];
    let ext_len = fold(ext, &mut extension, 3);
    let mut taken = [false; 1000];
    walk(disk, dir, |e| {
        // Which of BASIS~1 to BASIS~999 are taken by a name with this
        // extension and this stem's first characters.
        if e.short[8..11] == extension[..] || ext_len == 0 && e.short[8..11] == *b"   " {
            if let Some(t) = e.short[..8].iter().position(|&b| b == b'~') {
                let digits = &e.short[t + 1..8];
                let end = digits.iter().position(|&b| b == b' ').unwrap_or(digits.len());
                if let Some(n) = core::str::from_utf8(&digits[..end]).ok().and_then(|d| d.parse::<usize>().ok()) {
                    if n < 1000 && e.short[..t.min(stem_len)] == stem[..t.min(stem_len)] {
                        taken[n] = true;
                    }
                }
            }
        }
        false
    })?;
    for n in 1..1000usize {
        if taken[n] {
            continue;
        }
        let tail_len = if n < 10 { 2 } else if n < 100 { 3 } else { 4 };
        let keep = stem_len.min(8 - tail_len);
        let mut short = [b' '; 11];
        short[..keep].copy_from_slice(&stem[..keep]);
        short[keep] = b'~';
        let digits = [b'0' + (n / 100) as u8, b'0' + (n / 10 % 10) as u8, b'0' + (n % 10) as u8];
        short[keep + 1..keep + tail_len].copy_from_slice(&digits[3 - (tail_len - 1)..]);
        short[8..11].copy_from_slice(&extension);
        if by_short(disk, dir, &short)?.is_none() {
            return Ok(short);
        }
    }
    Err(ERR_NO_SPACE)
}

/// Write the thirty-two bytes `entry` at `place`.
fn put(disk: &DiskState, place: Place, entry: &[u8; 32]) -> Result<(), u64> {
    disk.read_sector(place.lba).map_err(|_| ERR_IO)?;
    let data = disk.sector_data_mut();
    data[place.index * 32..place.index * 32 + 32].copy_from_slice(entry);
    disk.write_sector(place.lba).map_err(|_| ERR_IO)?;
    unsafe { SECTOR_CACHE.invalidate(disk.part_lba + place.lba) };
    Ok(())
}

/// Mark the entry at `place` deleted.
fn delete(disk: &DiskState, place: Place) -> Result<(), u64> {
    disk.read_sector(place.lba).map_err(|_| ERR_IO)?;
    let data = disk.sector_data_mut();
    data[place.index * 32] = 0xE5;
    disk.write_sector(place.lba).map_err(|_| ERR_IO)?;
    unsafe { SECTOR_CACHE.invalidate(disk.part_lba + place.lba) };
    Ok(())
}

/// `count` free entries in a row in `dir`, the directory made longer by a
/// cluster where it has none — and never FAT12's or FAT16's root, which is
/// as long as it was made.
fn room(disk: &DiskState, dir: u32, count: usize) -> Result<[Place; MAX_PIECES + 1], u64> {
    let mut run = [Place::default(); MAX_PIECES + 1];
    let mut len = 0;
    let fixed = dir == 0 && disk.bpb.kind != FatKind::Fat32;
    let mut done = false;
    sectors(disk, dir, |lba| {
        let mut sector = [0u8; 512];
        sector.copy_from_slice(disk.cached_read_sector(lba).map_err(|_| ERR_IO)?);
        for index in 0..16 {
            let first = sector[index * 32];
            if first == 0x00 || first == 0xE5 {
                run[len] = Place { lba, index };
                len += 1;
                if len == count {
                    done = true;
                    return Ok(false);
                }
            } else {
                len = 0;
            }
        }
        Ok(true)
    })?;
    if done {
        return Ok(run);
    }
    if fixed {
        return Err(ERR_NO_SPACE);
    }
    // To the end of the chain, and one more cluster, zeroed: every entry in
    // it free, and the run goes on into it.
    let mut cluster = if dir == 0 { disk.bpb.root_cluster } else { dir };
    for _ in 0..disk.cluster_count() + 2 {
        match disk.fat_next(cluster) {
            Some(next) => cluster = next,
            None => break,
        }
    }
    while len < count {
        let added = disk.fat_extend(cluster).map_err(|_| ERR_NO_SPACE)?;
        disk.zero_cluster(added).map_err(|_| ERR_IO)?;
        let start = disk.cluster_start_lba(added);
        'fill: for s in 0..disk.bpb.sectors_per_cluster {
            for index in 0..16 {
                run[len] = Place { lba: start + s, index };
                len += 1;
                if len == count {
                    break 'fill;
                }
            }
        }
        cluster = added;
    }
    Ok(run)
}

/// Make an entry called `name` in `dir`: an 8.3 entry from `template` — its
/// attributes, times, first cluster and length — with the 8.3 name `name`
/// is, or one made for it and the long name before it. The 8.3 name.
pub(crate) fn create(disk: &DiskState, dir: u32, name: &[u8], template: &[u8; 32]) -> Result<[u8; 11], u64> {
    valid_name(name)?;
    if lookup(disk, dir, name)?.is_some() {
        return Err(ERR_EXISTS);
    }
    let mut short = [0u8; 11];
    let (case, long) = match short_name(name, &mut short) {
        Some(case) if by_short(disk, dir, &short)?.is_none() => (case, false),
        _ => {
            short = make_short(disk, dir, name)?;
            (0, true)
        }
    };
    let mut units = [0u16; MAX_UNITS];
    let unit_count = utf16_of(name, &mut units).ok_or(ERR_NAME_TOO_LONG)?;
    let pieces = if long { unit_count.div_ceil(13) } else { 0 };
    let places = room(disk, dir, pieces + 1)?;

    // The long name's pieces, the last first, then the 8.3 entry.
    let sum = checksum(&short);
    for p in 0..pieces {
        let ordinal = pieces - p;
        let mut e = [0u8; 32];
        e[0] = ordinal as u8 | if p == 0 { 0x40 } else { 0 };
        e[11] = ATTR_LONG;
        e[13] = sum;
        for (i, &at) in UNIT_AT.iter().enumerate() {
            let k = (ordinal - 1) * 13 + i;
            // After the name, one nought, and then all ones.
            let u = if k < unit_count { units[k] } else if k == unit_count { 0 } else { 0xFFFF };
            e[at..at + 2].copy_from_slice(&u.to_le_bytes());
        }
        put(disk, places[p], &e)?;
    }
    let mut e = *template;
    e[..11].copy_from_slice(&short);
    e[12] = case;
    put(disk, places[pieces], &e)?;
    Ok(short)
}

/// Take `entry` out of its directory, its long name with it.
pub(crate) fn remove(disk: &DiskState, entry: &Entry) -> Result<(), u64> {
    for p in &entry.pieces[..entry.piece_count] {
        delete(disk, *p)?;
    }
    delete(disk, entry.place)
}

/// Write `entry`'s 8.3 entry back as `raw` says, its name and place kept.
pub(crate) fn rewrite(disk: &DiskState, entry: &Entry, raw: &[u8; 32]) -> Result<(), u64> {
    let mut e = *raw;
    e[..11].copy_from_slice(&entry.short);
    put(disk, entry.place, &e)
}
