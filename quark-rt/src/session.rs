//! Starting a program as somebody: what `login` and `su` both do.
//!
//! The program is built the ordinary way — its image, the caller's three
//! descriptors, a directory to be in, its arguments — holding nothing but
//! the nameserver's endpoint. Before it is started `auth` is asked to make
//! it the user, which is where a password is checked and where what that
//! user's sessions hold comes from (see [`crate::auth`]). Then it is
//! started, and whoever started it waits.
//!
//! Nothing here holds the right to say who a task is. A program that used
//! to — `login` held `SetUid`, the power ports and authority over every
//! task, to hand them on — holds nothing now.

use crate::accounts::User;
use crate::spawn::{self, Scratch, Spawned};
use crate::{auth, syscall, vfs};

/// What is to be started, and as whom.
pub struct Session<'a> {
    pub user: &'a User<'a>,
    /// The password that was typed, if one was asked for.
    pub password: &'a [u8],
    /// [`auth::CHECK`], [`auth::OWN`] or neither.
    pub flags: u64,
    /// The program's path.
    pub program: &'a [u8],
    pub args: &'a [&'a [u8]],
    pub env: &'a [&'a [u8]],
    /// Whether it starts in the account's home, rather than where the
    /// caller is.
    pub home: bool,
}

/// Why a session did not begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The program could not be loaded.
    NoProgram,
    /// `auth` said no: one of its `ERR_*`.
    Auth(u64),
}

/// Build the program and have it made the user. It is not started: the
/// caller starts it, having done anything else it wants done first.
pub fn prepare(vfs_tid: usize, s: &Session, image_at: usize, scratch: &Scratch) -> Result<Spawned, Refused> {
    let info = spawn::load_path(vfs_tid, s.program, image_at, scratch, |_, _| {}).map_err(|()| Refused::NoProgram)?;
    let tid = info.tid;
    // The nameserver, which is how it finds everything else.
    let _ = syscall::sys_cap_grant(tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT);
    for fd in 0..3 {
        let _ = syscall::sys_fd_dup(tid, fd, fd);
    }
    // Somewhere to be: where this program is, unless it is to start at the
    // account's home — and that is not for this program to put it in. A home
    // is its owner's to enter and may be nobody else's, so `su - ada` typed
    // by somebody who is not ada would have left ada's shell wherever it was
    // typed. Whoever makes the child ada puts it there.
    if !s.home {
        let _ = vfs::give_cwd(vfs_tid, tid);
    }
    let _ = spawn::set_args_env(&info, s.args, s.env, scratch);

    let flags = s.flags | if s.home { auth::HOME } else { 0 };
    match auth::bless(tid, s.user.name, s.password, flags) {
        Ok(_) => Ok(info),
        Err(code) => {
            info.discard();
            Err(Refused::Auth(code))
        }
    }
}
