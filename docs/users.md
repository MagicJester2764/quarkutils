# Users

More than one person uses a system, and each is somebody: a number the
kernel keeps for every task, which decides whose files are whose and whose
programs are whose. This is how that works here, what is Unix's about it and
what is not.

## Three things, kept in three places

**Who a task is** is the kernel's. A task has a user, a group, and up to
sixteen groups it is in besides. A task is who its creator was; `fork` copies
all of it and `exec` keeps it. Anybody may ask (`SYS_GET_UID`,
`SYS_GET_TUID`, `SYS_GROUPS`). Saying it takes the `SetUid` capability.

**Whose a file is** is the file server's. Owners, groups and modes are
Unix's, checked on every request against who the caller is: see
`docs/vfs.md`. User 0 is not checked.

**What a program may do** — everything that is not a file — is neither. It
is what the program *holds*: a capability for each thing, handed over by
whatever started it. There is no test anywhere of "is this user 0" that
unlocks a port, a disk or another program. Being root gets a task past the
file server and past the two drivers that ask (a disk is claimed by user 0,
a font is taken from user 0); the rest of what "root" means on Unix is
capabilities, and they are handed to a session when it begins.

So an account, here, is two things: who its sessions are, and what its
sessions hold.

## Nothing is setuid

On Unix `su` and `passwd` work because of a bit in a file's mode: the kernel
runs that file as its owner, whoever asks. That cannot exist here, and the
reason is structural. A program is loaded by whoever starts it — the image
is read in user space, into an address space the starter made — so nothing
can vouch that what is about to run is the file whose mode said so. And a
starter hands on only what it holds, so no manifest could give `su` the
right either: a shell that could give it could keep it.

So the right to say who a task is belongs to one program, and that program
is a server: **`auth`**. It is in the boot image, `init` starts it, and it
alone (with `init`) holds `SetUid`. It alone reads `/etc/shadow`.

## What `auth` does

It **blesses a child**. A program that wants to start something as somebody
— `login`, `su`, a distribution's own — builds the child the ordinary way:
its image, its descriptors, its arguments, and no capability but the
nameserver's endpoint. Before starting it, it asks `auth` to make it user U.
`auth` checks the password, tells the kernel who the child is, hands the
child what U's sessions hold, and answers. The asker starts the child.

Nothing is ever narrowed, because a child that has not started holds nothing
to narrow. And nothing runs as root on the way: the child is U before it has
executed an instruction.

The kernel's part is `SYS_IDENTIFY`: a holder of `SetUid` says who a task is
— its user, its group and its groups, in one step — where that task is in a
call to the holder, or is a child the caller of such a call made and has not
started. The kernel checks that at the moment it happens. Task ids are
recycled, and "I looked, it was the caller's child, so I set its user" names
whatever has the number by then; `auth` therefore does everything that can
wait — reading the files, hashing the password — *before* it identifies, and
hands over the capabilities straight after with nothing in between that
waits.

### The protocol (`quark_rt::auth`)

The server is registered as `auth`. Every request lends its text.

| Tag | Request | Lent | Answer |
|---|---|---|---|
| 1 | `NEEDS [name_len]` | a name | `[1]` if a password will be asked for, `[0]` if the account has none |
| 2 | `BLESS [child, name_len, password_len, flags]` | the name, then the password | `[uid, gid]` |
| 3 | `PASSWD [name_len, old_len, new_len]` | the name, the old password, the new | — |

`BLESS`'s flags:

| Bit | Name | |
|---|---|---|
| 1 | `CHECK` | Check the password even though it is user 0 asking. `login` is user 0. |
| 2 | `OWN` | The password is the *caller's own*. For an account that may `become` (below). |
| 4 | `HOME` | Start the child in the account's home. A home is its owner's to enter; the asker may not be able to put it there. |

A refusal is tag `u64::MAX` and one of:

| | | |
|---|---|---|
| 1 | `NO_USER` | No account has that name. Only said where it tells nobody anything: to root, changing a password. |
| 2 | `WRONG` | The password is not the one — or the name is nobody's. The two are not told apart. |
| 3 | | Not said: an account nobody logs in to with a password is answered `WRONG`. Whoever is told an account is locked has been told it is there. |
| 4 | `NOT_YOURS` | The task is not the asker's own child, still being made. |
| 5 | `WAIT` | Too many wrong passwords; the second word is seconds. |
| 6 | `NOT_ALLOWED` | The asker may not ask this. |
| 7 | `BAD` | The request made no sense. |
| 8 | `IO` | The account files could not be read or written. |

Who is asked for a password: nobody, if it is user 0 asking without `CHECK`;
the asker's own account, with `OWN`; otherwise the account being become. An
account with no hash at all asks for none.

**A name nobody has is treated exactly as a name somebody has.** `NEEDS`
says a password will be asked for; `BLESS` hashes what was typed against a
hash no password makes, takes as long, counts it as a wrong one, and answers
`WRONG`. A locked account is answered the same way, after the same work.
`login` asks about a name it could not find for the same reason. Somebody at
a login prompt learns nothing about which names there are.

**Wrong passwords are counted, and slow the next one.** The first two cost
nothing; then one second, two, four, up to thirty, during which even the
right password is refused (`WAIT`). The server never sleeps — it serves
everybody — it refuses early. The count is kept by *who asked* (so that a
user guessing at root's password does not lock root out of the console) and
by the *name typed*, hashed into one of sixteen places for that asker (so
that names nobody has are counted like the rest, and so that nothing is ever
pushed out of a table to make room: a table that kept "the last few
accounts" had its count for root thrown away by anybody who guessed at
enough other names). A right password clears the count only for the name it
was right for. Ten minutes after the last wrong one, it is forgotten.

## The files

Unix's, in Unix's forms, so that every C program reads them:

| File | Mode | A line |
|---|---|---|
| `/etc/passwd` | 0644 | `name:x:uid:gid:about:home:shell` |
| `/etc/group` | 0644 | `name:x:gid:member,member` |
| `/etc/shadow` | 0600 | `name:hash:day::::::` — nine fields, of which the first three are used |
| `/etc/rights` | 0644 | `name right right ...` — this system's own; see below |

A hash is SHA-512 `crypt` (`$6$salt$...`), what `crypt(3)` makes and every
Unix tool writes: `quark_rt::crypt` is this system's own implementation,
`dtest` holds it to the published test vectors, and a C test holds the C
library's `crypt` to the same ones. An empty hash is an account that asks
for no password; `!` or `*` is one nobody logs in to with a password at all,
which is what a new account is until `passwd` gives it one.

`quark_rt::accounts` reads and rewrites all four. A file is written whole
beside the old one and renamed over it, so a machine that stops half way has
one whole file or the other — and it is made with the mode it is to have, so
that the passwords are at no moment anybody's to read under either name.

### What an account may do: `/etc/rights`

| Right | What a session of the account is handed |
|---|---|
| `power` | the ports that turn the machine off and restart it, and authority over every task (ending them is part of turning a machine off) |
| `tasks` | authority over every task: ending anybody's program |
| `become` | nothing at login. It lets `BLESS` with `OWN` succeed: the account becomes another on its own password |
| `all` | everything, `SetUid` among it |

An account no line names has what Unix gives it: everything for user 0 and
nothing for anybody else. A system with no such file at all is one where
root is root and nobody else is anything. A distribution that wants more
than that ships the file and something to edit it.

A right is a capability handed over when a session begins. Taking one away
takes effect at the account's next login, and a program that is already
running keeps what it holds.

## What somebody types

A terminal is its session's. `login`, on a terminal, begins a session and
takes the terminal as that session's own; the session ends when `login`
does, which is when its user logs out; and the kernel gives a terminal's
slave only to members of the session that has it. So a program that somebody
left running and then logged out — which still holds the descriptor it was
started with — cannot read what the next person types, cannot write to
their screen, and cannot open the terminal by its name. Nor can anybody open
a terminal of a session they are not in.

The exception is a holder of authority over every task, which a session of
an account with the `tasks` right, or root's, is.

This is true of a terminal and not of the plain console, which hands a
typed line to whoever asks for one. A system with more than one user runs
its sessions on a terminal (`session /usr/bin/getty` in `/etc/init.conf`).

## The programs

| | |
|---|---|
| `login` | asks for a name and, where there is one, a password; starts the account's shell |
| `su [-] [USER] [-c COMMAND]` | USER's shell, on this terminal. Asks for USER's password; root is asked for none |
| `passwd [USER]` | one's own, with the old one; anybody's, for root. `-d` takes a password away, `-l` locks |
| `useradd [-m] [-u UID] [-g GROUP] [-G GROUPS] [-c ABOUT] [-d HOME] [-s SHELL] NAME` | a new account, locked. `-m` makes its home, 0700 |
| `userdel NAME` | takes an account away. Its home is left |
| `groupadd [-g GID] NAME` | a new group |
| `gpasswd -a USER GROUP`, `-d USER GROUP` | puts a user in a group, takes one out |
| `id [USER]` | who this is, or who USER would be |

`passwd`, `useradd`, `userdel`, `groupadd` and `gpasswd` take `--root DIR`
first, to work on a system mounted at DIR. That is how an installer gives a
new system its first user: the system is not running, there is no `auth` to
ask, and root — who alone may — writes the files.

None of them holds anything. `login` used to hold `SetUid`, authority over
every task and the power ports, and handed all but the first to every shell
it started, whoever's.

## For somebody writing a program

- **To start something as somebody**, use `quark_rt::session::prepare`: it
  builds the child, gives it the three descriptors and somewhere to be, and
  asks `auth`. Start what it returns. If it is refused, the child has
  already been taken back (`Spawned::discard`) — a child that is built and
  not wanted has to go back whole, or sixty-four wrong passwords are the
  last anybody types.
- **A password is read with `stdio::read_secret`**, which stops the terminal
  echoing for one line, on a pty and on the old console both. Zero the
  buffer when it has been used.
- **A program that needs a capability its account was not given says so.**
  `shutdown` looks for the port before it does anything and says "this
  account may not turn the machine off". A program that just tries, and does
  nothing, is indistinguishable from one that is broken.
- **Never decide by user id what a program may do.** A check of
  `sys_get_uid().0 == 0` is right in two kinds of place: a server deciding
  whose a *resource it owns* is (the file server, a disk), and a program
  saying early what a server would say late (`mount`: "only root mounts a
  filesystem"). Anything else is a capability.
- **In C**, `getgroups` and `setgroups` are the kernel's groups. `setuid`
  away from user 0 gives the capability up, so that it is for good, as it is
  on Unix; `seteuid` keeps it, so that a program can come back. A C program
  that wants to be able to say who it is asks for `QUARK_CAP_SET_UID` in its
  manifest, and gets it only if whoever starts it holds it.

## What is tested

`dtest`: `identity` (the kernel's calls), `passwords` (the hash and the
account files' text), `users` (a program run as a user: what the file server
and the kernel refuse it), `jobs` (a terminal refused to a user outside its
session, and a session that is one login's), `auth` (an account made, given a password,
become, throttled, given rights, taken away), and in `mounts` the same file
rules through a second file server and on FAT. `qfuzz` sends `auth` two
thousand requests and holds it to having made nobody anybody and changed no
password. The C tests `idtest` and `crypttest` are the C library's side.

## Known gaps

- **The keyboard and the display are claimed, not owned.** A terminal is
  its session's, but under the terminal `input` hands the raw keyboard to
  whoever claims it and `fb` the display — which is how a compositor
  somebody runs takes both. Any program can, a user's left-behind one among
  them. It is not quiet: the console gets none of the keys a claimant takes,
  so nothing typed is echoed. But it is not nothing. Whose seat a machine's
  keyboard and screen are is a question nothing here answers yet.
- **One id.** A task has one user, not Unix's three. The real and effective
  ids are always equal; `seteuid` changes who the task *is*, and what keeps
  the way back is the capability, not a saved id.
- **A thread has a copy of its program's capabilities**, so a threaded C
  program that drops `SetUid` drops it in the thread that asked.
- **A right taken away is not taken from a session that has it.**
- **No list of commands a user may run as another**: `become` is all or
  nothing. The server would have to load the program itself to know what it
  was vouching for.
- **No password ageing, no PAM, no `newgrp`, no `groupdel` or `usermod`.**
  The fields are kept in `/etc/shadow` and nothing reads them.
- **User and group numbers are sixteen bits on disk**: ext2's inode keeps
  that many, and this does not use the field that would hold the rest.
- **The count of wrong passwords is in memory**, and a restart forgets it.
- **If `auth` goes, nobody logs in** until the machine is restarted: nothing
  starts it again, and nothing else may say who a task is.
- **`auth` reads the account files again for every request.** It is correct
  and it is not fast; nothing here logs in often enough to notice.
