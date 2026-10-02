//! Asking who somebody is: the client's side of `auth`.
//!
//! There is one program that may say who a task is, and it is a server:
//! see `auth/src/main.rs` for why. A program that wants a child of its own
//! to be somebody — `login`, `su` — builds the child, asks with [`bless`]
//! before starting it, and starts it if the answer is yes.

use crate::ipc::Message;
use crate::{nameserver, syscall};

/// The name the server registers.
pub const NAME: &[u8] = b"auth";

pub const TAG_OK: u64 = 0;
pub const TAG_ERROR: u64 = u64::MAX;
/// `[user_len]`, the name lent. Answers `[1]` if a password will be asked
/// for and `[0]` if the account has none. A name nobody has is asked for
/// one, so that asking tells nobody which names there are.
pub const TAG_NEEDS: u64 = 1;
/// `[child, user_len, password_len, flags]`, the name and then the password
/// lent. Answers `[uid, gid]`.
pub const TAG_BLESS: u64 = 2;
/// `[user_len, old_len, new_len]`, the three lent one after another.
pub const TAG_PASSWD: u64 = 3;

/// Check the password whoever is asking. Root is not asked for one
/// otherwise — and `login` is root.
pub const CHECK: u64 = 1;
/// The password is the caller's own, not the account's being become: for
/// an account that may `become`.
pub const OWN: u64 = 2;

/// Start the child in the account's home. Said by a program that would
/// have put it there itself — `login`, `su -` — if the home of an account
/// were always the caller's to enter. It is the account's, and the server's.
pub const HOME: u64 = 4;

/// No account has that name (only where saying so tells nobody anything).
pub const ERR_NO_USER: u64 = 1;
/// The password is not the one, or the account is not one.
pub const ERR_WRONG: u64 = 2;
/// The account cannot be logged in to with a password.
pub const ERR_LOCKED: u64 = 3;
/// The task is not the caller's own child, still being made.
pub const ERR_NOT_YOURS: u64 = 4;
/// Too many wrong passwords: the second word is how many seconds to wait.
pub const ERR_WAIT: u64 = 5;
/// The caller may not ask this: somebody else's password, or to become
/// somebody without the right to.
pub const ERR_NOT_ALLOWED: u64 = 6;
/// The request made no sense.
pub const ERR_BAD: u64 = 7;
/// The account files could not be read, or written.
pub const ERR_IO: u64 = 8;
/// There is no such server to ask.
pub const ERR_NO_SERVER: u64 = 9;

/// What a refusal means, to somebody reading it.
pub fn why(code: u64) -> &'static str {
    match code {
        ERR_NO_USER => "there is no such user",
        ERR_WRONG => "that is not right",
        ERR_LOCKED => "that account is locked",
        ERR_NOT_YOURS => "that is not a program of yours to make somebody of",
        ERR_WAIT => "too many wrong tries: wait a little",
        ERR_NOT_ALLOWED => "this account may not",
        ERR_IO => "the account files could not be read or written",
        ERR_NO_SERVER => "nothing here can say who anybody is",
        _ => "the request was refused",
    }
}

fn ask(tag: u64, data: [u64; 6], pieces: &[&[u8]]) -> Result<Message, u64> {
    let server = nameserver::lookup(NAME).ok_or(ERR_NO_SERVER)?;
    let mut text = [0u8; 512];
    let mut len = 0;
    for piece in pieces {
        text.get_mut(len..len + piece.len()).ok_or(ERR_BAD)?.copy_from_slice(piece);
        len += piece.len();
    }
    let mut reply = Message::empty();
    let msg = Message { sender: 0, tag, data };
    let sent = syscall::sys_call_lend(server, &msg, &mut reply, &text[..len]);
    // A password does not stay in memory this has finished with.
    text.fill(0);
    match sent {
        Ok(()) if reply.tag == TAG_OK => Ok(reply),
        Ok(()) => Err(match reply.data[0] {
            0 => ERR_BAD,
            code => code,
        }),
        Err(()) => Err(ERR_NO_SERVER),
    }
}

/// Whether `user` will be asked for a password.
pub fn needs(user: &[u8]) -> Result<bool, u64> {
    ask(TAG_NEEDS, [user.len() as u64, 0, 0, 0, 0, 0], &[user]).map(|r| r.data[0] != 0)
}

/// Make `child` — a task the caller has created and not yet started — the
/// user `user`, whose sessions hold what that account may. The ids it was
/// given.
pub fn bless(child: usize, user: &[u8], password: &[u8], flags: u64) -> Result<(u32, u32), u64> {
    let data = [child as u64, user.len() as u64, password.len() as u64, flags, 0, 0];
    ask(TAG_BLESS, data, &[user, password]).map(|r| (r.data[0] as u32, r.data[1] as u32))
}

/// Change `user`'s password. `old` is not looked at when root asks.
pub fn passwd(user: &[u8], old: &[u8], new: &[u8]) -> Result<(), u64> {
    let data = [user.len() as u64, old.len() as u64, new.len() as u64, 0, 0, 0];
    ask(TAG_PASSWD, data, &[user, old, new]).map(|_| ())
}
