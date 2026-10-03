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

- `fork` shares the address space and copies a page when either side writes
  it, as on Linux, so a child that execs at once costs page tables and
  little else. `vfork` is `fork`. `clone` makes a thread (with `CLONE_VM | CLONE_THREAD`)
  or is `fork` (with neither).
- **`posix_spawn` works, and its child does not share memory.** musl makes
  it of `clone` with `CLONE_VM | CLONE_VFORK`: a child that borrows its
  parent's memory while the parent waits. Here that child is a copy, as a
  `vfork` child is. musl's reports a failed exec down a pipe, so nothing it
  does depends on the difference; a program that calls `clone` that way
  itself and expects to see what the child wrote will not. `CLONE_VM` with
  neither `CLONE_THREAD` nor `CLONE_VFORK` is `ENOSYS`.
- `exit` ends every thread of the program. `pthread_exit` ends one.
- **A thread is joined, never waited for.** It is no child: `waitpid(-1)`
  in a program with threads and no child processes is `ECHILD`, and is
  never answered with a thread. A thread that has ended and been joined —
  or was detached — gives back its place among the system's tasks, of
  which there are sixty-four. *A child is the thread's that made it*,
  though, where on Linux it is the process's: `waitpid` for a child another
  thread forked is `ECHILD`. Fork and wait in the same thread.
- **Threads run at the same time**, on as many processors as the machine
  has: `sysconf(_SC_NPROCESSORS_ONLN)`, `nproc` and `sched_getaffinity` say
  how many, and `sched_getcpu` which one a thread is on at that moment.
  Every thread may run on every processor, and `sched_setaffinity` cannot
  change that: nothing pins one. The kernel itself serves one processor at
  a time, so threads that compute run in parallel and threads that make
  system calls take turns at them.
- A child that has ended waits to be collected, as a zombie does, and is
  collected by `wait`. `wait4` reports no resource usage: the structure is
  zeroed.
- An exit status is eight bits. A program ended by a signal, or by a fault —
  which ends it with the signal Linux would have sent — reports that signal
  to `WTERMSIG`.

## Who a program is

- **One id, not three.** A task has one user and one group. `getuid` and
  `geteuid` answer the same number, always, and so do the three that
  `getresuid` fills in. There is no file whose mode makes a program run as
  its owner, so nothing here ever has a real id that differs from its
  effective one.
- **`getgroups` and `setgroups` are the kernel's**: the groups a process is
  in besides its own, sixteen at most. `getgroups` with too little room is
  `EINVAL`, and with none says how many there are.
- **Changing who a program is takes a capability, not being root.** `setuid`,
  `setgid` and `setgroups` are `EPERM` without `SetUid`, which a program is
  given only if it asks for it (`QUARK_CAP_SET_UID` in its manifest) and
  whoever starts it holds it — or if it was forked from something that did.
  Asking to be who one already is, or to be in the groups one is in, is
  always allowed.
- **`setuid` to somebody else is for good.** The capability is given up as
  the id changes, so there is no way back, as Unix promises. The forms that
  change only the effective id — `seteuid`, `setreuid(-1, u)`,
  `setresuid(-1, u, -1)` — keep it, and a program can come back, as Unix
  allows; while it is away its real id is the other user's too, there being
  one. Change groups first, and the user last: after `setuid` there is
  nothing left to change them with.
- **`crypt` is musl's**, and the system's own `passwd` writes hashes it
  verifies (`$6$`, SHA-512). `getpwnam`, `getgrnam` and `getspnam` read the
  files, which are Unix's; `/etc/shadow` is root's to read.
- **`kill` of somebody else's process is `EPERM`**, and of one that is not
  there `ESRCH`.

## Signals

A program has what Linux gives it: `sigaction` with `SA_SIGINFO`,
`SA_RESTART`, `SA_NODEFER`, `SA_RESETHAND` and `SA_ONSTACK`; a mask for
each thread (`sigprocmask`, `pthread_sigmask`); `kill`, `raise`,
`pthread_kill`; `sigsuspend`, `pause`, `sigpending`, `sigwait`,
`sigwaitinfo` and `sigtimedwait`; `sigaltstack`; `alarm` and
`setitimer(ITIMER_REAL)`; and `SIGCHLD` when a child ends.

**The kernel runs the handler**, as Linux's does: a thread that does not
block the signal is turned aside wherever it is — in a call, or computing —
and goes on where it was when the handler returns. A handler that changes
the `ucontext_t` it is handed changes where the thread goes on from, and
the mask it goes back to. The floating-point registers are kept around it,
and it runs with the rounding mode and exceptions of a new thread.

A signal for the program is run by a thread that does not block it; one for
a thread (`pthread_kill`, `raise`, `tgkill`) by that thread. A signal every
thread blocks waits, whatever it would do — one that would end the program
ends it when a thread unblocks it — and can be taken with `sigwaitinfo`
without a handler. A fault — touching what is not there (`SIGSEGV`), a page
of a file that cannot be had (`SIGBUS`), dividing by nought (`SIGFPE`), an
instruction that is not one (`SIGILL`) — goes to a handler for it, with
`si_addr`; a program that overflows its stack is saved by a handler on an
alternate one, as on Linux.

Every wait a thread sits in is ended by a signal it is to handle, and says
so as Linux does: a read or a write of a pipe, a socket or a terminal, a
`wait` for a child, a futex (and so `sem_wait`), an `open` of a named pipe
— each fails with `EINTR`, or is made again if the handler was installed
with `SA_RESTART`; and `poll`, `ppoll`, `select`, `pselect`, `epoll_wait`,
`epoll_pwait`, `nanosleep`, `clock_nanosleep`, `sleep`, `usleep`, `pause`,
`sigsuspend` and `sigtimedwait` fail with `EINTR` whatever the handler asked
for, as on Linux, where none of them is made again. `ppoll`, `pselect` and
`epoll_pwait` put their mask on in the same step as the wait begins.

What is different:

- **A read or a write of a file is not ended by a signal**, nor a wait for a
  lock: they are calls to the file server, and the handler runs when it has
  answered. On Linux a file on a disk is not interruptible either; a lock's
  wait is.
- **Nothing is queued.** A signal raised twice before it is handled is
  handled once — the real-time signals included, which Linux queues.
- **`siginfo_t` says who sent a signal by process id and nothing more**:
  `si_uid` is 0, and for `SIGCHLD` there is no `si_status`.
- The interval timers that count time spent running (`ITIMER_VIRTUAL`,
  `ITIMER_PROF`) are refused: the time is measured (`getrusage`, below) and
  limited (`RLIMIT_CPU`), but nothing counts it down. `timer_create` is
  `ENOSYS`, and the programs that try it first fall back to `setitimer`.

## Job control

There are process groups, sessions, a controlling terminal with a group in
front of it, and programs that stop: `setpgid`, `getpgid`, `getpgrp`,
`setsid`, `getsid`, `tcsetpgrp`, `tcgetpgrp`, `tcgetsid`, `TIOCSCTTY`,
`kill` and `killpg` of a group, and `waitpid` with `WUNTRACED`,
`WCONTINUED`, 0 and a negative pid. A shell with job control runs as it
does on Linux. What is different is at the edges:

- **The terminal is checked as on Linux.** A job that reads the terminal
  from the background is stopped (`SIGTTIN`), and so is one that changes it
  from there — `tcsetattr`, `TIOCSWINSZ`, `tcsetpgrp` (`SIGTTOU`) — or
  writes to it when `stty tostop` is set. One that blocks or ignores the
  signal goes ahead, and a read with `SIGTTIN` blocked fails with `EIO`.
  Whoever is in front is sent `SIGWINCH` when the terminal's size changes.
- **A terminal's slave is its session's.** A process outside the session
  that has the terminal — one left running after its own session ended, or
  another user's — gets `EIO` from a read or a write of a descriptor for the
  slave, and `ENOENT` from opening `/dev/pts/N`, where Linux leaves a
  descriptor usable until the terminal is hung up. Before any session has
  claimed a terminal it is its maker's user's, which is what `openpty` and
  `forkpty` need.
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

The clock is the kernel's, in nanoseconds: `clock_gettime` says what it
says, and a wait — a sleep, a timeout, a timer, an alarm — is as long as was
asked and no longer, where the machine can wake a program between two of the
hundred ticks a second it used to wait for. (Where it cannot — a processor
with no counter the kernel trusts — every time is a multiple of ten
milliseconds and every wait ends on a tick, as they all once did.)
`clock_getres` says a nanosecond either way, as Linux does of a clock like
that.

`CLOCK_MONOTONIC` counts from boot and `CLOCK_REALTIME` is the date, read
from the machine's clock at boot as UTC. `clock_settime` and `settimeofday`
set it, for a program that holds the right to — root's shell does, and what
it starts — and are `EPERM` for one that does not. Setting the date moves no
wait: a sleep until a time on the clock that says the date is turned into
how long that is when the sleep begins.

There is no time zone but what `TZ` spells out.

## What a process uses, and how it runs

- **The time a process has run is counted, to the nanosecond.**
  `CLOCK_PROCESS_CPUTIME_ID` is every thread's together and
  `CLOCK_THREAD_CPUTIME_ID` the calling thread's. `getrusage`, `times` and
  `wait4` say the same, divided between the program and the kernel — which
  part was which is sampled, each tick looking at where it finds the
  process, as Linux does when it is not told more — and how many times it
  gave the processor up and had it taken. The rest of a `struct rusage` is
  nought.
- **A child's use is its parent's once collected**: `RUSAGE_CHILDREN`, and
  the children's half of `times`, count every child the process has waited
  for, with what each of those collected of its own. A child nobody waits
  for counts for nobody.
- **`nice`, `getpriority` and `setpriority` are the kernel's**, for a
  process: how nice it is decides its share of the processor against the
  other ordinary programs, by Linux's weights — one at 10 has about a ninth
  of what one at nought has. A child has its parent's and `exec` keeps it.
  Anybody may be nicer; to be less nice takes the right root's shell has,
  and is `EACCES` without it. `PRIO_PGRP` and `PRIO_USER` mean only the
  caller's own process, and are `EINVAL` for any other.
- **`RLIMIT_CPU` is the kernel's**: past the soft limit a process is sent
  `SIGXCPU`, once a second, and at the hard one it is killed, as on Linux.
  A child inherits it, and raising the hard limit takes the same right.
  The other limits are kept nowhere: setting one says it succeeded, and
  asking says what the system has — 64 descriptors — or that there is no
  limit. `prlimit` is for the caller's own process.

## Wide registers

A program compiled for AVX, AVX2 or AVX-512 runs where the processor has
them: the kernel saves each task's, and `__builtin_cpu_supports` — which
asks the processor and then asks what the system saves — says so. Nothing
in the C library chooses a routine by what the processor has, musl having
no such mechanism for a static program; a library that does its own
choosing (pixman, zlib-ng) finds them.

## Where things are

A program's stack, what `malloc` gives and what `mmap` gives with no
address asked for are each somewhere else every time the program runs: a
random number of pages into a window of their own, chosen by `execve` for
the stack and by the C layer, the first time it is wanted, for the rest. A
forked child is in its parent's places. A mapping smaller than two
gigabytes never crosses a multiple of two gigabytes: pixman's stress test
turns bit 31 of an image's address over, adds to it and turns it back,
which is the address only while the image does not. A program's code is where it was
linked: nothing is built to be loaded anywhere (PIE), and `dlopen` has
nothing to load.

## What is refused

A call the layer has no answer for returns `ENOSYS`, on purpose: a C library
told "no" copes, and one handed a made-up answer fails somewhere unrelated
and much later. The ones a ported program is most likely to meet:
`timer_create`, `set_robust_list`, `rseq`, `statx` (musl falls back to
`fstatat`), and `epoll_wait` on anything but the descriptors the layer can
poll.

`socket` is the exception: it answers `EAFNOSUPPORT`, for every family.
There are no sockets to be bound or connected by name — a local stream is
made as a pair (`socketpair`) — and "that family is not supported" is the
answer a program has something to do about. musl asks a name service daemon
who a user is before it concludes nobody has the name, and takes this for
"there is no daemon".

Everything is linked statically. There is no dynamic loader, so `dlopen`
fails, and a library that would be loaded as a plugin has to be built in.
