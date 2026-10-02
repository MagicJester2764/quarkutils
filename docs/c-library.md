# C on Quark

What a C program written for Linux finds here, and where it differs.

A C program on Quark is built against **musl**, and musl believes it is
talking to Linux: it makes Linux's system calls, with Linux's numbers and
structures. Quark's kernel has different calls, so each one musl makes is
answered by a layer linked into the program — `linux-abi/` in this tree —
which turns it into what Quark does have: a call to the kernel, a message to
the file server, or `ENOSYS`. Nothing in musl is changed beyond the seven
files that make it call the layer (`quark-toolchain`'s patch).

So a program ported to Quark is a Linux program, and nearly everything in a
Linux manual page is true of it. This document is the list of what is not.

There is a second, small C library in `libc/`, written against Quark's own
calls. The kernel's tools and two of this tree's programs use it; nothing
ported does, and nothing below is about it except where it says so.

## Processes and threads

### `getpid()` and `gettid()` are different kinds of number

On Linux a thread id and a process id come from one set of numbers, and a
program's first thread has the process's own: `gettid() == getpid()` in
`main`, always. **Here it is never true.**

| | is | range | reused |
|---|---|---|---|
| `getpid()`, `getppid()`, what `fork()` returns, what `wait`, `waitpid` and `kill` take | a **process id** | 64 and up | never |
| `gettid()`, what `pthread_kill`'s `tkill` takes, what musl keeps in `pthread_t` | a **task id** | 2 to 63 | as soon as it is free |

A task id is the kernel's name for one thread of control: a slot in a table
of sixty-four, and the next task made is given the lowest slot free — usually
the one that has just been let go. A process id is the number of the task a
program *began as*, counted from a counter that only goes up, and it stays
the program's through every `exec` and for every thread it makes.

Both exist because each is wanted for something the other cannot do. The
kernel's own calls name a task by its slot. A Unix program remembers a child
by a number and assumes the number is not somebody else a moment later: bash
decides whether to wait for a command by comparing its pid with the last
background job's, and when the two were the same slot, every command after
`sleep 2 &` ran with the prompt already back.

What follows from it:

- **`getpid() == gettid()` is not a test for the main thread.** It is false
  in every thread. Use `pthread_self()` and `pthread_equal()`, or remember
  the main thread's `pthread_t` at start.
- **`tgkill(getpid(), getpid(), sig)` does not signal the main thread**, and
  neither does `syscall(SYS_tkill, getpid(), sig)`: a process id is not a
  task id, and the call fails with `ESRCH`. `raise(sig)`,
  `pthread_kill(thread, sig)` and `kill(getpid(), sig)` all work.
- **`kill(gettid(), sig)` fails** with `ESRCH` for the same reason the other
  way round.
- **A number printed by one is not found by the other.** `ps` shows both
  columns.
- **A process id is at least 64**, so the two are never confused: a number
  below 64 is a task, and one from 64 up is a process.

Everything a C program does with *processes* uses process ids throughout —
`fork`, `wait4`, `kill`, `getppid`, `getpgrp`, `setpgid`, `setsid` — and is
exactly as on Linux. A process group and a session are named by process ids
too. The difference is only visible to code that mixes the two kinds.

### Starting and ending

- `fork` copies the whole address space at once; there is no copy-on-write.
  `vfork` is `fork`. `clone` makes a thread (with `CLONE_VM | CLONE_THREAD`)
  or is `fork` (with neither).
- **`posix_spawn` works, and its child does not share memory.** musl makes
  it of `clone` with `CLONE_VM | CLONE_VFORK`: a child that borrows its
  parent's memory while the parent waits. Here that child is a copy, as a
  `vfork` child is. musl's reports a failed exec down a pipe, so nothing it
  does depends on the difference; a program that calls `clone` that way
  itself and expects to see what the child wrote will not. `CLONE_VM` with
  neither `CLONE_THREAD` nor `CLONE_VFORK` is `ENOSYS`.
- **A program with more than one thread cannot `exec`.** POSIX has `exec` end
  the other threads; ending them here means unwinding what they hold in a
  server, so it is refused rather than half done.
- `exit` ends every thread of the program. `pthread_exit` ends one.
- A child that has ended waits to be collected, as a zombie does, and is
  collected by `wait`. `wait4` reports no resource usage: the structure is
  zeroed.
- An exit status is eight bits. A program ended by a signal, or by a fault —
  which ends it with the signal Linux would have sent — reports that signal
  to `WTERMSIG`.

## Signals

A program has `sigaction`, a signal mask, `kill`, `raise`, `sigsuspend`,
`sigtimedwait`, `alarm` and `setitimer(ITIMER_REAL)`, and is told when a
child ends (`SIGCHLD`). What differs is *when a handler runs*.

**A handler runs when the program next makes a system call, and at no other
time.** The kernel does not interrupt a program to run one: it records the
signal, and the layer runs the handler on the way out of whatever call the
program makes next. For nearly every program that is indistinguishable from
Linux, because a program waiting for something is in a system call, and the
waits a program sits in are ended early by a signal as they are on Linux:

- a read of a terminal, which fails with `EINTR`, or is made again if the
  handler was installed with `SA_RESTART`;
- `poll`, `ppoll`, `select`, `pselect` and `epoll_wait`, and `nanosleep`,
  `clock_nanosleep`, `sleep`, `usleep`, `pause` and `sigsuspend`, which fail
  with `EINTR` whatever the handler asked for — as on Linux, where none of
  them is ever restarted.

So is an `open` of a named pipe that is waiting for its other end.

Other waits are **not** ended by a signal: a read of a pipe, a socket or a
file, a `wait` for a child, a lock. (A `wait` is ended by what it is
waiting for, which with `WUNTRACED` includes a child stopping.) The handler runs when the call returns.
A program that wants a signal to interrupt one of those waits with `poll`
first, as it would for a timeout.

The program it is not indistinguishable for is one that installs a handler
and then **computes without making a call** — a loop that waits for a flag
the handler sets. Its handler does not run until it calls something.

Also different:

- A signal with no handler does what it does at once, **even if it is
  blocked**. The mask is kept by the layer, which can hold back only a signal
  it would have run a handler for. That includes the signals that stop a
  program: see *Job control*.
- A handler runs on the stack of whatever the program was doing, in whichever
  thread made the next system call. There is no alternate signal stack and
  no way to aim a signal at one thread: `pthread_kill` raises it for the
  program.
- The mask is the program's, not each thread's, and is not kept across
  `exec`.
- The interval timers that count time spent running (`ITIMER_VIRTUAL`,
  `ITIMER_PROF`) are refused, since nothing measures it; `timer_create` is
  `ENOSYS`, and the programs that try it first fall back to `setitimer`.
- Nothing is sent when a terminal changes size (`SIGWINCH`).

## Job control

There are process groups, sessions, a controlling terminal with a group in
front of it, and programs that stop: `setpgid`, `getpgid`, `getpgrp`,
`setsid`, `getsid`, `tcsetpgrp`, `tcgetpgrp`, `tcgetsid`, `TIOCSCTTY`,
`kill` and `killpg` of a group, and `waitpid` with `WUNTRACED`,
`WCONTINUED`, 0 and a negative pid. A shell with job control runs as it
does on Linux. What is different is at the edges:

- **Only a read, and a change of who is in front, are checked.** A job that
  reads the terminal from the background is stopped (`SIGTTIN`), and so is
  one that calls `tcsetpgrp` from there (`SIGTTOU`). A job that *writes*
  from the background is never stopped — `stty tostop` is stored and not
  acted on — and nor is one that calls `tcsetattr`.
- **A blocked stop signal still stops.** The mask is the layer's, and it can
  hold back only a signal it would have run a handler for; `SIGTSTP`,
  `SIGTTIN` and `SIGTTOU` with no handler do what they do at once, blocked
  or not. A program that blocks one of them to do something undisturbed
  should ignore it instead, or catch it. The one case every shell relies on
  works as on Linux: `tcsetpgrp` with `SIGTTOU` blocked succeeds from the
  background.
- **A terminal becomes a session's when its leader opens it** without
  `O_NOCTTY`, or asks with `TIOCSCTTY`; it cannot be taken from a session
  that has it, and is given up only by the leader ending (`TIOCNOTTY` is
  accepted and does nothing). Nothing is hung up on when a terminal's
  master closes: its readers see the end of the file.
- **`SIGCHLD` is raised for every stop and continue**, whatever
  `SA_NOCLDSTOP` asked for. A handler that waits with `WNOHANG` and without
  `WUNTRACED` finds nothing and returns.
- `waitid` is `ENOSYS`; `waitpid` and `wait4` are what there is.

## Files and descriptors

- A program has **64 descriptors**. `open` hands out numbers from 3: 0, 1
  and 2 are where the standard three go, and a file never lands on one
  because it happened to be closed.
- A file is a descriptor the kernel knows about, so it is copied by `fork`,
  kept by `exec` unless marked close-on-exec, and can be `dup`ed onto
  standard output — but the file itself is in the file server, and reading
  one is a message to it.
- There is no `/proc`. The one path under it that is answered is
  `/proc/self/fd/N` for a descriptor that is a terminal, because that is how
  musl's `ttyname` asks what a terminal is called.
- `/dev` has `null`, `zero`, `full`, `random` and `urandom`. `/dev/tty`,
  `/dev/stdin`, `/dev/stdout`, `/dev/stderr` and `/dev/fd/N` are names for
  descriptors the program already has, and `/dev/ptmx` and `/dev/pts/N` are
  terminals.
- **A disk is a file under `/dev`**: `/dev/disk0`, `/dev/disk0p1`,
  `/dev/ram0`. It is a block device to `stat`, root's alone, and is read and
  written with `pread` and `pwrite` at any offset. `BLKGETSIZE64`,
  `BLKGETSIZE`, `BLKSSZGET` and `BLKRRPART` are answered, and `lseek` to its
  end finds its size; `st_size` is 0, as on Linux. Opening one to write
  fails with `EBUSY` if a filesystem on it is mounted, or if it is the disk
  the system is running from; opening one to read never does, so a mounted
  filesystem can be looked at. `BLKRRPART` wants a descriptor that writes
  the whole disk (`EACCES`, and `EINVAL` for a partition). Nothing written
  to a disk this way is cached: it is on the disk when the write returns,
  and `fsync` and `BLKFLSBUF` add nothing.
- **A mounted filesystem is another device.** `st_dev` is 1 for the root
  and something else for each mount; `st_ino` and a directory entry's
  `d_ino` are the file's number in its own filesystem. `rename` and `link`
  across two say `EXDEV`. `statfs` and `fstatfs` answer for the filesystem
  the file is in. `/etc/mtab` lists what is mounted, as `getmntent` reads
  it; it is a file `mount`, `umount` and `init` write, not a view of the
  truth. There is no `mount(2)`: mounting is a program's job
  (`/usr/bin/mount`), because a mount is a server it has to start.
  `mmap` of a file in a mounted filesystem fails, and a named pipe there
  cannot be opened.
- **`fsync`, `fdatasync`, `syncfs` and `sync` wait.** A write returns a
  moment before the filesystem has recorded it for good — half a second at
  most, and usually a fiftieth of one; these return when it has. They do not tell
  one file from another: everything written is recorded, which is more than
  was asked and never less. On a pipe, a terminal or a socket they are
  `EINVAL`, as on Linux.
- `mmap` of a file works, shared and private. Anonymous memory is given its
  pages when they are first touched.
- **A named pipe** is made with `mkfifo` or `mknod`, on ext2 and ext4, and
  opened like any file. Opening one waits for the other end to be opened,
  as on Linux, unless `O_NONBLOCK` is given: then a reader is let in at
  once, and a writer with nobody reading gets `ENXIO`. A signal ends the
  wait (`EINTR`, or the open goes on if the handler was installed with
  `SA_RESTART`). **It cannot be opened `O_RDWR`** — Linux allows it, and
  POSIX leaves it undefined; here a descriptor is one end of a pipe, and the
  open fails with `EOPNOTSUPP`. `mknod` makes nothing else: a device is
  `EPERM`.

## Terminals

A terminal is a pseudo-terminal in the kernel, with a line discipline:
`termios` is stored whole, and what is acted on is canonical mode and its
editing characters, echo, the newline translations, the characters that
raise signals, and `IUTF8` — which is set on a new terminal, so that erasing
takes back a character and not a byte of one.

The console is UTF-8, and a character on it is as wide as `wcwidth` says. A
program still has to be told: in the C locale musl takes every byte for a
character. Any other locale name is UTF-8 — `LANG=C.UTF-8` is the usual one.
A combining mark is not drawn, and what a character looks like depends on
the font the console was given at boot. `openpty`, `forkpty`, `isatty`, `ttyname`, `tcgetattr` and
`tcsetattr` work unchanged.

## Time

The clock ticks a hundred times a second, and every wait — a sleep, a
timeout, an alarm — is rounded up to a tick. `clock_gettime` is finer than
that only as far as counting ticks allows. The date is read from the machine's
clock at boot, as UTC, and cannot be set.

## What is refused

A call the layer has no answer for returns `ENOSYS`, on purpose: a C library
told "no" copes, and one handed a made-up answer fails somewhere unrelated
and much later. The ones a ported program is most likely to meet:
`timer_create`, `set_robust_list`, `rseq`, `statx` (musl falls back to
`fstatat`), and `epoll_wait` on anything but the descriptors the layer can
poll.

Everything is linked statically. There is no dynamic loader, so `dlopen`
fails, and a library that would be loaded as a plugin has to be built in.
