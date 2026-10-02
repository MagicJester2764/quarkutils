//! A command line split into words, as a shell splits it.
//!
//! Spaces separate words, except inside quotes. Inside `"…"`, `\"` and `\\`
//! stand for a quote and a backslash; inside `'…'` nothing is special. Quoted
//! and unquoted pieces that touch make one word, so `a"b c"d` is `ab cd`, and
//! `""` is a word with nothing in it.

/// The most words a command may have, its name included.
pub const MAX_WORDS: usize = 16;
/// Room for the words of one line, unquoted. Unquoting never lengthens a line,
/// so this is as long as the longest line.
pub const STORE: usize = 256;

/// Split `line` into at most [`MAX_WORDS`] words, copied unquoted into
/// `store`, with `out` pointing into it. Returns how many there are, or what
/// is wrong with the line.
pub fn split<'a>(
    line: &[u8],
    store: &'a mut [u8; STORE],
    out: &mut [&'a [u8]; MAX_WORDS],
) -> Result<usize, &'static str> {
    let mut ranges = [(0usize, 0usize); MAX_WORDS];
    let mut n = 0;
    let mut len = 0;
    let mut i = 0;
    macro_rules! push {
        ($c:expr) => {{
            if len >= STORE {
                return Err("line too long");
            }
            store[len] = $c;
            len += 1;
        }};
    }
    loop {
        while i < line.len() && line[i] == b' ' {
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        if n == MAX_WORDS {
            return Err("too many arguments");
        }
        let start = len;
        while i < line.len() && line[i] != b' ' {
            match line[i] {
                b'"' => {
                    i += 1;
                    loop {
                        match line.get(i) {
                            None => return Err("unterminated quote"),
                            Some(b'"') => {
                                i += 1;
                                break;
                            }
                            Some(b'\\') if matches!(line.get(i + 1), Some(b'"' | b'\\')) => {
                                push!(line[i + 1]);
                                i += 2;
                            }
                            Some(&c) => {
                                push!(c);
                                i += 1;
                            }
                        }
                    }
                }
                b'\'' => {
                    i += 1;
                    loop {
                        match line.get(i) {
                            None => return Err("unterminated quote"),
                            Some(b'\'') => {
                                i += 1;
                                break;
                            }
                            Some(&c) => {
                                push!(c);
                                i += 1;
                            }
                        }
                    }
                }
                c => {
                    push!(c);
                    i += 1;
                }
            }
        }
        ranges[n] = (start, len);
        n += 1;
    }
    let store: &'a [u8; STORE] = store;
    for (slot, &(from, to)) in out.iter_mut().zip(ranges.iter()).take(n) {
        *slot = &store[from..to];
    }
    Ok(n)
}

/// Where a command's input comes from and its output goes, when it is not
/// where the shell's does. The names are as they were typed, quotes and all:
/// [`split`] each to have the word.
#[derive(Clone, Copy, Default)]
pub struct Redirects<'a> {
    /// `< FILE`
    pub input: Option<&'a [u8]>,
    /// `> FILE`, or `>> FILE`
    pub output: Option<&'a [u8]>,
    /// Whether the output is added to what the file has.
    pub append: bool,
}

/// Take the redirections out of a command: `< FILE`, `> FILE` and `>> FILE`,
/// outside quotes, wherever in it they are. What is left is copied to
/// `rest`; how long that is, and what was asked for.
pub fn redirects<'a>(line: &'a [u8], rest: &mut [u8; STORE]) -> Result<(usize, Redirects<'a>), &'static str> {
    let mut found = Redirects::default();
    let mut len = 0;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        match quote {
            Some(b'"') if c == b'\\' && i + 1 < line.len() => {
                // The pair is copied whole, and the second is not a quote.
                if len + 2 > STORE {
                    return Err("line too long");
                }
                rest[len] = c;
                rest[len + 1] = line[i + 1];
                len += 2;
                i += 2;
                continue;
            }
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' || c == b'<' => {
                let append = c == b'>' && line.get(i + 1) == Some(&b'>');
                i += if append { 2 } else { 1 };
                while line.get(i) == Some(&b' ') {
                    i += 1;
                }
                // The name: up to a space, or another redirection, outside
                // quotes.
                let start = i;
                let mut q: Option<u8> = None;
                while i < line.len() {
                    match (q, line[i]) {
                        (Some(b'"'), b'\\') => i += 1,
                        (Some(open), ch) if ch == open => q = None,
                        (Some(_), _) => {}
                        (None, b'"' | b'\'') => q = Some(line[i]),
                        (None, b' ' | b'>' | b'<') => break,
                        (None, _) => {}
                    }
                    i += 1;
                }
                let name = &line[start..i.min(line.len())];
                if name.is_empty() {
                    return Err("a redirection wants a file to go to");
                }
                if c == b'<' {
                    found.input = Some(name);
                } else {
                    found.output = Some(name);
                    found.append = append;
                }
                // A space where it was, so that what was either side of it
                // is still two words.
                if len < STORE {
                    rest[len] = b' ';
                    len += 1;
                }
                continue;
            }
            None => {}
        }
        if len >= STORE {
            return Err("line too long");
        }
        rest[len] = c;
        len += 1;
        i += 1;
    }
    Ok((len, found))
}

/// Split `line` into the stages of a pipeline, at every `|` outside quotes.
/// Returns how many there are, or `None` if there are more than `out` holds.
pub fn stages<'a>(line: &'a [u8], out: &mut [&'a [u8]]) -> Option<usize> {
    let mut n = 0;
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        match quote {
            Some(b'"') if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'|' => {
                *out.get_mut(n)? = &line[start..i];
                n += 1;
                start = i + 1;
            }
            None => {}
        }
        i += 1;
    }
    *out.get_mut(n)? = &line[start..];
    Some(n + 1)
}
