/// Parser for /etc/passwd files.
///
/// One entry a line, in either of two shapes. Unix's seven fields —
/// `name:password:uid:gid:comment:home:shell` — which is what every C library
/// reads to turn a file's owner into a name; or the five this started with,
/// `name:uid:gid:home:shell`. Which one a line is, is said by how many fields
/// it has. The password field is not looked at: nothing here asks for one.

pub struct PasswdEntry {
    pub username: [u8; 32],
    pub username_len: usize,
    pub uid: u32,
    pub gid: u32,
    pub home: [u8; 64],
    pub home_len: usize,
    pub shell: [u8; 64],
    pub shell_len: usize,
}

impl PasswdEntry {
    pub fn username(&self) -> &[u8] {
        &self.username[..self.username_len]
    }

    pub fn home(&self) -> &[u8] {
        &self.home[..self.home_len]
    }

    pub fn shell(&self) -> &[u8] {
        &self.shell[..self.shell_len]
    }
}

/// Look up a user by name in passwd file data.
pub fn lookup_user(data: &[u8], username: &[u8]) -> Option<PasswdEntry> {
    let mut pos = 0;
    while pos < data.len() {
        // Find end of line
        let line_end = data[pos..].iter().position(|&b| b == b'\n')
            .map_or(data.len(), |p| pos + p);
        let line = &data[pos..line_end];
        pos = line_end + 1;

        if line.is_empty() {
            continue;
        }

        if let Some(entry) = parse_line(line) {
            if entry.username_len == username.len()
                && entry.username[..entry.username_len] == *username
            {
                return Some(entry);
            }
        }
    }
    None
}

fn parse_line(line: &[u8]) -> Option<PasswdEntry> {
    let mut fields = [&[][..]; 7];
    let mut field_count = 0;
    for field in line.split(|&b| b == b':') {
        if field_count == fields.len() {
            return None; // more than a passwd line has
        }
        fields[field_count] = field;
        field_count += 1;
    }
    // Where each thing is, in whichever shape this is.
    let (uid, gid, home, shell) = match field_count {
        7 => (2, 3, 5, 6),
        5 => (1, 2, 3, 4),
        _ => return None,
    };
    let fields = [fields[0], fields[uid], fields[gid], fields[home], fields[shell]];

    let uid = parse_u32(fields[1])?;
    let gid = parse_u32(fields[2])?;

    let mut entry = PasswdEntry {
        username: [0; 32],
        username_len: 0,
        uid,
        gid,
        home: [0; 64],
        home_len: 0,
        shell: [0; 64],
        shell_len: 0,
    };

    let ulen = fields[0].len().min(32);
    entry.username[..ulen].copy_from_slice(&fields[0][..ulen]);
    entry.username_len = ulen;

    let hlen = fields[3].len().min(64);
    entry.home[..hlen].copy_from_slice(&fields[3][..hlen]);
    entry.home_len = hlen;

    let slen = fields[4].len().min(64);
    entry.shell[..slen].copy_from_slice(&fields[4][..slen]);
    entry.shell_len = slen;

    Some(entry)
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut val: u32 = 0;
    for &b in s {
        if b < b'0' || b > b'9' {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(val)
}
