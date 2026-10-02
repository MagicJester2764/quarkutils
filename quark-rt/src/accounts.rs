//! Who there is: `/etc/passwd`, `/etc/shadow`, `/etc/group` and `/etc/rights`,
//! read and rewritten as text.
//!
//! The files are Unix's, a record a line and a field between colons, because
//! every program written for Unix reads them that way: a C library turns a
//! file's owner into a name by reading `/etc/passwd`, and `crypt` checks a
//! password against what `/etc/shadow` holds. `/etc/rights` is this
//! system's own: what an account's sessions may hold beyond being
//! themselves. Nothing here opens a file or allocates; it is handed the text.

use crate::vfs;

/// The longest name an account or a group may have.
pub const MAX_NAME: usize = 32;
/// How many groups a task may be in besides its own.
pub const MAX_GROUPS: usize = 16;
/// The user nobody is: who a caller that has gone is taken to be.
pub const NOBODY: u32 = 65534;

/// A line of `/etc/passwd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct User<'a> {
    pub name: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    /// The comment field: a full name, usually.
    pub about: &'a [u8],
    pub home: &'a [u8],
    pub shell: &'a [u8],
}

/// A line of `/etc/group`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group<'a> {
    pub name: &'a [u8],
    pub gid: u32,
    /// The members, with commas between them.
    pub members: &'a [u8],
}

fn number(field: &[u8]) -> Option<u32> {
    if field.is_empty() {
        return None;
    }
    field.iter().try_fold(0u32, |n, &c| {
        c.is_ascii_digit().then_some(())?;
        n.checked_mul(10)?.checked_add((c - b'0') as u32)
    })
}

/// The lines of a file that are records: not blank, not a comment.
fn records(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty() && l[0] != b'#')
}

/// A name that may be an account's or a group's: what Unix allows, which is
/// also what is safe between colons and in a path.
pub fn name_ok(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && (name[0].is_ascii_lowercase() || name[0] == b'_')
        && name.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// A field that can sit between colons on one line.
pub fn field_ok(field: &[u8]) -> bool {
    field.iter().all(|&c| c != b':' && c != b'\n' && c != b'\r' && c != 0)
}

fn user_of(line: &[u8]) -> Option<User<'_>> {
    let mut fields = [&[][..]; 7];
    let mut n = 0;
    for field in line.split(|&b| b == b':') {
        if n == fields.len() {
            return None;
        }
        fields[n] = field;
        n += 1;
    }
    // Unix's seven fields, or the five this system began with.
    let (uid, gid, about, home, shell) = match n {
        7 => (fields[2], fields[3], fields[4], fields[5], fields[6]),
        5 => (fields[1], fields[2], &[][..], fields[3], fields[4]),
        _ => return None,
    };
    Some(User { name: fields[0], uid: number(uid)?, gid: number(gid)?, about, home, shell })
}

/// Every account in the text of `/etc/passwd`.
pub fn users(passwd: &[u8]) -> impl Iterator<Item = User<'_>> {
    records(passwd).filter_map(user_of)
}

pub fn user_named<'a>(passwd: &'a [u8], name: &[u8]) -> Option<User<'a>> {
    users(passwd).find(|u| u.name == name)
}

pub fn user_numbered(passwd: &[u8], uid: u32) -> Option<User<'_>> {
    users(passwd).find(|u| u.uid == uid)
}

/// The first id at or above `from` that no account has: where a new user
/// goes.
pub fn free_uid(passwd: &[u8], from: u32) -> u32 {
    let mut uid = from;
    while users(passwd).any(|u| u.uid == uid) && uid < NOBODY - 1 {
        uid += 1;
    }
    uid
}

/// Every group in the text of `/etc/group`.
pub fn groups(group: &[u8]) -> impl Iterator<Item = Group<'_>> {
    records(group).filter_map(|line| {
        let mut f = line.splitn(4, |&b| b == b':');
        let name = f.next()?;
        let _password = f.next()?;
        let gid = number(f.next()?)?;
        Some(Group { name, gid, members: f.next().unwrap_or(b"") })
    })
}

pub fn group_named<'a>(group: &'a [u8], name: &[u8]) -> Option<Group<'a>> {
    groups(group).find(|g| g.name == name)
}

pub fn free_gid(group: &[u8], from: u32) -> u32 {
    let mut gid = from;
    while groups(group).any(|g| g.gid == gid) && gid < NOBODY - 1 {
        gid += 1;
    }
    gid
}

/// Whether `user` is among a group's members.
pub fn is_member(g: &Group, user: &[u8]) -> bool {
    g.members.split(|&b| b == b',').any(|m| m == user)
}

/// The groups `user` is in besides `primary`, into `out`. How many.
pub fn groups_of(group: &[u8], user: &[u8], primary: u32, out: &mut [u32; MAX_GROUPS]) -> usize {
    let mut n = 0;
    for g in groups(group) {
        if g.gid != primary && is_member(&g, user) && n < MAX_GROUPS && !out[..n].contains(&g.gid) {
            out[n] = g.gid;
            n += 1;
        }
    }
    n
}

/// What `/etc/shadow` holds for `name`: the hash field, which is empty for
/// "no password", `!` or `*` and more for "locked", and a `$6$` hash
/// otherwise. `None` if the account has no line there.
pub fn hash_of<'a>(shadow: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    records(shadow).find_map(|line| {
        let mut f = line.splitn(3, |&b| b == b':');
        (f.next()? == name).then(|| f.next().unwrap_or(b""))
    })
}

/// Whether a hash field means nobody can log in with a password.
pub fn locked(hash: &[u8]) -> bool {
    matches!(hash.first(), Some(b'!') | Some(b'*'))
}

/// What an account's sessions may hold beyond being themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    /// Turn the machine off and start it again.
    pub const POWER: u32 = 1;
    /// End or signal any program, whoever's.
    pub const TASKS: u32 = 2;
    /// Become another user with one's own password.
    pub const BECOME: u32 = 4;
    /// Everything a session can hold, the right to say who a task is among
    /// it: what user 0 has where nothing says otherwise.
    pub const ALL: u32 = 0x8000_0000 | 7;

    pub fn has(self, right: u32) -> bool {
        self.0 & right == right
    }

    pub fn named(word: &[u8]) -> Option<u32> {
        Some(match word {
            b"power" => Self::POWER,
            b"tasks" => Self::TASKS,
            b"become" => Self::BECOME,
            b"all" => Self::ALL,
            _ => return None,
        })
    }
}

/// The names of the rights, in the order they are written.
pub const RIGHT_NAMES: [(&[u8], u32); 4] =
    [(b"all", Rights::ALL), (b"power", Rights::POWER), (b"tasks", Rights::TASKS), (b"become", Rights::BECOME)];

/// What `/etc/rights` gives `name`, whose id is `uid`.
///
/// A line is a name and the rights after it, with spaces between. An
/// account no line names — and every account, where there is no file at all
/// (`None`) — has what Unix gives it: everything for user 0 and nothing for
/// anybody else. A line for user 0 can therefore take rights away from it,
/// and has to be written on purpose to do so.
pub fn rights_of(rights: Option<&[u8]>, name: &[u8], uid: u32) -> Rights {
    let unix = if uid == 0 { Rights(Rights::ALL) } else { Rights::NONE };
    let Some(text) = rights else { return unix };
    for line in records(text) {
        let mut words = line.split(|&b| b == b' ' || b == b'\t').filter(|w| !w.is_empty());
        if words.next() != Some(name) {
            continue;
        }
        return Rights(words.filter_map(Rights::named).fold(0, |all, r| all | r));
    }
    unix
}

/// `text` with the record whose first field is `name` replaced by `line`
/// (or taken out, for `None`), or with `line` added at the end if no record
/// has that name. Written into `out`; how long it is, or `None` if it does
/// not fit. `separator` is what ends the first field: a colon, or a space
/// for `/etc/rights`.
pub fn with_record(text: &[u8], name: &[u8], separator: u8, line: Option<&[u8]>, out: &mut [u8]) -> Option<usize> {
    let mut len = 0;
    let mut put = |bytes: &[u8], out: &mut [u8]| -> Option<()> {
        out.get_mut(len..len + bytes.len())?.copy_from_slice(bytes);
        len += bytes.len();
        Some(())
    };
    let mut found = false;
    for record in text.split(|&b| b == b'\n') {
        let first = record.split(|&b| b == separator || b == b'\t').next().unwrap_or(b"");
        let is_it = !record.is_empty() && record[0] != b'#' && first == name;
        if is_it {
            if !found {
                if let Some(line) = line {
                    put(line, out)?;
                    put(b"\n", out)?;
                }
            }
            found = true;
        } else if !record.is_empty() {
            put(record, out)?;
            put(b"\n", out)?;
        }
    }
    if !found {
        if let Some(line) = line {
            put(line, out)?;
            put(b"\n", out)?;
        }
    }
    Some(len)
}

/// Fields with colons between them, as a line, into `out`.
pub fn line<'a>(fields: &[&[u8]], out: &'a mut [u8]) -> Option<&'a [u8]> {
    let mut len = 0;
    for (i, field) in fields.iter().enumerate() {
        if !field_ok(field) {
            return None;
        }
        if i > 0 {
            *out.get_mut(len)? = b':';
            len += 1;
        }
        out.get_mut(len..len + field.len())?.copy_from_slice(field);
        len += field.len();
    }
    Some(&out[..len])
}

/// A number as its digits, into `out`.
pub fn digits(mut n: u32, out: &mut [u8; 10]) -> &[u8] {
    let mut at = out.len();
    loop {
        at -= 1;
        out[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &out[at..]
}

/// `group` — the text of `/etc/group` — with `user` put in, or taken out
/// of, the members of the group called `name`. Written into `out`; how long
/// it is. `None` if there is no such group or the result does not fit.
/// Putting in somebody who is there, or taking out somebody who is not,
/// changes nothing and is not an error.
pub fn with_member(group: &[u8], name: &[u8], user: &[u8], present: bool, out: &mut [u8]) -> Option<usize> {
    let g = group_named(group, name)?;
    let mut members = [0u8; 512];
    let mut len = 0;
    let mut put = |m: &[u8], members: &mut [u8; 512]| -> Option<()> {
        if len > 0 {
            *members.get_mut(len)? = b',';
            len += 1;
        }
        members.get_mut(len..len + m.len())?.copy_from_slice(m);
        len += m.len();
        Some(())
    };
    for m in g.members.split(|&b| b == b',').filter(|m| !m.is_empty() && *m != user) {
        put(m, &mut members)?;
    }
    if present {
        put(user, &mut members)?;
    }
    let mut gid = [0u8; 10];
    let mut new = [0u8; 640];
    let new = line(&[g.name, b"x", digits(g.gid, &mut gid), &members[..len]], &mut new)?;
    with_record(group, name, b':', Some(new), out)
}

/// `group` with `user` taken out of every group's members.
pub fn without_member(group: &[u8], user: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut len = 0;
    for record in group.split(|&b| b == b'\n') {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(4, |&b| b == b':');
        let (name, pw, gid, members) = (fields.next(), fields.next(), fields.next(), fields.next());
        let (Some(name), Some(pw), Some(gid), Some(members)) = (name, pw, gid, members) else {
            // Not a group's line: kept as it is.
            out.get_mut(len..len + record.len())?.copy_from_slice(record);
            len += record.len();
            *out.get_mut(len)? = b'\n';
            len += 1;
            continue;
        };
        for (i, part) in [name, pw, gid].iter().enumerate() {
            if i > 0 {
                *out.get_mut(len)? = b':';
                len += 1;
            }
            out.get_mut(len..len + part.len())?.copy_from_slice(part);
            len += part.len();
        }
        *out.get_mut(len)? = b':';
        len += 1;
        let mut first = true;
        for m in members.split(|&b| b == b',').filter(|m| !m.is_empty() && *m != user) {
            if !first {
                *out.get_mut(len)? = b',';
                len += 1;
            }
            out.get_mut(len..len + m.len())?.copy_from_slice(m);
            len += m.len();
            first = false;
        }
        *out.get_mut(len)? = b'\n';
        len += 1;
    }
    Some(len)
}

// ---------------------------------------------------------------------------
// The files themselves
// ---------------------------------------------------------------------------
//
// Everything above is handed text. What follows reads and writes it: the
// account files of this system, or — `root` not empty — of one mounted at
// `root`, which is how an installer makes the first user of a system that is
// not running.

/// `ROOT/etc/NAME`, into `out`. On a FAT root the name is in capitals.
fn path_of<'a>(root: &[u8], name: &[u8], capitals: bool, suffix: &[u8], out: &'a mut [u8; 256]) -> Option<&'a [u8]> {
    let root = root.strip_suffix(b"/").unwrap_or(root);
    let mut len = 0;
    for part in [root, b"/etc/", name, suffix] {
        out.get_mut(len..len + part.len())?.copy_from_slice(part);
        len += part.len();
    }
    if capitals {
        let from = len - name.len() - suffix.len();
        out[from..from + name.len()].make_ascii_uppercase();
    }
    Some(&out[..len])
}

/// Read `ROOT/etc/NAME` whole into `buf`. `None` if it is not there, is not
/// a file, or does not fit.
pub fn read<'a>(vfs_tid: usize, root: &[u8], name: &[u8], buf: &'a mut [u8]) -> Option<&'a [u8]> {
    let mut path = [0u8; 256];
    let (handle, _, is_dir) = vfs::open(vfs_tid, path_of(root, name, false, b"", &mut path)?)
        .or_else(|_| vfs::open(vfs_tid, path_of(root, name, true, b"", &mut path).unwrap_or(b"")))
        .ok()?;
    let mut len = 0;
    let whole = !is_dir
        && loop {
            if len == buf.len() {
                // Full, and there may be more: not the whole of it.
                let mut one = [0u8; 1];
                break !matches!(vfs::read(vfs_tid, handle, &mut one, len as u32), Ok(n) if n > 0);
            }
            match vfs::read(vfs_tid, handle, &mut buf[len..], len as u32) {
                Ok(0) => break true,
                Ok(n) => len += n as usize,
                Err(_) => break false,
            }
        };
    let _ = vfs::close(vfs_tid, handle);
    whole.then_some(&buf[..len])
}

/// Write `ROOT/etc/NAME` whole, with `mode`: beside the old one and then
/// moved over it, so that a machine that stops half way has one whole file
/// or the other. Where a file cannot be moved — FAT — it is written in
/// place.
pub fn write(vfs_tid: usize, root: &[u8], name: &[u8], bytes: &[u8], mode: u32) -> Result<(), u64> {
    let mut path = [0u8; 256];
    let mut beside = [0u8; 256];
    // The spelling the file already has, if it is there.
    let capitals = {
        let lower = path_of(root, name, false, b"", &mut path).ok_or(vfs::ERR_NAME_TOO_LONG)?;
        match vfs::open(vfs_tid, lower) {
            Ok((h, _, _)) => {
                let _ = vfs::close(vfs_tid, h);
                false
            }
            Err(_) => {
                let upper = path_of(root, name, true, b"", &mut path).ok_or(vfs::ERR_NAME_TOO_LONG)?;
                match vfs::open(vfs_tid, upper) {
                    Ok((h, _, _)) => {
                        let _ = vfs::close(vfs_tid, h);
                        true
                    }
                    Err(_) => false,
                }
            }
        }
    };
    let path = path_of(root, name, capitals, b"", &mut path).ok_or(vfs::ERR_NAME_TOO_LONG)?;
    let beside = path_of(root, name, capitals, b".new", &mut beside).ok_or(vfs::ERR_NAME_TOO_LONG)?;

    let put = |to: &[u8]| -> Result<(), u64> {
        let handle = vfs::open_with(vfs_tid, to, vfs::OPEN_CREATE | vfs::OPEN_TRUNCATE)?.handle;
        let mut at = 0;
        let done = loop {
            if at == bytes.len() {
                break Ok(());
            }
            match vfs::write(vfs_tid, handle, &bytes[at..], at as u32) {
                Ok(0) => break Err(vfs::ERR_NO_SPACE),
                Ok(n) => at += n as usize,
                Err(code) => break Err(code),
            }
        };
        let _ = vfs::close(vfs_tid, handle);
        done?;
        // The mode before the name: a shadow file is never readable under
        // the name anybody looks for it by.
        match vfs::set_attr(vfs_tid, to, vfs::ATTR_MODE, mode, 0, 0, 0, 0) {
            Ok(()) | Err(vfs::ERR_NOT_SUPPORTED) => Ok(()),
            Err(code) => Err(code),
        }
    };
    match put(beside).and_then(|()| vfs::rename(vfs_tid, beside, path)) {
        Ok(()) => {}
        Err(vfs::ERR_NOT_SUPPORTED) => {
            let _ = vfs::unlink(vfs_tid, beside);
            put(path)?;
        }
        Err(code) => {
            let _ = vfs::unlink(vfs_tid, beside);
            return Err(code);
        }
    }
    let _ = vfs::sync(vfs_tid);
    Ok(())
}

/// Why an account could not be made, changed or removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// The name is not one an account or a group may have.
    BadName,
    /// There is one of that name, or with that number, already.
    Taken,
    /// There is no such user, or no such group.
    Missing,
    /// A file is too long for this to rewrite, or a line for the file.
    TooLong,
    /// A file could not be read or written: the file server's code.
    File(u64),
}

impl Trouble {
    pub fn words(&self) -> &'static str {
        match self {
            Trouble::BadName => "a name is lower-case letters, digits, - and _, and begins with a letter",
            Trouble::Taken => "that is taken already",
            Trouble::Missing => "there is nothing of that name",
            Trouble::TooLong => "the account files have no room for it",
            Trouble::File(vfs::ERR_PERMISSION) => "only root changes the accounts",
            Trouble::File(vfs::ERR_READ_ONLY) => "the accounts are on a filesystem that cannot be written",
            Trouble::File(_) => "the account files could not be read or written",
        }
    }
}

/// Somebody to add.
pub struct NewUser<'a> {
    pub name: &'a [u8],
    /// A number for it, or the first free one from 1000.
    pub uid: Option<u32>,
    /// The group it is in: an existing one's name, or a new group of its own
    /// name.
    pub group: Option<&'a [u8]>,
    pub about: &'a [u8],
    /// Where it lives: `/home/NAME` if not said.
    pub home: Option<&'a [u8]>,
    /// Its shell: root's, if not said.
    pub shell: Option<&'a [u8]>,
    /// Whether to make the home directory.
    pub make_home: bool,
}

/// Room to work in: the account files are read here and rewritten here.
pub const WORK: usize = 5 * 16 * 1024;

/// Add a user to the system at `root`. The account is locked — nobody logs
/// in to it — until it is given a password. Its ids.
pub fn add_user(vfs_tid: usize, root: &[u8], new: &NewUser, work: &mut [u8]) -> Result<(u32, u32), Trouble> {
    if !name_ok(new.name) || !field_ok(new.about) {
        return Err(Trouble::BadName);
    }
    if work.len() < WORK {
        return Err(Trouble::TooLong);
    }
    let (passwd_buf, rest) = work.split_at_mut(WORK / 5);
    let (group_buf, rest) = rest.split_at_mut(WORK / 5);
    let (shadow_buf, rest) = rest.split_at_mut(WORK / 5);
    let (out, out2) = rest.split_at_mut(WORK / 5);
    let passwd = read(vfs_tid, root, b"passwd", passwd_buf).ok_or(Trouble::File(vfs::ERR_NOT_FOUND))?;
    let group = read(vfs_tid, root, b"group", group_buf).unwrap_or(b"");
    let shadow = read(vfs_tid, root, b"shadow", shadow_buf).unwrap_or(b"");

    if user_named(passwd, new.name).is_some() {
        return Err(Trouble::Taken);
    }
    let uid = match new.uid {
        Some(uid) if user_numbered(passwd, uid).is_some() => return Err(Trouble::Taken),
        Some(uid) => uid,
        None => free_uid(passwd, 1000),
    };

    // Its group: one named, one of its own name that is there already, or a
    // new one — with the user's own number if that is free, as Unix does.
    let mut digits_buf = [0u8; 10];
    let mut line_buf = [0u8; 256];
    let (gid, group_len) = match new.group {
        Some(name) => (group_named(group, name).ok_or(Trouble::Missing)?.gid, None),
        None => match group_named(group, new.name) {
            Some(g) => (g.gid, None),
            None => {
                let gid = if groups(group).any(|g| g.gid == uid) { free_gid(group, 1000) } else { uid };
                let made = line(&[new.name, b"x", digits(gid, &mut digits_buf), b""], &mut line_buf)
                    .ok_or(Trouble::TooLong)?;
                (gid, Some(with_record(group, new.name, b':', Some(made), out).ok_or(Trouble::TooLong)?))
            }
        },
    };
    if let Some(len) = group_len {
        write(vfs_tid, root, b"group", &out[..len], 0o644).map_err(Trouble::File)?;
    }

    // Locked, until somebody gives it a password.
    let mut day = [0u8; 10];
    let day = digits((crate::syscall::unix_time() / 86400) as u32, &mut day);
    let locked = line(&[new.name, b"!", day, b"", b"", b"", b"", b"", b""], &mut line_buf).ok_or(Trouble::TooLong)?;
    let len = with_record(shadow, new.name, b':', Some(locked), out).ok_or(Trouble::TooLong)?;
    write(vfs_tid, root, b"shadow", &out[..len], 0o600).map_err(Trouble::File)?;

    // And the account itself, last: it is there when this is.
    let mut home_buf = [0u8; 64];
    let home = match new.home {
        Some(home) => home,
        None => {
            let n = new.name.len();
            home_buf[..6].copy_from_slice(b"/home/");
            home_buf[6..6 + n].copy_from_slice(new.name);
            &home_buf[..6 + n]
        }
    };
    let shell = new.shell.or_else(|| user_numbered(passwd, 0).map(|r| r.shell)).unwrap_or(b"/bin/sh");
    let mut gid_buf = [0u8; 10];
    let entry = line(
        &[new.name, b"x", digits(uid, &mut digits_buf), digits(gid, &mut gid_buf), new.about, home, shell],
        &mut line_buf,
    )
    .ok_or(Trouble::TooLong)?;
    let len = with_record(passwd, new.name, b':', Some(entry), out2).ok_or(Trouble::TooLong)?;
    write(vfs_tid, root, b"passwd", &out2[..len], 0o644).map_err(Trouble::File)?;

    if new.make_home {
        let mut path = [0u8; 256];
        let root = root.strip_suffix(b"/").unwrap_or(root);
        let n = root.len() + home.len();
        if n <= path.len() {
            path[..root.len()].copy_from_slice(root);
            path[root.len()..n].copy_from_slice(home);
            match vfs::mkdir(vfs_tid, &path[..n]) {
                Ok(()) | Err(vfs::ERR_EXISTS) => {}
                Err(code) => return Err(Trouble::File(code)),
            }
            // Its own, and nobody else's to look in.
            let which = vfs::ATTR_MODE | vfs::ATTR_UID | vfs::ATTR_GID;
            vfs::set_attr(vfs_tid, &path[..n], which, 0o700, uid, gid, 0, 0).map_err(Trouble::File)?;
        }
    }
    Ok((uid, gid))
}

/// Take a user away: its line in each file, and its name out of every
/// group. A group of its own name that nobody else is in goes with it. Its
/// home is left: what is in it is somebody's work.
pub fn remove_user(vfs_tid: usize, root: &[u8], name: &[u8], work: &mut [u8]) -> Result<(), Trouble> {
    if work.len() < WORK {
        return Err(Trouble::TooLong);
    }
    let (passwd_buf, rest) = work.split_at_mut(WORK / 5);
    let (group_buf, rest) = rest.split_at_mut(WORK / 5);
    let (shadow_buf, rest) = rest.split_at_mut(WORK / 5);
    let (out, out2) = rest.split_at_mut(WORK / 5);
    let passwd = read(vfs_tid, root, b"passwd", passwd_buf).ok_or(Trouble::File(vfs::ERR_NOT_FOUND))?;
    let user = user_named(passwd, name).ok_or(Trouble::Missing)?;
    if user.uid == 0 {
        // A system with nobody who may put it right is not one to make.
        return Err(Trouble::Taken);
    }

    let len = with_record(passwd, name, b':', None, out).ok_or(Trouble::TooLong)?;
    write(vfs_tid, root, b"passwd", &out[..len], 0o644).map_err(Trouble::File)?;

    if let Some(shadow) = read(vfs_tid, root, b"shadow", shadow_buf) {
        let len = with_record(shadow, name, b':', None, out).ok_or(Trouble::TooLong)?;
        write(vfs_tid, root, b"shadow", &out[..len], 0o600).map_err(Trouble::File)?;
    }
    if let Some(group) = read(vfs_tid, root, b"group", group_buf) {
        let len = without_member(group, name, out).ok_or(Trouble::TooLong)?;
        // A group of its own name, with nobody in it and nobody else's.
        let shared = users(passwd).any(|u| u.name != name && u.gid == user.gid);
        let own = !shared && group_named(&out[..len], name).is_some_and(|g| g.members.is_empty() && g.gid == user.gid);
        let len2 = if own {
            with_record(&out[..len], name, b':', None, out2).ok_or(Trouble::TooLong)?
        } else {
            out2[..len].copy_from_slice(&out[..len]);
            len
        };
        write(vfs_tid, root, b"group", &out2[..len2], 0o644).map_err(Trouble::File)?;
    }
    Ok(())
}

/// Add a group. Its number.
pub fn add_group(vfs_tid: usize, root: &[u8], name: &[u8], gid: Option<u32>, work: &mut [u8]) -> Result<u32, Trouble> {
    if !name_ok(name) {
        return Err(Trouble::BadName);
    }
    if work.len() < WORK {
        return Err(Trouble::TooLong);
    }
    let (group_buf, out) = work.split_at_mut(WORK / 5);
    let group = read(vfs_tid, root, b"group", group_buf).unwrap_or(b"");
    if group_named(group, name).is_some() {
        return Err(Trouble::Taken);
    }
    let gid = match gid {
        Some(gid) if groups(group).any(|g| g.gid == gid) => return Err(Trouble::Taken),
        Some(gid) => gid,
        None => free_gid(group, 1000),
    };
    let mut digits_buf = [0u8; 10];
    let mut line_buf = [0u8; 128];
    let made = line(&[name, b"x", digits(gid, &mut digits_buf), b""], &mut line_buf).ok_or(Trouble::TooLong)?;
    let len = with_record(group, name, b':', Some(made), out).ok_or(Trouble::TooLong)?;
    write(vfs_tid, root, b"group", &out[..len], 0o644).map_err(Trouble::File)?;
    Ok(gid)
}

/// Put a user in a group, or take it out.
pub fn set_member(
    vfs_tid: usize,
    root: &[u8],
    group_name: &[u8],
    user: &[u8],
    present: bool,
    work: &mut [u8],
) -> Result<(), Trouble> {
    if work.len() < WORK {
        return Err(Trouble::TooLong);
    }
    let (group_buf, out) = work.split_at_mut(WORK / 5);
    let group = read(vfs_tid, root, b"group", group_buf).ok_or(Trouble::Missing)?;
    if group_named(group, group_name).is_none() {
        return Err(Trouble::Missing);
    }
    let len = with_member(group, group_name, user, present, out).ok_or(Trouble::TooLong)?;
    write(vfs_tid, root, b"group", &out[..len], 0o644).map_err(Trouble::File)
}
