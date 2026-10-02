//! Passwords, as a Unix keeps them: SHA-512 and the `$6$` scheme built on it.
//!
//! `/etc/shadow` holds `$6$salt$hash` — the scheme every C library's `crypt`
//! speaks (Drepper's "Unix crypt using SHA-256 and SHA-512"), so that a
//! password set here is one a program written for Unix can check, and the
//! other way about. Nothing here allocates: a password is at most
//! [`MAX_PASSWORD`] bytes, and one longer is refused rather than cut.

/// The longest password taken. The scheme has no limit; its working copy of
/// the password needs somewhere to be.
pub const MAX_PASSWORD: usize = 128;
/// The longest salt the scheme uses; more is ignored, as `crypt` ignores it.
pub const MAX_SALT: usize = 16;
/// How many rounds when a hash does not say.
pub const DEFAULT_ROUNDS: u32 = 5000;
/// `$6$` + `rounds=999999999$` + salt + `$` + 86 characters.
pub const MAX_HASH: usize = 3 + 17 + MAX_SALT + 1 + 86;

const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// SHA-512, a piece at a time.
pub struct Sha512 {
    state: [u64; 8],
    block: [u8; 128],
    held: usize,
    total: u64,
}

impl Sha512 {
    pub const fn new() -> Sha512 {
        Sha512 {
            state: [
                0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
                0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
            ],
            block: [0; 128],
            held: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let take = (128 - self.held).min(data.len());
            self.block[self.held..self.held + take].copy_from_slice(&data[..take]);
            self.held += take;
            data = &data[take..];
            if self.held == 128 {
                let block = self.block;
                self.compress(&block);
                self.held = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 64] {
        let bits = (self.total as u128) * 8;
        self.block[self.held] = 0x80;
        self.block[self.held + 1..].fill(0);
        if self.held + 1 > 112 {
            let block = self.block;
            self.compress(&block);
            self.block = [0; 128];
        }
        self.block[112..].copy_from_slice(&bits.to_be_bytes());
        let block = self.block;
        self.compress(&block);
        let mut out = [0u8; 64];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&block[i * 8..i * 8 + 8]);
            *word = u64::from_be_bytes(bytes);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// SHA-512 of one piece.
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(data);
    h.finish()
}

const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The order the digest's bytes are written out in, three at a time.
const ORDER: [[usize; 3]; 21] = [
    [0, 21, 42], [22, 43, 1], [44, 2, 23], [3, 24, 45], [25, 46, 4], [47, 5, 26], [6, 27, 48],
    [28, 49, 7], [50, 8, 29], [9, 30, 51], [31, 52, 10], [53, 11, 32], [12, 33, 54],
    [34, 55, 13], [56, 14, 35], [15, 36, 57], [37, 58, 16], [59, 17, 38], [18, 39, 60],
    [40, 61, 19], [62, 20, 41],
];

/// What a `$6$` setting says: how many rounds, whether it said so, and the
/// salt.
fn setting(stored: &[u8]) -> Option<(u32, bool, &[u8])> {
    let rest = stored.strip_prefix(b"$6$")?;
    let (rounds, said, rest) = match rest.strip_prefix(b"rounds=") {
        Some(after) => {
            let end = after.iter().position(|&b| b == b'$')?;
            let mut n: u64 = 0;
            for &c in &after[..end] {
                if !c.is_ascii_digit() {
                    return None;
                }
                n = (n * 10 + (c - b'0') as u64).min(u32::MAX as u64);
            }
            (n.clamp(1000, 999_999_999) as u32, true, &after[end + 1..])
        }
        None => (DEFAULT_ROUNDS, false, rest),
    };
    let end = rest.iter().position(|&b| b == b'$').unwrap_or(rest.len());
    Some((rounds, said, &rest[..end.min(MAX_SALT)]))
}

/// Hash `password` the way `stored` says to — its rounds and its salt — and
/// write the whole `$6$...` string into `out`. The length written, or `None`
/// if `stored` is not a `$6$` setting or the password is too long.
pub fn sha512_crypt(password: &[u8], stored: &[u8], out: &mut [u8; MAX_HASH]) -> Option<usize> {
    let (rounds, said, salt) = setting(stored)?;
    if password.len() > MAX_PASSWORD {
        return None;
    }

    let mut b = Sha512::new();
    b.update(password);
    b.update(salt);
    b.update(password);
    let b = b.finish();

    let mut a = Sha512::new();
    a.update(password);
    a.update(salt);
    let mut left = password.len();
    while left > 64 {
        a.update(&b);
        left -= 64;
    }
    a.update(&b[..left]);
    let mut bits = password.len();
    while bits > 0 {
        if bits & 1 != 0 {
            a.update(&b);
        } else {
            a.update(password);
        }
        bits >>= 1;
    }
    let mut digest = a.finish();

    // P: the password's digest, drawn out to the password's length.
    let mut dp = Sha512::new();
    for _ in 0..password.len() {
        dp.update(password);
    }
    let dp = dp.finish();
    let mut p = [0u8; MAX_PASSWORD];
    for (i, byte) in p.iter_mut().take(password.len()).enumerate() {
        *byte = dp[i % 64];
    }
    let p = &p[..password.len()];

    // S: the salt's, drawn out to the salt's.
    let mut ds = Sha512::new();
    for _ in 0..16 + digest[0] as usize {
        ds.update(salt);
    }
    let ds = ds.finish();
    let mut s = [0u8; MAX_SALT];
    s[..salt.len()].copy_from_slice(&ds[..salt.len()]);
    let s = &s[..salt.len()];

    for round in 0..rounds {
        let mut c = Sha512::new();
        if round & 1 != 0 {
            c.update(p);
        } else {
            c.update(&digest);
        }
        if round % 3 != 0 {
            c.update(s);
        }
        if round % 7 != 0 {
            c.update(p);
        }
        if round & 1 != 0 {
            c.update(&digest);
        } else {
            c.update(p);
        }
        digest = c.finish();
    }

    let mut len = 0;
    let mut put = |bytes: &[u8], out: &mut [u8; MAX_HASH]| {
        out[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
    };
    put(b"$6$", out);
    if said {
        put(b"rounds=", out);
        let mut digits = [0u8; 10];
        let mut n = rounds;
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        put(&digits[at..], out);
        put(b"$", out);
    }
    put(salt, out);
    put(b"$", out);
    for [x, y, z] in ORDER {
        let mut w = (digest[x] as u32) << 16 | (digest[y] as u32) << 8 | digest[z] as u32;
        for _ in 0..4 {
            put(&[ALPHABET[(w & 63) as usize]], out);
            w >>= 6;
        }
    }
    let mut w = digest[63] as u32;
    for _ in 0..2 {
        put(&[ALPHABET[(w & 63) as usize]], out);
        w >>= 6;
    }
    Some(len)
}

/// Whether `password` is the one `stored` was made from. An empty `stored`
/// is no password at all and is not for this to say; anything that is not a
/// `$6$` hash — `!`, `*`, another scheme — matches nothing.
pub fn verify(password: &[u8], stored: &[u8]) -> bool {
    let mut made = [0u8; MAX_HASH];
    let Some(len) = sha512_crypt(password, stored, &mut made) else {
        return false;
    };
    // Every byte looked at, however early they differ: how long this takes
    // says nothing about where.
    let mut differ = (len != stored.len()) as u8;
    for i in 0..len.min(stored.len()) {
        differ |= made[i] ^ stored[i];
    }
    differ == 0
}

/// A new hash for `password`, salted with sixteen characters made from
/// `random` (twelve bytes of it).
pub fn make(password: &[u8], random: &[u8; 12], out: &mut [u8; MAX_HASH]) -> Option<usize> {
    let mut setting = [0u8; 3 + MAX_SALT];
    setting[..3].copy_from_slice(b"$6$");
    for (i, three) in random.chunks(3).enumerate() {
        let mut w = (three[0] as u32) << 16 | (three[1] as u32) << 8 | three[2] as u32;
        for j in 0..4 {
            setting[3 + i * 4 + j] = ALPHABET[(w & 63) as usize];
            w >>= 6;
        }
    }
    sha512_crypt(password, &setting, out)
}
