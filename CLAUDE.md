# Working on quarkutils

Everything that runs on Quark: the runtime (`quark-rt`), `init`, the
nameserver, the drivers, the servers, the C library and the Linux system-call
layer, the shell and the programs. It was `user/` in the kernel's repository
until Phase 18, and its history came with it.

It is one of seven repositories that build together and must be checked out
as siblings:

```
repos/
  quark/       the kernel, and the ABI it installs
  quarkutils/  this repo
  bang/        UEFI bootloader, and nothing else
  quark-toolchain/  the cross compilers: gcc, binutils and musl for Quark
  explosion/   a distro: stages the other three and assembles the image
  gnu-quark/   another: the kernel, four programs and the boot services
               from here, and GNU's programs on top
  rust/        fork of rust-lang/rust carrying the x86_64-unknown-quark std PAL
```

`explosion` and `gnu-quark` refer to `../quark`, `../quarkutils` and `../bang`, and the fork's
`library/Cargo.toml` patches `quark-rt` through the relative path
`../../quarkutils/quark-rt`. Anything other than a flat sibling layout breaks
the build. The dependency runs one way — a distro reaches down, never the
reverse — and **this tree never looks inside the kernel's**. What it knows
about the kernel is the ABI, and `../quark/CLAUDE.md` is where ring 0's own
rules are.

## The ABI, from this side

The contract is the kernel's `docs/abi.md`, which its `make install` puts at
`usr/share/doc/quark/abi.md` beside `usr/include/quark/abi.h`. This tree
carries its own copy of the numbers, twice: `quark-rt/src/syscall.rs` for Rust
and `libc/include/quark/syscall.h` for C. A mismatch is silent and
catastrophic — a program calls one number and the kernel runs another.

- **`tools/check-abi.sh` runs first in every build.** The two copies agree with
  each other and no number is used twice: that half needs nothing but this
  checkout. Given the kernel's installed header, the runtime's table is also
  the kernel's, call for call. `make` looks for that header in `$(DESTDIR)`,
  which is where a distro puts it before building this — never in a path into
  somebody's checkout.
- **Without a kernel the comparison is skipped, and said to be**, so this
  builds on a machine with no kernel on it. `REQUIRE_ABI=1` makes the skip an
  error; ExplOSion sets it, because the build that has both is the one place
  the comparison must not be skipped.
- **The runtime says which ABI it was written against.**
  `quark_rt::syscall::ABI_VERSION_MAJOR` and `_MINOR`, and the check holds them
  to the kernel's header by the rule the version number promises: a different
  major is a different interface; at an equal version the two tables are the
  same table; against a kernel whose minor is ahead, every call here must be
  there and the kernel may have more; a kernel that is behind fails.
- **`init` asks the kernel that is actually running**, before any call whose
  number could have moved, and stops if the major differs or the kernel is
  older than this was built for. The message is on the kernel's own console,
  which is the only thing drawing at that point; on a good boot the line is
  wiped a moment later, when the text console claims the display.
- **A new system call is added in the kernel first**, with its row in the
  document and a new minor version, and here second: the constant in
  `quark-rt` (and the C header if C needs it), and `ABI_VERSION_MINOR` raised
  to the version that has it. The stage fails until both agree.

## Toolchain

Pinned to `nightly-2026-03-01` in `rust-toolchain.toml`. `../quark` and
`../bang` pin the same nightly for reasons of their own; this is the tree
whose pin has to be exactly what it is.

**The pin must equal the commit the fork is based on.** `../rust`'s `library/`
is a checkout of upstream at one commit and only compiles with the rustc built
from it; a newer compiler rejects its own `core` (`impl const Trait for Type`
becomes "expected a trait, found type", features get removed). This is checked
rather than remembered — `make` runs it whenever the fork is on disk:

```bash
tools/std-patches.sh check
# std-patches: the mirror is the fork (16 files added, 29 changed, on 38c0de8dc)
# std-patches: rustc is built from the fork's base (38c0de8dc)
```

The base is `rust-std-patches/BASE`, and by hand it is
`git -C ../rust merge-base HEAD main` against the short hash in
`rustc --version`. If you rebase the fork, move every pin in the same step.

Note the off-by-one — the date in a rustup channel is the *publish* date, so
`nightly-2026-03-01` is the build dated 02-28. A floating `nightly` channel is
what to avoid: it drifts forward and silently leaves the fork behind.

Fresh machine:

```bash
rustup toolchain install nightly-2026-03-01
rustup component add rust-src llvm-tools-preview --toolchain nightly-2026-03-01
rustup target add x86_64-unknown-none --toolchain nightly-2026-03-01
git -C ../rust submodule update --init --depth 1 library/backtrace
```

That submodule is the easy one to miss. Without it `std` fails with
`couldn't read .../backtrace/src/lib.rs`, which stops the build before
`../explosion` can stage anything — so the image keeps whatever it had and you
debug the wrong binary.

The fork is needed only for the hosted programs, `hello` and `httpget`. Without
it on disk, `make` skips those and says so; everything else still builds.

**`rust-std-patches/` is a mirror of what the fork carries, and it is
generated.** The upstream commit the fork left from, one patch for the files
upstream already has, and the sixteen files the fork adds — enough to rebuild
the fork's tree object for object. It lives here because the platform layer is
written against `quark-rt` and has to change with it. Never edit it: change the
fork, then `tools/std-patches.sh sync`, and commit both. `make` fails while the
two differ, which is the point — it was a seed nothing checked once, and ten of
its sixteen files had gone stale under notes that described a plan.

The C programs here (`cwc`, `envtest`), the C library's tests (`ctests/`) and
everything a distribution ports are built with the `x86_64-quark` cross
toolchain, which is a repository of its own: `../quark-toolchain`. Its musl
specs name three things in this checkout by absolute path — `libc/include`,
`linux-abi/src/manifest.o` and `linux-abi/liblinux-abi.a` — so if this
checkout moves, run `../quark-toolchain/musl-wrappers.sh` again.

## Build and run

```bash
make            # every program
make install DESTDIR=<dir>   # stage them for a distro to consume

cd ../explosion
make run        # stage all three trees, assemble the image, boot it in QEMU
```

`make install` lays out `drivers/init.elf`, `boot/` (the services that go in
the boot image), `usr/bin/` and `etc/`; the kernel installs into the same
directory from its own repository and the two do not overlap.

The hosted programs compile `std` from source and are the memory-hungriest
step; an unexplained build death is usually the OOM killer. Note that
`cargo -Z build-std` does not track `quark-rt`, which reaches them only through
the fork's `library/Cargo.toml` patch, so the Makefile hashes the quark-rt
sources and cleans the hosted build when they change. Without that, editing
quark-rt leaves a stale `hello` linked against the previous copy — which is how
it ended up calling pre-Phase-0 syscall numbers and faulting.

`cwc` and `envtest` are relinked on every build. Under the cross compiler they
name no library on their command line — the target knows where its C library
and its link script are — so make had nothing to compare them against, and a
copy linked before the link script changed went on being installed.

`linux-abi` is built by `make` even though nothing here links it: the cross
toolchain's specs name the archive by path, so a stale one is linked into every
musl program silently, and the symptom is a bug you already fixed still
happening. After changing it, relink whatever C programs you are testing.

Only serial reaches the host's stdout; a program's `println!` goes to the
framebuffer. To see user-space output headlessly, screendump over QMP
(`../explosion/tools/boot-test.sh`) rather than assuming the system hung.

## Testing

`dtest` is the kernel's test suite as much as this tree's: 565 checks made from
user space through the ABI, with a recap of what failed before the count. A
check that times out or is refused says which. `runtests <list>` runs the
programs a list names — `/etc/libc.tests`, `/etc/pixman.tests` — and `qfuzz`
throws random requests at every registered service.

**The C library's own tests are here too**, in `ctests/`: one small C program
per lie a ported program has caught the library telling, and `libc.tests`,
the list `runtests` reads. `tools/build-ctests.sh <outdir>` builds them with
the musl compiler; a distribution puts the programs in `/usr/bin` and the
lists in `/etc`. A bug in `linux-abi` gets its test there, in the commit that
fixes it.

All of it runs on a booted image, so verification is a distribution's. With
ExplOSion: `tools/boot-test.sh <keys-file> <shot.ppm>` drives QEMU from a
script of key presses and screenshots, and `tools/check-rootfs.sh` runs
`e2fsck` on the image afterwards. Read a window's position out of a
screenshot before scripting a pointer at it.

## Invariants that must not regress

These were established deliberately. Breaking one silently re-opens a hole.
The kernel's own — paging, ownership of frames, what ring 0 may touch — are in
`../quark/CLAUDE.md`; these are the rules for programs.

- **A spawner gives a program its pages; it does not lend them.** The loader
  builds the image in its own `sys_mmap` memory and moves it across, so the
  child owns its code and stack and frees them when it goes. Lending frames
  with `sys_addrspace_map` kept them the spawner's: a shell leaked every program
  it ran, and a spawner that exited first freed them under its children.
- **The argument page carries the program's own header table** — its end
  belongs to it however long the command line is (`spawn::PHDRS_AT`, mirrored
  in `libc/include/quark/layout.h`). musl finds a static program's
  thread-local template through it, and without it every thread-local lands
  outside its block. So every spawner calls `set_args`, even with no
  arguments; a program reading an argument page that was never mapped faults.
- **C objects must put constructors in `.init_array`.** The cross compiler is
  configured `--enable-initfini-array`, and the user link script places the
  arrays and refuses `.ctors` outright, so an object carrying them has
  constructors that silently never run. `crtbegin.o` and `crtend.o` are on the
  link line, but for the other thing they do: they bracket `.eh_frame` and
  register it, which is how a C++ exception finds its handler. Their own
  constructor is in `.init_array` like everybody else's.
- **A descriptor that says non-blocking must not park the task.** `O_NONBLOCK`
  was accepted and ignored for a phase: the flag set a bit only `sendmsg` read,
  and `read`/`write` always used the calls that park. Every main loop drains
  its wake-up "until it is empty", and empty is a read that answers `EAGAIN`;
  a read that waits there is a program that stops rather than one that fails —
  glib's does it holding its context lock, so nothing else in the program ever
  runs again. `SYS_FD_READ_NB` and `SYS_FD_WRITE_NB` are the calls that answer
  instead, and the same goes for a futex wait with a deadline: dropping the
  timeout makes `g_cond_wait_until` wait for ever.
- **A server copies what a client lent; it never maps a client's page.** Data
  travels with the call (`SYS_CALL_LEND`, then `SYS_LENT_READ` and
  `SYS_LENT_WRITE`), so no server needs authority over physical memory to serve
  anyone. A protocol that names a physical address makes its server a deputy
  that will read or write any page in the machine — that is why the disk, VFS
  and NET servers once held all of it. A shared-memory handle is no better
  when the request names it: handles are global numbers.
- **No task holds a `PhysRange` wider than one device.** init is started with
  the framebuffer and its boot modules, `fb` holds the framebuffer and lends it
  on, and no driver holds any. `dtest physical` walks every CSpace and fails
  otherwise.
- **A program declares what it needs; a spawner grants from that.** Capabilities
  come from a `quark_rt::manifest!` block compiled into the image, found by
  scanning for its magic, not from a table of names in `init`. A spawner mints
  each request from a capability it already holds, so it can never hand out more
  than it has — the shell holds no `PhysRange` and therefore cannot give one
  away. The framebuffer is the one exception: its address comes from the
  bootloader at runtime, so `init` grants it directly.
- **A request a spawner cannot mint is skipped, and nothing says so.** A
  program that needs a capability needs it in every spawner above it:
  `shutdown -r` writes the reset control register (port 0xCF9), and asked
  for it, and the machine turned off instead — the port stopped at `login`,
  which hands the shell its ports one at a time, by hand. It is in
  `getty`'s manifest, `login`'s grants, `qsh`'s manifest and `shutdown`'s
  now. A program that "does nothing" is the first thing to suspect of this.
- **IPC needs an Endpoint capability, and nobody mints one for a stranger.**
  The kernel will not deliver a call the caller holds no `Endpoint` for. Every
  program gets the nameserver's from its spawner, and a lookup grants the one
  for the name; a server calls a client back only with a capability the client
  offered on the request (`sys_call_offer`, `sys_cap_take`), and a claim or
  registration made without one is refused.
- **A program is its address space, not its task.** `SYS_TASK_SPACE` names the
  program a task belongs to with an id the kernel never reuses, and
  `SYS_SPACE_WATCH` says when its last task has gone. Anything a server keeps
  for a program — an open file, a working directory, a lock — is kept by that,
  so every thread of a program shares it and a recycled TID inherits nothing.
- **A pager answers only the kernel.** A call from the kernel to a pager
  carries `PAGER_BIT` in its sender and nothing else can set it. The VFS
  answers `TAG_PAGE_IN` and `TAG_OBJECT_SYNC` only for a sender that has it,
  and replies to the sender as it came — the reply strips the bit and reaches
  the faulting task.
- **No server blocks on one client.** A request that cannot be answered now is
  kept and answered later, rather than waited for: `input` holds a reader
  until there is a line, the framebuffer device gives a claimant a second to
  answer a handover and then goes on without it, and a compositor's writes to
  a client are non-blocking. A server that waits on one client has stopped
  serving every other, and a fuzzer finds that in seconds.
- **A driver answers only the server that claimed it.** The disk driver
  serves the VFS and the keyboard driver serves `input`, each from the first
  claim until that claimant dies, and refuses everybody else (error 5). A
  program that could reach the disk driver could write any sector, and one
  that could reach the keyboard's would read whatever anybody typed.
- **Only the kernel reports a death.** Any program that can call a server can
  send `TAG_TASK_DIED`; what it cannot do is send as sender 0. A server
  believes a notice through `quark_rt::ipc::death_notice` (or
  `space_death_notice`) and answers anything else with that tag as the
  unknown request it is. The nameserver used to forget a service, and `fb`
  give up the console's display, because a program said so.
- **Waiting means blocking.** Programs run in four bands — drivers, servers,
  ordinary programs, idle — and a task in a better band that spins on
  `sys_yield` is immediately runnable again, so nothing below it ever runs.
  `nameserver::lookup_retry` yielded a hundred times between tries and starved
  the VFS out of ever registering. Use `sleep_ticks`, or `sys_recv_timeout` if
  there is also something to hear. A program asks for its band in its
  `manifest!` block, and a spawner can never grant a better one than its own.

`init` spawns `FB`, `CONSOLE`, `INPUT` and `VFS` in passes of their own. If a
program misbehaves for lack of a capability, check that its pass actually calls
`grant_caps_from_manifest` — there is no shared path that does it for them, and
INPUT's pass once granted nothing at all, which the UID bypass hid.

## Starting programs

A program starts one of two ways, and both are ordinary.

A **spawner** builds one: it makes an address space, reads an ELF into its own
memory, moves the pages across, wires the descriptors, hands over the
capabilities and starts a task in it. That is `quark_rt::spawn`, and it needs
authority over nobody — a task the caller created and has not started is its
own to fill, because nothing else can name it, it holds nothing and it cannot
run. `TaskMgmt` buys the unbounded form; without it a program may have as many
children at once as it may have threads.

Or a program **forks** and **execs**, which is what a C program does and what
every Unix program assumes. Both are kernel calls — `../quark/CLAUDE.md` says
what they copy and keep — and the C layer's part is `linux-abi/src/process.c`:
`execve` reads the ELF and builds the new address space exactly as
`quark_rt::spawn` does for a child, then asks the kernel to swap the task into
it.

What a shell does between the two works because **a C program's files are
kernel descriptors** (`linux-abi/src/files.c`). `open` asks the server for the
file *as a descriptor*, and from then on its number is like a pipe's: `dup2`
puts it where standard output was, a forked child has a copy, the program it
execs keeps it, and close-on-exec is the kernel's flag. The layer keeps no
table — only a note of which numbers are files, forgotten whenever something
changes what a number names (`__quark_fd_forget`; a path that installs a
descriptor some other way, as `recvmsg` does, has to say so). Three things
ride along in the same table and so follow a program the same way: its
working directory (descriptor 64), its umask (`SYS_UMASK`), and nothing else.
`open` hands out numbers from 3, where POSIX says the lowest free: 0, 1 and 2
are where a program's standard descriptors go, and a file that landed on one
because it happened to be closed is the bug that rule invites.

A terminal is two kernel descriptors, and the paths that name one are caught
here, ahead of the VFS: `/dev/ptmx` and `/dev/pts/N` in `linux-abi/src/pty.c`,
the way a Linux kernel catches them ahead of its filesystems. musl's `openpty`
and `forkpty` run unpatched on top. So does its `ttyname`, which is a
`readlink` of `/proc/self/fd/N` and then a `stat` of what that said: the
first is answered for a descriptor that is a terminal's slave, and the second
only for a terminal the program has open — opening one in order to describe
it would be a slave opened and closed, and the last slave closing is how a
terminal's master hears that its session has ended. A terminal is one file
however many descriptors there are for it: its inode is the terminal's.

**Every call musl makes comes through the layer, and three of them come by
a different door.** Almost all go through `__quark_syscall`. Three are
assembly in musl because the kernel is asked for something a C function
cannot be written to receive, and the toolchain's patch turns each into a
tail call here: `__quark_clone` (a child that starts on a new stack),
`__quark_fork` for `vfork` (a child that would have borrowed its parent's),
and `__quark_unmapself` (a detached thread giving back the stack it is on,
which it does from one kept for the purpose in `clone-entry.s`). A file of
musl's that still says `syscall` is making Linux's call, by number, at a
kernel that means something else by it — and nothing fails loudly: a
detached thread's exit fell through into whatever came next in memory, and
the thread stayed for good. `ctests/detachtest` and `spawntest` are for
those two. `posix_spawn` is musl's `clone` with `CLONE_VM | CLONE_VFORK`,
and its child is a copy like any other.

**Signals are the program's to run.** The kernel ends a program that has said
nothing about a signal and tells one that has a handler; it calls no handler
itself. `linux-abi/src/signal.c` does: on the way out of every system call
the layer makes for musl, and when a read of a terminal, a poll or a sleep
comes back saying a signal ended it (`QUARK_INTERRUPTED`), it takes what is
waiting and calls the handlers as functions — which is when that wait fails
with `EINTR`, unless the handler asked for it to go on. Three rules follow.
A wait added to the layer that the kernel can end has to handle that answer,
or it returns a count of four thousand million. A default action is the
kernel's to carry out — the layer asks it to (`SYS_SIG_RAISE` at itself),
because a program cannot exit with a signal's status by asking to. And a
program that holds a session's terminal without being what the session runs
says what it does about signal 2: `getty` and `login` ignore it, and `qsh`
handles it so that Ctrl-C at its prompt is a fresh prompt.

**A process id is not a task id.** The kernel gives a dead task's id to the
next task made, and a Unix program assumes a pid it was told a moment ago is
nobody else yet: bash does not wait for a command
whose pid is the last background job's, and after `sleep 2 &` that was every
command that landed in the slot. So in C a process is named by its process id
(`SYS_PID`: the number of the task the program began as, which the kernel
never gives out again) — `getpid`, `getppid`, what `fork` returns, what
`wait4` and `kill` take — and a thread by its task id: `gettid`, `tkill`,
what musl keeps to lock with. The kernel's own calls still take task ids, so
the layer's uses of itself (`SYS_FD_DUP`, a sleep, a change of identity) ask
`SYS_GETPID`, which answers with the *task's* id despite its name, and never
`getpid()`; C that hands `getpid()` to a Quark call is handing over the wrong
number. Rust is unchanged: `quark_rt` spawns, waits and kills by task id, and
`syscall::sys_pid` is there for a program that wants the other one. `ps`
shows both.

**A session runs on a terminal when the distribution says so.** `init` reads
`/etc/init.conf`: `run <path> [arguments]` lines name programs to run to
their end, in order, before anybody is let in — loading the console's font
is the first use — and `session <path>` there names what it starts once the
filesystem is up. With no such file that is `login`, on the console as it
always was: standard input a message to `input`, output the console's pipe.
A distribution that names `getty` gets a real terminal instead: `getty` asks
the console for its pty (`TAG_TTY_OPEN`), opens the slave, and runs `login`
with it as descriptors 0, 1 and 2, so that everything below has a tty —
`isatty` is true, `tcsetattr` works, and a shell that edits its own command
line can turn the echo off. `getty` keeps the slave open between sessions, so
the terminal never sees its last holder go. `login` reads `/etc/passwd` in
Unix's seven fields or the five this began with, and starts any shell but
`qsh` the way a Unix login does: in the home directory, with `HOME`, `USER`,
`LOGNAME`, `SHELL`, `PATH` and `TERM`, under a name with a dash in front.

**`getty` is what makes that a session**, in the sense job control needs.
It begins one (`sys_setsid`) and takes the terminal as the session's own
(`sys_pty_set_session`), which gives the terminal a process group in front
of it: `getty`'s, in which `login` and whatever it starts all begin. A shell
that does nothing about groups — `qsh` — leaves it at that, and Ctrl-C is
for the lot of them, as it always was. A shell with job control puts itself
in a group of its own and in front, and each job after it. Two things follow
for whoever is *not* the shell:

- **`login` takes the terminal back when the shell ends**, and `getty` when
  `login` does (`sys_pty_set_front`, quietly). The group in front is the one
  that has just gone; until somebody is in front again, nothing typed is for
  anybody and a read from behind is not a read — `login` would print its
  prompt and be refused the answer for ever.
- **Ctrl-Z does nothing at `qsh`**, by the kernel's rule and not by anybody
  ignoring it: a group with nobody to continue it is not stopped from a
  terminal. Nothing here needs to say what it does about signal 20.

## The screen

`fb` is the framebuffer device: it owns the hardware the way `/dev/fb0`
does, knows the mode, and decides who draws. It has no opinion about windows.

Everything else is a client of it. `qtty` is the text console: it claims
the display at boot and draws fullscreen — that is what the machine boots into,
a plain TTY. To the services started before there are users it is a pipe, and
what they write down it is drawn. To a session it is a terminal: asked
(`TAG_TTY_OPEN`), it makes a pseudo-terminal, keeps the master, draws what
comes out of it and types into it what is typed. It holds the keyboard for
that the way a compositor does — a claim on `input`, under the compositor's
when one is running — and turns keys into the bytes a Linux console sends for
them. It gives its terminal to the first program that asks and to nobody else
while that one lives: whoever holds the slave reads what is typed. What it
understands of ECMA-48 is what `qtty/termcap` says, which is installed as
`/etc/termcap`: a capability goes there when the console acts on it and not
before. A sequence it does not act on is read to its end and dropped, never
drawn.

**The console is UTF-8.** A cell holds a character and not a byte, and three
things about that are rules:

- *A character is as wide as `wcwidth` says.* A program lays out its output
  by asking its C library, and a console that disagrees draws the cursor
  where the program does not think it is. `qtty/src/width.rs` is generated
  from the Unicode data (`tools/gen-width.py`), like the C library's own
  table, and is regenerated when that moves. Two cells for most of East
  Asia; none for a combining mark, which is dropped, because a cell has room
  for one character.
- *What a character looks like comes from a font the console is given.* It
  was built with ASCII and has nothing else. `setfont FILE` reads a font in
  GNU Unifont's `.hex` format and lends it to the console a piece at a time;
  `init` runs it at boot when `/etc/init.conf` has a `run` line for it. The
  console does not read the file itself: a server that called the file
  server would be waiting on something that may be waiting to print on it.
  It takes a font from root only — whoever draws the characters can make a
  prompt say anything. A character no font has is drawn as its nearest
  ASCII, or as a box, and never as nothing.
- *The screen is read by what it was drawn in.* The tests read a screenshot
  back as text by matching cells against glyphs, so a test of an image that
  loads a font names that font (`QUARK_FONT_HEX`).

The keyboard still types ASCII: there is one layout, and it is US. The
kernel's line discipline knows a character from a byte (`IUTF8`) and rubs a
whole one out.

`wm` is a compositor you *run*: `wm <program>` takes the
display, starts that program, composites its windows, and gives the display back
when it exits.

`wm` speaks **Wayland**, not a protocol shaped like it. It hands each program a
socketpair end as descriptor 3 and `WAYLAND_SOCKET=3`, which is what
`wl_display_connect` looks at first — so upstream libwayland runs unpatched.
`wm/src` is one module per part of that: `client` (a connection and its
buffered bytes), `objects` (one id table per client, which is what makes "a
client cannot name another client's objects" true by construction), `surface`,
`shell`, `shm`, `seat`, `grab` (what the pointer is doing between a press on
the compositor's own furniture and the release that ends it), `clipboard`,
`cursor`, `keymap`, `protocol`, `draw`.
`wmdemo` and the older six-tag window protocol still work alongside it.

The pointer has a wheel, and it reaches a client as `wl_pointer.axis` with the
group the version-5 events describe: `axis_source` says it is a wheel,
`axis_discrete` gives the click count, the axis carries ten units per detent as
Weston sends, and a `frame` ends the group. It starts at the i8042: a PS/2
mouse says nothing about a wheel until it is asked, and what asks is the knock
in `keyboard` — sample rate 200, then 100, then 80 — after which the
device calls itself 3 and sends four bytes instead of three. The packet length
comes from that answer and not from hope; reading a fourth byte from a mouse
sending three loses the stream for good.

The keyboard goes with it, and so does the pointer. `input` has the same
claim protocol: while a program holds it, raw key and pointer events go to that
program and line readers wait. The compositor claims input when it claims the
display and hands each event to the focused window — whoever owns the screen
owns the keyboard, the way switching virtual terminals has always worked.
With nobody holding it, `input` cooks keys as they are typed: the driver
notifies it of each one, and a finished line waits for a reader. It never
waits on the keyboard itself, so a reader waiting for a line holds up nobody
else's request.

Both come from one driver. A PS/2 mouse is not a second device: it is the same
i8042 answering on the same data port 0x60, with IRQ 12 instead of 1 and bit 5
of the status port saying which device a byte came from. `keyboard` holds
both lines and routes on that bit — never on which interrupt fired, because a
byte for one device can be waiting when the other's interrupt arrives. Two
drivers sharing port 0x60 would take each other's bytes, and the symptom of
losing that race is a keyboard that types rubbish or stops.

Some things to know before changing any of it:

- **The display is a stack, and so is the keyboard.** A claim goes on top and
  displaces the one below, which gets it back (`TAG_FB_GAINED`) when
  everything above has let go; a claimant further down that releases or dies
  just leaves the line. So `wm "wm <program>"` unwinds to the outer session
  and then the console. A compositor that loses the display unmaps it and
  waits, keeping its keyboard claim below the new one, and repaints all of
  the screen when the display comes back. Eight deep, then claims are
  refused.
- **The display is lent, not shared.** `init` grants the framebuffer
  `PhysRange` to `fb` and nowhere else; `fb` mints a derived capability per
  claimant and revokes it to take the display back. Revocation governs the
  right to *map*, not mappings that already exist, so the outgoing owner is
  told and answers before the new one is let in — and must empty its
  capability slot, since granting into an occupied one fails.
- **Guard every framebuffer write on still owning the display**, not just the
  flush. The console gated its flush and not `hide_cursor` or `scroll`, and
  carried on writing into memory it had just unmapped.
- **Composite into a back buffer.** Painting onto the visible surface means the
  cleared screen is briefly the one on the monitor, once per frame; a cursor
  blink is enough to make that a visible flash. `sys_mmap`/`sys_munmap` take at
  most 256 pages, so a screenful takes a loop.
- **Repaint the region that changed, not the screen.** A commit says which
  window changed; repainting all of it costs a megapixel of backdrop and a
  four-megabyte copy, which a client committing a dozen times a second turns
  into a compositor with no time left to read the keyboard. `wm` clips every
  drawing primitive to a region and copies only that region out.
- **A client's request is read inside the request.** Every argument comes
  through a cursor bounded by the size in the message's own header, and
  anything that cannot be honoured — an opcode the interface does not have, an
  object that is not there or is not what the request needs, a string that
  does not end in a NUL, a `bind` above the version advertised — is a
  `wl_display.error` naming the object and the reason before the connection
  ends. Reading straight from the buffer took the next message's bytes, or the
  last read's, as arguments a client had not sent.
- **A slot is not freed while an object still names it.** `xdg_toplevel.destroy`
  takes the role away and leaves the surface, because the client's
  `wl_surface` still names it; destroying the surface takes away every object
  of that client's that named it. A surface slot freed under a live name is a
  slot the next client's surface takes — with the first client still able to
  attach to it. Buffers count the surfaces showing them rather than carrying a
  flag, for the same reason, and a pool is unmapped by what was mapped rather
  than by what the client called a pool.
- **What the compositor has, each client has a share of.** Pools, buffers and
  surfaces are shared tables, so one client may hold a quarter of each: a
  client asking for them in a loop is a client, not a compositor.
- **Between a press on the compositor's own furniture and the release that
  ends it, the pointer is the compositor's.** That is a grab (`wm/src/grab.rs`),
  and while one is on no client hears a motion or a button — the movement is
  not about them. A press on the title bar moves the window, one within `GRIP`
  of an edge or corner resizes it, one on the close box asks the client to go,
  and two on the bar within half a second fill the screen. A grab ends when the
  button comes up, when the window goes, or when its client disconnects; the
  last two are one thing, and `destroy_window` says so before it frees the slot,
  or the grab would go on moving a window somebody else has since been given.
  `xdg_toplevel.move` and `.resize` start the same grabs for a client that
  draws its own decorations, and are refused unless a button is actually down —
  a grab with nothing held ends at the next release or never, which is a client
  taking the pointer away from whoever is using the machine.
- **A size is agreed, not imposed.** The compositor never resizes a window
  itself: it sends `xdg_toplevel.configure` with a size and the states, then
  `xdg_surface.configure` with a serial, and the window follows whatever buffer
  the client attaches. A client that ignores the pair keeps the size it had and
  nothing waits for it. The two halves go together — one without the other
  leaves a client waiting for a serial that never comes — and a surface accepts
  any serial from the oldest unanswered one up to the newest sent, because a
  resize sends one per tick and answering one supersedes the older ones. A
  compositor that insisted on the newest killed a client for being a frame
  behind.
- **A version is advertised only when every event of it is sent.** `wl_seat` is
  5 because `wl_pointer.frame` and the axis events go out; `wl_compositor` is 4
  — the buffer transform, the buffer scale and `damage_buffer` are read and
  checked, and `wl_surface.enter`/`leave` are the only events up to it —
  because weston's toytoolkit binds it at 3 with no negotiation and a
  compositor offering less is one every weston client dies against; `wl_output`
  is 2 for `scale` and `done`; `xdg_wm_base`, `wl_shm`, the decoration manager,
  `wl_data_device_manager` and the primary selection are 1. The
  clipboard stops at 1 deliberately: 2 and 3 are drag and drop. An object made
  from another inherits its version, which is how a client that bound
  `wl_seat` at 4 gets a `wl_pointer` with no `frame`.
- **Say why a client was killed.** A protocol error is fatal to a connection
  and most programs die of one in silence: libwayland hands the reason to the
  program, and the program exits. `weston-terminal` exited three times without
  a word before the compositor started printing what it had refused.
- **Events are pulled, not pushed.** A server calls a client only when the
  client asked it to and handed over the right to — `fb` and the keyboard are
  offered an `Endpoint` with the request that needs one. Otherwise it answers:
  a call blocks until the client replies, so a slow client would stall the
  server, and one that is itself calling the server would deadlock with it. A
  reply needs no capability, so every other hop here is the client asking.

## Disks

A disk driver serves *volumes* (`quark_rt::block`): volume 0 is the whole
device and volume N its Nth partition, read from the GPT, or an MBR if
there is no GPT. A request names a volume, and its sector numbers count
from that volume's start; the driver refuses what is past its end. So a
client given a partition cannot reach outside it and does not know where it
is. `disk`, the ATA driver, registers as `disk0`.

- **The protocol is one module, for both ends.** `block::serve` is the
  driver's half — volumes, claims, the partition table — and a driver
  supplies only where the sectors are (`block::Device`). A second kind of
  disk is a second `Device`, not a second copy of who may read what.
- **A volume has one writer at a time**, which *claims* it. Writes are
  answered to the claimant and nobody else, and a claim goes when its holder
  does. Reads are answered to the claimant and to root: looking at a disk
  somebody has mounted takes nothing from them, and it is how `disks` says
  what is on one. The whole device and a partition of it are the same sectors:
  two different clients cannot hold one each. That is what stops a mounted
  filesystem being written under its server.
- **Only root claims.** A capability to call a driver is handed to anybody
  who looks its name up, so being able to call cannot be the authority. The
  driver asks the kernel who the caller is.
- **The partition table is read again when whoever holds the whole device
  asks**, and not while any partition is claimed: its holder was told where
  it is.
- **A RAM disk is a disk** (`ramdisk`): the same `block::serve`, with pages
  where a platter would be. `ramdisk MEGABYTES` makes an empty one out of
  its own memory, for anything that wants a disk nothing depends on — the
  tests do. `ramdisk module PHYS BYTES` serves a file the bootloader loaded,
  and is how a system runs from memory: when there is a boot module called
  `live.img`, `init` starts `ramdisk` on it, grants it that memory and no
  other, gives up its own right to it, and tells the file server its root
  is `ram0`. Each registers as the first of `ram0`..`ram7` nobody has.
- **The file server is told what to serve**: `vfs DRIVER VOLUME`. `init`
  passes what the boot module `root.cfg` says (`root disk0 2`), which is how
  an installed system finds a root that is not where an image built
  elsewhere puts it. With nothing said the server takes volume 2 of
  `disk0`, or the only partition, or a device with no table at all.
- **A disk is also a file** (`vfs/src/devices.rs`): `/dev/disk0`,
  `/dev/disk0p2`, `/dev/ram0`, for programs that want `pread` and `pwrite`
  — the ones that make filesystems are C. The file server claims the volume
  from its driver while anybody has the device open to write, so the
  driver's rule is the rule here too: what is mounted cannot be opened to
  write. The exception is the file server's own root, which it holds
  itself and so has to refuse itself. Opening to read claims nothing: the
  first version claimed for every open, so `stat` of a mounted partition
  was refused as busy and one look at the root's disk left it claimed for
  good. A claim is given back when the last handle that writes closes —
  `handles.rs` has one place every way a handle goes comes through,
  *including a handle that was never made*, because a volume left claimed
  is a disk nobody can format. And a handle remembers its driver as a
  program, not as a task or a name: a RAM disk is killed and another takes
  both.
- **Writing a disk is slow here, and it is the emulator.** The ATA driver
  writes with programmed I/O, and under a hypervisor every write to the
  data port is a trap: about 0.8 MB a second, however the data is sent.
  `rep outsw` is the slowest way — it goes through an instruction emulator
  — so the driver writes 32 bits at a time, one instruction each, which is
  nearly three times as fast; and it writes a run of sectors as one command.
  Reads are forty times faster, because a hypervisor reads ahead for
  `rep insw`. The answer is DMA, and it has not been taken on purpose: a
  bus-master device writes wherever its driver points it, so DMA with no
  IOMMU gives the disk driver all of physical memory, which is exactly what
  it was built not to have. Until the kernel can confine it, what is slow is
  made up for above the driver: fewer sectors, not faster ones.
- **`qfuzz` does not send a disk driver a claim.** It runs as root, a volume
  nobody has is one it would be given, and its next random write would be a
  write.

## Files

`vfs` serves ext2, ext4 and FAT32; `docs/vfs.md` is its protocol. What a
C program sees goes through `linux-abi`, which turns descriptors and
Linux's calls into that protocol. `tools/check-rootfs.sh` in ../explosion runs
`e2fsck` on the image a boot test just used, and it is the check for any
change here: it has found what reading the code did not.

- **A handle names an inode, never a copy of one.** The inode is read when the
  handle is used, so two handles on one file agree about its size and blocks,
  and one cannot write through a block map the other just shortened.
- **A handle is its program's, and closes when the program does.** The server
  names a program by its address space (`SYS_TASK_SPACE`) and watches each one
  it gives a handle to, a working directory or a lock. A file whose last name
  went while a handle or a working directory held it is freed when that goes,
  not before.
- **A file removed while in use is on the disk's orphan list** (`s_last_orphan`,
  each inode's `i_dtime` naming the next), in the same transaction that took
  its last name, and comes off it before it is freed. The server frees what a
  stopped machine left there before it answers anything. Deletion times are
  kept above the inode count so that no freed inode reads as a link in that
  list. `tools/crash-test.sh` stops a machine with one on the list.
- **A symbolic link's text is not a block map.** A target under 60 bytes lives
  in `i_block`; freeing, truncating or mapping such an inode as if it held
  block numbers frees whatever blocks the text spells.
- **`/dev` is the server's, whatever the disk holds.** The lookup answers for
  the root's `dev` directory itself, so no path — through links, or relative —
  reaches the disk's copy, and nothing is made there.
- **Paths are lent, never cut.** A path up to 4095 bytes travels in a buffer
  lent with the call, and a longer one is refused. The old requests carried
  paths in the message and truncated them, which opens a different file.
- **A directory's times change with its entries**, which is how fontconfig
  knows its cache is stale.
- **A file opened as a descriptor is the kernel's to count.** The server
  makes the descriptor (`SYS_FD_SERVE`), believes a request that names its
  handle only after asking the kernel whether the caller holds it
  (`SYS_FD_HOLDS`), keeps the position — shared by every copy, which is what
  `{ a; b; } > file` needs — and closes the handle when the kernel says the
  last descriptor has gone. `dup`, `fork` and `exec` never reach it. A handle
  opened the old way, by a program for itself, is still its program's.
- **A named pipe is the server's name for a pipe the kernel keeps.** The
  inode is a name, an owner and a mode; it has no blocks, and on ext4 no
  extent tree, so nothing that walks a file's blocks may be pointed at one.
  Opening it for reading or for writing checks the mode and hands the caller
  an end of the pipe the kernel has for that inode number
  (`SYS_FD_SERVE_PIPE`). The server never sees the bytes or the close, and
  never waits for the other end — the opener does (`SYS_PIPE_PEER`), in its
  own time, with the number the open came back with. That number is what
  makes the wait right: a writer that opened, wrote and closed between the
  open and the wait has still been.
- **A mapping asks what the descriptor was opened for.** A descriptor opened
  to read is not one that writes because it was mapped shared.
- **A write is answered before it is recorded, and only a write is.**
  `deferred` (in `vfs/src/main.rs`) leaves a file write's transaction open
  for the next write to join; everything else is `transacted` and commits
  before it returns, taking any waiting writes with it. That asymmetry is
  the safety of it: a write only allocates, so nothing that *frees* a block
  ever waits, and a block cannot be given to a second file while the disk
  still says it is the first's. The loop commits what is waiting after a
  fiftieth of a second with nothing asked — and after half a second whatever
  is being asked, since a server answering reads is never quiet — `TAG_SYNC`
  commits it on demand,
  a mounted filesystem's server commits before it ends, and `shutdown` asks
  for a sync before it ends the servers. A new request that changes the
  filesystem goes through `transacted`, never `deferred`.
- **Whole blocks of file data go round the journal**, straight to their
  blocks, before the transaction that names them commits. A block the open
  transaction holds is the exception (`journal::holds_block`): the disk's
  copy of that one is not the latest, and a write to it goes through the
  transaction or is undone at the checkpoint.
- **A structure with a checksum goes to the disk in one request**, and the
  ATA driver hands the drive a request as one block (WRITE MULTIPLE). The
  superblock is two sectors with its checksum in the second; written as two
  requests — or as one, to an emulated drive that writes each sector as it
  arrives — a machine stopped between them has a superblock nothing accepts,
  and `e2fsck` falls back to the copy made at `mkfs` and "repairs" everything
  since. The journal cannot help: its replay is decided by reading that
  superblock. `tools/crash-test.sh` in ExplOSion found it by stopping the
  machine at many moments instead of one, two stops in twenty-four. No ATA
  command promises a write is all or nothing — a real disk gets it from
  sectors that are physically four kilobytes — so this is the case removed
  that there was no need to have, not a guarantee.
- **A journaled write never lets a prefetch cache the old copy.** While a
  transaction holds a sector, a read ahead skips it; caching what is on disk
  under it lost a rename on ext4.
- **A write allocates every block in its range, holes included.** A
  truncate that lengthens a file leaves holes, and ext4 keeps the extent root
  in logical order so that a block written into one is where a read looks.

## Toolkits

The ports themselves are ExplOSion's — `../explosion/toolchain` builds every
one and its `README.md` says how — but what they stand on is `linux-abi`, here,
and what each needed was added to this tree or to the kernel.

Above the font stack there is now a whole GNOME-shaped one, built for Quark and
running on it: **glib** (with GObject, GIO, a main loop and a thread pool),
**harfbuzz**, **fribidi**, **pango**, **graphene**, **gdk-pixbuf**, and
**GTK 4**. `wm hello-world` draws GTK's own `examples/hello/hello-world.c`,
unmodified, in a window — and prints "Hello World" when the button is clicked.

The rules that got it there, and that a further port should follow:

- **Nothing patches an upstream library.** Everything each one needed was added
  to Quark: `eventfd`, a futex wait that honours its timeout, an `O_NONBLOCK`
  that means it, a `poll` with no descriptors that waits, a spawner that can
  read a program bigger than four megabytes, and a compiler that admits this is
  a Unix. Teaching a package's `config.sub` the word `quark` is not a patch to
  the package; it is a patch to autoconf's idea of what operating systems
  exist.
- **Static, and non-PIC.** There is no dynamic loader here, so
  `-Ddefault_library=static -Db_staticpic=false` is on every meson build and
  the compiler wrapper drops `-fPIC` whatever a build system says. A module
  that would be `dlopen`ed has to be built in instead — gdk-pixbuf's loaders
  are, which is also why no loader cache is needed.
- **glib is built twice.** Three of its tools are C programs rather than Python
  — `glib-compile-resources`, `glib-compile-schemas` and `gio-querymodules` —
  and GTK's build runs two of them to turn XML into C. The copies in the target
  prefix are Quark binaries and cannot run on the build machine, so a native
  glib lives in `$QUARK_HOSTDEPS` beside the host expat, and every build script
  puts it on PATH first.
- **There is no OpenGL.** GTK links libepoxy whatever it draws with; epoxy was
  built to look for a GL implementation at run time and correctly finds none,
  so GSK falls back to its cairo renderer. A Mesa software rasteriser is a
  project of its own and is not this one.
- **GTK 4 has no static build.** `gtk/meson.build` says `shared_library` with
  no choice about it. What it also has is the `static_library` the shared one
  wraps, so `build-gtk.sh` builds those and `build-gtk-client.sh` links a
  program against them — the same shape as the weston toytoolkit port.
- **A toolkit program is twenty-five megabytes**, and the spawner reads the
  whole image into its own memory before giving the pages away, so it is in
  memory twice while it starts. QEMU gets a gigabyte and the root filesystem is
  128 MiB.

## Mounts

**A mount is a server.** `mount /dev/disk0p2 /mnt` starts a second file
server — `vfs disk0 2 mount`, the same program — and hands it to the server
`/mnt` is in, which stands between it and everybody else from then on
(`vfs/src/mounts.rs`). A path that walks into the directory goes on in the
other server; a file opened there is a handle here that names a handle
there, and reading it is asking the other server to read. Nothing outside
the file servers knows: a client calls the server it always called, a
descriptor is the root server's, and a program built before any of this
reads a file on a mounted disk. `docs/vfs.md` has the requests.

- **The root is nobody's to adopt.** A mounted filesystem's server believes
  the server above about whose request it is passing on (`TAG_IDENTITY`),
  and ends when that server does. Only a server started to be mounted
  accepts `TAG_ADOPT`. The first version let anybody say it to the root:
  any program could then speak as user 0, and the root's file server ended
  when that program did. `qfuzz` sends all of them, and `dtest mounts`
  checks the refusal.
- **A mounted filesystem's server has no `/dev`.** The devices are the
  root server's (`devices::disable`). A file server treats `dev` at its own
  root as theirs, and one serving a mount did too: `/mnt/dev` could not be
  made, and installing a system into a mounted root stopped at its first
  directory.
- **A mounted filesystem's server has no name.** It is not registered, so
  nobody can look it up and call it: the capability for it goes from the
  program that started it to the server it is mounted in, with the request
  (`SYS_CALL_WITH` lends the path and offers the capability at once), and
  that is the only copy given out. Permissions there are checked against
  what the server above says, so a second way in would be a way round them.
- **A request is taken before its handler if it crosses** — and there are
  two places, because a descriptor's read is turned into a positioned one
  before it is dispatched: `mounts::intercept` in the loop, for requests
  that name a path and for what servers say to each other, and
  `mounts::serve` in `dispatch`, for reads, writes, `STAT`, listings and
  `TRUNCATE` on a handle that stands for another server's. A new request is
  one more thing to place: if it names a path it goes in `crossing`, and if
  it names a handle, something has to say what it means for a `Remote`.
- **Handlers never see a path that leaves.** `ext2_dir::resolve` stops at a
  directory something is mounted on and says `ERR_ELSEWHERE`; `locate` asks
  it first, with the handler's own idea of whether the last component is
  followed, and a handler runs only for a path that stays. That code is
  never sent to a client. With nothing mounted none of it runs.
- **A handle here closes its handle there**, through the same `gone` that
  lets a disk's claim go — including the handle that was never made.
- **A server that stops answering has gone.** Every request passed on has a
  deadline of a minute, and a server that misses it, or cannot be called, is
  marked dead: everything through that mount is an error until `umount`
  takes the entry away. A lent buffer of no length is not lent — the kernel
  refuses one, and a call that could not be made is not a dead server.
- **What is offered as a server is asked whether it is one**, with a third
  of a second to say so. A task that is not a file server would not answer
  at all, and the file server would wait with everybody waiting on it.
- **An id is folded as it comes up**: the mounts a file is under are written
  above the fortieth bit, four bits to a mount. `STAT`, `OPEN`'s reply and
  directory entries all do it, or `d_ino` and `st_ino` disagree.
- **`/etc/mtab` is a file.** `mount`, `umount` and `init` (at boot) write
  it from what the file server says is mounted, for the programs written
  for Unix that look there: `mke2fs` refuses a mounted disk by reading it,
  and complains when it is missing.

At the edge of a mount two things are not as one kernel holding every
filesystem would have them, and both are in `docs/vfs.md`: `..` at a
mounted filesystem's root leads out only where the server above can see it
coming, and an absolute symbolic link inside a mount is followed from the
mount's root. A file there cannot be mapped, and a named pipe there cannot
be opened.

**Nothing has run as another user through a mount.** Every test is root,
because nothing here can start a program as anybody else without `login`.
The path that carries a caller's identity down is read, and short; it has
not been run with an identity that would be refused. That is owed when
there are users to test with.

FAT32, which had only ever been a root nobody wrote much to, is what an EFI
system partition is, so it had to be right enough to mount one and have
`fsck.fat` find nothing: a file with nothing in it has no cluster (one with
a cluster and no length is an error to a checker), the count of free
clusters the filesystem keeps is brought up to date after every request
that changes it, the search for a free cluster starts where the last one
ended and stops at the last cluster the volume has rather than the last
the table has room for, and a file can be shortened, written over and
removed.

## Known gaps

- Servers still know their clients by TID (`Message.sender`). The kernel will
  not deliver a call the caller had no capability for, but a server that keeps
  a client's TID past one call — a lease, a registration, a foreground task —
  must watch it with `sys_task_watch` and forget it on death. Otherwise it
  treats whatever takes the TID next as the same client.
- Focus is a single stack with little policy: Tab cycles, a new window takes it,
  and a click raises the one under the pointer. Keyboard focus and pointer focus
  are tracked separately, as Wayland requires, but there is no follow-mouse and
  no focus stealing prevention.
- **A handler runs at a system-call boundary and nowhere else.** A C program
  has `sigaction`, a mask, `kill`, `EINTR` and SIGPIPE (`linux-abi/src/
  signal.c`), and one that handles a signal and then computes without a call
  is not interrupted by it. Nothing is raised when a terminal changes size.
  `alarm` and `setitimer` are the kernel's one alarm for a program, in real
  time, to the tick: the timers that count time spent running are refused,
  and so is `timer_create`, which every program asked falls back from. A
  signal that is blocked and has no handler is not held back. The mask is the program's
  rather than a thread's, and is not kept across an exec.
- **A program's first thread is not numbered as its process.** `getpid` is
  a process id, 64 or more, and `gettid` a task id, below 64. On Linux the
  two are equal in the first thread, and code that finds its main thread by
  comparing them, or signals it with `tgkill(getpid(), getpid(), …)`, is
  wrong here. Nothing ported so far does either. `docs/c-library.md` is
  where this is said to somebody porting a program — with everything else
  about C here that is not as Linux has it — and a deviation the layer gains
  is written there in the commit that gains it.
- The **plain console has its own Ctrl-C**, older than signals: `input` sends
  the foreground task one of the kernel's three task signals (`SYS_SIGNAL`),
  which `qsh`'s `kill` and `shutdown` use too. On a terminal it is signal 2,
  from the line discipline.
- A program started by `fork` and `exec` **holds what its parent held**: the
  kernel copies capabilities at a fork and keeps them across an exec, and
  nothing narrows them to what the new program's manifest asks for, as a
  spawner does. A shell that execs is as trusted as everything it runs.
- The compositor keeps no history of serials, so `xdg_toplevel.move` and
  `.resize` cannot check that the serial they are given was a recent press.
  What they check instead is that a button is down. Drag and drop, touch and
  key repeat as a compositor policy are all still missing, and so is any way to
  put a window somewhere other than on the one screen: fullscreen and minimise
  are read and ignored.
- **`wl_shm_pool.resize` is refused.** A pool may only grow, and growing means
  new memory, which means a descriptor the request does not carry; a client
  that drew past the old end would fault the compositor. A client that needs a
  bigger pool makes a new one and lets the old go after the commit that
  replaces it — which is safe because a buffer destroyed while it is being
  shown becomes a zombie and its pool stays mapped until nothing shows it.
  Toolkits do call `resize`, so this is a real gap rather than a preference.
- `O_CREAT` through a symbolic link whose target does not exist says EEXIST,
  where Linux makes the target, and `linkat` cannot name its source by
  descriptor (`AT_EMPTY_PATH`). FAT32 has no links.
- FAT32 cannot rename anything or make a file longer except by writing to
  it, and ext4 refuses to shorten a file whose extent tree has grown past
  the inode.
- A mapped file's pages stay cached until nothing maps the file any more, and
  the VFS pages 30 objects at once. A private writable mapping copies a page
  when it is first touched, read or write.
- `mprotect` says yes and does nothing: a mapping is made with the protection
  it will keep, so a program that maps read-only and then asks for write gets
  a mapping that still faults on the write. Shortening a file does not take
  away pages of it a program has already mapped past the new end; what it does
  is stop new ones being filled from beyond it.
- `std::fs` is not implemented for this target: a hosted Rust program reads
  and writes through descriptors it is given, not through `File::open`. Nor are
  `std::process` or `std::io::pipe`, and `std::env` sees no variables — the
  spawner passes an environment and std does not look. The fork has files for
  the first three that nothing selects; `rust-std-patches/README.md` says
  which. C programs have the whole of the C library's interface.
- `flock` and `fcntl` locks are one kind here, so the two can keep each other
  out where Linux keeps them apart. Locks live in the server's memory, 256 at
  once.
- A program has 64 descriptors, files included; the VFS has 512 handles for
  everybody, and 128 for any one program. A pipe, a terminal and a stream all
  say they are a character device to `fstat`: nothing tells the layer what
  kind a kernel descriptor is.
- **No OpenGL, no D-Bus, no `dlopen`.** GTK starts without any of them and says
  so: `g_module_symbol` complains about a NULL module twice, the session bus
  cannot be reached, and GSK draws through cairo. Each is a real absence rather
  than a stub, and each is a thing a bigger application may ask for and not get.
- **Quark has no dma-buf**, and the Linux uapi headers copied wholesale into
  the sysroot said it did until `linux/dma-buf.h` was taken out of them. Every
  other header there describes something a program can ask for and be told no;
  that one is asked at *build* time, and a yes makes a toolkit compile a path
  that cannot work.
- The shell cannot set a variable for one command — there is no
  `VAR=value program`, only the four in `BASE_ENV`. Nothing has needed it yet,
  because GTK falls back to the cairo renderer by itself, but the next program
  that wants an environment variable will need the shell to grow one.
- The rust fork is a short branch, `quark`, on upstream's `main`. Rebasing it
  means re-checking the PAL against std's internals, which move: the allocator
  PAL shape, the futex module location, `RawOsError`'s home and
  `BorrowedCursor`'s parameters have all changed under it before.
