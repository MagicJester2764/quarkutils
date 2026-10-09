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
A program linked to `libc.so` relinks nothing: it has the copy of the layer
that `../quark-toolchain`'s `build-musl.sh` last put in the shared object,
so build the layer position-independent (`make -C linux-abi pic`) and that
again.

Its objects, and `libc`'s, are built again when *any* header changes
(`$(OBJS): $(HEADERS)` in both Makefiles). The system calls are inline
functions in `libc/include/quark/syscall.h`, so that header is code in
every one of them. For a long time only a `.c` that was itself touched was
compiled again: the header was changed so that a call passes zero for an
argument it does not give, two files were built with that and eight were
not, and nothing showed until the kernel read a second argument to a call
that used to take one — then `raise` failed in every C program. A new
Makefile for C gets the same line.

Only serial reaches the host's stdout; a program's `println!` goes to the
framebuffer. To see user-space output headlessly, screendump over QMP
(`../explosion/tools/boot-test.sh`) rather than assuming the system hung.

## Testing

`dtest` is the kernel's test suite as much as this tree's: 1072 checks made
from user space through the ABI on a machine of one processor with nothing
more to ask about — four of them of registers only some processors have,
and not made where there are none — eighteen more on four processors
(`dtest placement` wants two, `dtest offline` four), and more on a machine
with more, below: 1109 on the machine ExplOSion tests on, which has `swapd`
and `edu`, and 1127 on it with four processors. A recap of what failed
comes before the count. A
check that times out or is refused says which. It is run on one processor
and on several (`SMP=4` to a distribution's `boot-test.sh`); `dtest smp` is
the part about what a second processor changes, and passes on one. `dtest
clock` is about time: what the clock says, and whether a wait ends when it
was asked to. A check that a wait is *on time* asks that most of several
are — one of them is on time by luck on a machine that wakes only on its
tick, and on any machine one is sometimes late — and is not made at all
where the clock is the tick, which `dtest` says. `dtest fork` is about a
fork that shares what the program has and copies a page when one side
writes it: who sees a write, what it costs, and every way to a page that is
not a write through it — the kernel writing for the program, a server
writing what it was lent, a word a thread is waiting on. What it costs is
measured after one fork that is not counted: the first task the kernel
makes room for costs it room it then keeps. `dtest pressure` is about
memory given back when there is not enough: it needs `swapd` running
(`start /usr/bin/swapd /var/swap 16` in `/etc/init.conf`) and makes one
check without it and twenty-six with. Most of it asks for what the
kernel otherwise does only when it must — a program may give up pages of
its own (`sys_page_out`) — because which pages a machine short of memory
takes is not something a check can count on; the last of it is the thing
itself, a program that wants more than there is (`dchild fill`), and then
four threads of one that touch the same new pages where memory runs short,
and wait for it together (`dchild crowd`): each takes about ten seconds on
a disk driven a word at a time. `dtest handlers` is about the
kernel running a handler: in a program that is computing, in the thread it
was meant for, on a stack of its own, for a fault, and ending every kind of
wait — each with a handler of its own (`quark_rt::signal::handle`), where
the rest of `dtest` is told of signals as a program written for this
system is. `dtest jobs` runs jobs behind a terminal a child of its own
leads (`dchild behind`). `dtest usage` is about what a program has used
and its share of the processor. The share is measured by starting
programs that compute for ever — as many as there are processors, and as
many again at nice 10 — and asking how much of 600 ms each had: four
times as much at nought, where Linux's weights give nine, so that shares
evened out by the order tasks are taken in fail, and a slow machine does
not.
`dtest
msi` is about a device that interrupts by message, and needs one: QEMU's
`edu` (`-device edu`, which a distribution that ships `dtest` gives the
machines it tests on) and its driver, which the device manager starts where
the image has it in `/usr/lib/drivers`: seven checks more. Without them it
says so and checks nothing. `dtest devices` is about who holds which device:
that the device manager knows the machine, that a program holding no device
reaches none, that only `init` and the device manager hold every device and
a driver its own — and, with `edu` running, that a driver maps its device's
registers and not another's, and may not move its device, aim its message or
claim another driver's: five checks more. `dtest iommu` is about where that device may copy memory, and
needs an IOMMU between it and memory as well (QEMU's `intel-iommu`, on its
q35 chipset): eight checks more — that it copies between its driver's pages
and not to or from a page of `dtest`'s, that what its driver gives back it
no longer reaches, that the kernel counts what it stopped, and whose a
device is. Without an IOMMU it says so and checks nothing. `dtest usb` asks
a USB controller what is plugged into it: eleven checks where there is one
with a keyboard, a mouse and a disk in it, none where there is none.
`dtest display` makes four checks of any display and three more of one a
driver can make another size (a virtio GPU); `dtest sound` one with no
sound card and eighteen with one.
`runtests <list>` runs the
programs a list names — `/etc/libc.tests`, `/etc/pixman.tests` — and `qfuzz`
throws random requests at every registered service. `callbench N SECS`
counts the calls a second N pairs of threads make, each pair kept to a
processor of its own (`sweep` for 1, 2, 4 and 8): with one lock for the
kernel, pairs on four processors make what one pair makes. `kstress
calls|faults|futex|pipes|mix SECS` has every processor hammering the
kernel and checks each operation — every call answered, every page read
back as written, every word through a pipe in order, no wake lost.

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
- **A program starts with what Linux leaves on a stack** — argc, argv, the
  environment and the auxiliary vector, eight below a multiple of sixteen,
  the strings and `AT_RANDOM`'s bytes above them, at the top of its stack:
  as much of it as that takes up to 128 KiB (`spawn::ARGS_PAGES`,
  `ARGS_PAGES` in `process.c`, musl's `ARG_MAX`), and a list that would take
  more is refused (E2BIG) — never cut short. It was one page, and what did
  not fit was left off without a word: cargo gives rustc kilobytes of
  environment, rustc gives the linker that and a long command line, and cc
  started without most of its environment could not find where it was
  installed (`ctests/bigargs.c`; `dtest environment`). `set_args_env`
  builds it at the bottom of the stack's top `ARGS_PAGES` (`BLOCK_AT` in),
  so that `Spawned::start` knows where without being told; `execve` builds
  it just below the strings. A C library's entry reads it, and a dynamic
  loader can read nothing else, since it can call nothing until it has
  relocated itself. Eight below sixteen, because `SYS_TASK_START` takes
  the stack pointer down to sixteen and eight below, as a call leaves one,
  which is what a Rust entry point expects — `Spawned::start` passes
  sixteen up; `SYS_EXEC_SPACE` takes it as given, and `execve` passes it as
  it is. The first boot of this put every C program's argc where it read
  `argv[0]`. The argument page stays one page: Rust reads it, and so does
  a C program built before (`__quark_start_args`) — and what does not fit
  there is still left off it, for them.
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
  on, and a driver holds the registers of its own device and nothing else.
  `dtest physical` walks every CSpace and fails otherwise. A driver's range
  is one it minted for itself, inside its own device's BARs, from the
  capability for that device (`pci::map_bar`; see *Devices*): the firmware
  chooses where a device is and the kernel says where, and nothing else can
  be minted from it. `dtest devices` has a driver ask for another device's
  registers, and `dtest msi` for the kernel's and an interrupt controller's,
  and each is refused. The right to *device memory* (`DeviceMemory`) was the
  way before, and was every device's at once; nothing asks for it now.
- **A frame a device is told the address of is asked for below four
  gigabytes** (`sys_phys_alloc_low`). Ordinary memory comes from the top of
  what the machine has, and on a machine with more than four gigabytes that
  is above what a register of thirty-two bits can say: the network card was
  handed half an address for its ring, and nothing arrived or left. It
  passes on every machine with four gigabytes or fewer, which is every
  machine the acceptance runs on unless it is told otherwise (`MEM=6G` to a
  distribution's `boot-test.sh`). A new driver whose device reads and
  writes memory itself asks for low frames; one that copies through ports
  or its own mapped registers does not care.
- **Where a program puts a thing is chosen at random** (`quark_rt::layout`,
  `__quark_random_pages` in the C layer): a child's stack by whoever builds
  it (`spawn`, `execve`), and a heap, threads' stacks and storage, and the
  C layer's `mmap` arena by the program, the first time each is wanted —
  a random number of pages into a window of its own, from
  `SYS_GETRANDOM`. A new region a runtime puts things in is a window and a
  random start too, or an address learned from one run of a program is one
  to aim at in every run. A forked child keeps its parent's: it is the
  same program. `dtest layout` and `layouttest` run a program twice and
  look.
- **A driver whose device copies memory claims the device first**
  (`pci::claim`), before it lets the device master the bus — the kernel
  refuses to turn that on before — and gives it only memory from
  `sys_phys_alloc`. On a machine with an IOMMU that is all the device can
  reach, at the same addresses — a ring in anonymous memory, or a page of
  somebody else's, is a write that never arrives, and a line on the serial
  console saying so — and a device nobody has claimed reaches nothing at
  all. `rtl8139`, `virtnet`, `e1000`, `virtblk`, `ahci`, `nvme` and `edu`
  claim theirs. A driver can claim only the device it holds, and there is one
  driver for each.
- **A program declares what it needs; a spawner grants from that.** Capabilities
  come from a `quark_rt::manifest!` block compiled into the image, found by
  scanning for its magic, not from a table of names in `init` — and so does
  which devices a driver drives (`CapReq::drives`), which the device manager
  reads to start it. A spawner mints
  each request from a capability it already holds, so it can never hand out more
  than it has — the shell holds no `PhysRange` and therefore cannot give one
  away. The framebuffer is the one exception: its address comes from the
  bootloader at runtime, so `init` grants it directly.
- **A request a spawner cannot mint is skipped, and nothing says so.** A
  program that needs a capability needs it in every spawner above it:
  `shutdown -r` wrote the reset control register (port 0xCF9), and asked
  for it, and the machine turned off instead — the port stopped at `login`,
  which handed the shell its ports one at a time, by hand. A session's
  capabilities come from `auth` now, so the chain is `auth`'s manifest, the
  shell's (`qsh`) and `shutdown`'s — for the right to turn the machine off
  (`CapReq::power()`), which is what `shutdown` asks the kernel with now,
  as much as for the three ports it still falls back on where the
  firmware's tables say nothing of power. A program that "does nothing" is the
  first thing to suspect of this — and a program that needs a capability
  looks for it and says so (`shutdown`: "this account may not turn the
  machine off") rather than trying and doing nothing.
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
- **A server keeps nothing for a program it could not watch.** A watch that
  fails says the program has gone already, and it can have: a program is
  ended from outside while its request waits its turn, or — with a second
  processor — while it is being answered. "A program that is calling is
  there" was written in three places in the VFS, each beside a watch whose
  answer was thrown away, and each kept a handle, a lock or a directory for
  a program nobody would ever say had gone: a removed directory that a
  killed program had been reading stayed on the disk until the next start,
  and `e2fsck` found it. The same for a task (`sys_task_watch`): a lock
  somebody waits for is not kept waiting for a task already gone. And
  `exec` is a program going: the kernel says so, what was kept for the old
  one is let go, and nothing of it is the new one's.
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
- **Only a driver is a source of keys.** `input` takes keys from a program
  that offers itself only when the device manager says the program is a
  driver it started (`TAG_IS_DRIVER`, by its space id, which is never used
  twice). A program that could make itself a source could type into the
  console as whoever is logged in. The answer is the device manager's
  because only it knows, and `input` looks it up when it starts, before
  anybody could have taken its name.
- **Only the kernel reports a death.** Any program that can call a server can
  send `TAG_TASK_DIED`; what it cannot do is send as sender 0. A server
  believes a notice through `quark_rt::ipc::death_notice` (or
  `space_death_notice`) and answers anything else with that tag as the
  unknown request it is. The nameserver used to forget a service, and `fb`
  give up the console's display, because a program said so.
- **What a program starts is running before the call that started it
  returns**, if a processor is free — and since the kernel started using
  every processor the machine has, one usually is. Give a child everything
  first — its descriptors, its capabilities, its place with a server — and
  start it last, as `quark_rt::spawn` does; what a thread reads has to be
  there before `thread::spawn`. With one processor the starter went on
  until it waited, and code that handed a child something *after* starting
  it worked by that accident. The same is true the other way round: a
  client is no longer stopped while a server it is not calling works, and
  two threads of a program really are in the same memory at the same
  moment. A shared word is an atomic or is under a `sync` lock; "nothing
  else can be running" was never a rule here, and is now not even true.
- **Waiting means blocking.** Programs run in four bands — drivers, servers,
  ordinary programs, idle — and a task in a better band that spins on
  `sys_yield` is immediately runnable again, so nothing below it ever runs.
  `nameserver::lookup_retry` yielded a hundred times between tries and starved
  the VFS out of ever registering. Use `sleep_ns` (or `sleep_ms`, or
  `sleep_ticks`), or `sys_recv_timeout` if there is also something to hear.
  A program asks for its band in its `manifest!` block, and a spawner can
  never grant a better one than its own.
- **A time is nanoseconds, and a span of time says which it is in.** The
  kernel keeps time to the nanosecond (`sys_clock`, `sys_clock_wall`) and
  ends a wait when it is due. Every call that takes how long takes a
  *span*: a count of ticks, hundredths of a second, as all of them always
  did, or — from `syscall::ns` — nanoseconds. `sys_recv_timeout(from, msg,
  5)` is fifty milliseconds and `sys_recv_timeout(from, msg, ns(5))` is
  five nanoseconds; the type is `u64` either way and nothing will say which
  was meant. New code says nanoseconds. And a sleep is now as long as was
  asked: `sleep_ms(1)` used to mean "until the next tick", up to ten
  milliseconds, and a loop that polled that way now polls ten times as
  often.
- **What memory is written out with is never written out.** The kernel
  takes pages a program has not used lately and writes them out when memory
  is short — through `swapd`, which writes through the file server, which
  writes through the disk driver — and it takes them from programs in the
  ordinary band only. A driver and a server keep all of theirs. So the band
  a program asks for in its manifest is also a statement about its memory:
  `swapd` asks for the server band, and a pager for memory that did not
  would be asked to read its own code back from the file it could not run
  without. The same goes for anything put under it. The file it keeps
  pages in is on the root filesystem, whose server `init` starts as one; a
  filesystem `mount` starts a server for is served by an ordinary program,
  and is not a place for it.
- **Nobody but `swapd` holds `Swap`**, and no account has a right that
  gives it. Whoever holds it is handed pages of every program's memory and
  hands them back: it could read them, and answer with anything. `init`
  has it from the kernel and gives it to a program that asks in its
  manifest (`CapReq::swap()`), which a distribution starts with a `start`
  line; a session does not hold it and cannot hand it on.

`init` spawns `FB`, `CONSOLE`, `DEVMGR`, `INPUT` and `VFS` in passes of their
own. If a program misbehaves for lack of a capability, check that its pass
actually calls `grant_caps_from_manifest` — there is no shared path that does
it for them, and INPUT's pass once granted nothing at all, which the UID
bypass hid. A boot-image program that drives a device is not started by `init`
at all: it is offered to the device manager (see *Devices*).

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
working directory (`FD_CWD`), its umask (`SYS_UMASK`), and nothing else.
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

**Signals are the kernel's to run, for C; a program written for this system
is told.** A C program says as it starts where the kernel is to enter it to
run a handler, and that it wants Unix's answers (`__quark_sig_start`, from
musl's start); `linux-abi/src/signal.c` is what is entered — it keeps the
floating-point registers, makes the `siginfo_t` and `ucontext_t` out of the
kernel's record, calls the handler, puts back what it changed and gives the
record back. Masks, `sigwait`, `sigaltstack` and a signal for one thread are
the kernel's (`docs/c-library.md`). A program built on `quark-rt` is told
instead, and runs its handlers at a call (`SYS_SIG_TAKE`); it can have the
kernel run one too (`quark_rt::signal::handle`). Three rules follow. Every
call the layer makes that the kernel can end for a signal has to look at
the answer with `quark_cut_short` — `QUARK_AGAIN` is a call to make again,
and a wait added without that returns a count of four thousand million. A
default action is the kernel's to carry out, because a program cannot exit
with a signal's status by asking to. And a program that holds a session's
terminal without being what the session runs says what it does about
signal 2: `getty` and `login` ignore it, and `qsh` handles it so that Ctrl-C
at its prompt is a fresh prompt.

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
shows both, and what each program was started as.

**A program is what it was started as**, and the kernel keeps it, a
hundred and twenty-eight bytes of its arguments (`SYS_PROGRAM_NAME`): a
spawner says it for the child it has made and not started
(`spawn::set_args_env` does, for every spawner), and the C layer's
`execve` says it for what the program is about to become. A fork's child
is what its parent was, and a program may say it of itself. Anybody may
read it: `ps`, and `/proc/PID/cmdline` and `comm`.

**A session runs on a terminal when the distribution says so.** `init` reads
`/etc/init.conf` (`docs/services.md`): `service` lines name services — a
program, the services it needs, the name it is up once it has registered —
which `init` starts once what they need is up, in whatever order the lines
are in; `start <path> [arguments]` lines name programs to start
and leave running — a driver or a server besides the ones `init` knows by
name, given what its manifest asks for; `run <path> [arguments]` lines name
programs to run to their end, in order, before anybody is let in — loading
the console's font is the first use — and `session <path>` there names what
it starts once the filesystem is up. With no such file that is `login`, on the console as it
always was: standard input a message to `input`, output the console's pipe —
and there it starts a child for each login, which is the login, as a
terminal's `getty` starts a `login` for each.
A distribution that names `getty` gets a real terminal instead: `getty` asks
the console for its pty (`TAG_TTY_OPEN`), opens the slave, and runs `login`
with it as descriptors 0, 1 and 2, so that everything below has a tty —
`isatty` is true, `tcsetattr` works, and a shell that edits its own command
line can turn the echo off. `getty` keeps the slave open between sessions, so
the terminal never sees its last holder go, and starts `login` again each
time it ends — which, on a terminal, is after one login. `login` reads `/etc/passwd` in
Unix's seven fields or the five this began with, and starts any shell but
`qsh` the way a Unix login does: in the home directory, with `HOME`, `USER`,
`LOGNAME`, `SHELL`, `PATH` and `TERM`, under a name with a dash in front.
Who the shell is, and what it holds, is not `login`'s to say: see *Users*.

**`login` is what makes that a session**, in the sense job control needs —
and each login is one. It begins a session (`sys_setsid`), takes the seat
for it (*Users*), and takes the terminal as the session's own
(`sys_pty_set_session`), which gives the terminal a process group in front
of it: `login`'s, in which whatever it starts begins. A shell that does nothing about groups — `qsh` — leaves it at
that, and Ctrl-C is for the lot of them, as it always was. A shell with job
control puts itself in a group of its own and in front, and each job after
it. Three things follow:

- **A session ends with its login, and the terminal goes with it.** The
  kernel gives a terminal's slave to the session that has claimed it and to
  nobody else who merely holds a descriptor (`../quark/CLAUDE.md`). So a
  program somebody left running, and then logged out, is in a session that
  no longer has the terminal: it cannot read what the next person types, by
  the descriptor it kept or by the terminal's number. `getty` led one
  session for as long as the machine was up, and everybody who ever logged
  in was in it. `dtest jobs` checks that a session's leader is not something
  `init` started, and `dchild linger` is the program left behind.
- **`login` takes the terminal back when the shell ends** (`sys_pty_set_front`,
  quietly), and then ends. The group in front is the one that has just gone;
  until somebody is in front again, nothing typed is for anybody and a read
  from behind is not a read.
- **Ctrl-Z does nothing at `qsh`**, by the kernel's rule and not by anybody
  ignoring it: a group with nobody to continue it is not stopped from a
  terminal. Nothing here needs to say what it does about signal 20.

## Services

`init` is the service manager (`init/src/services.rs`, `docs/services.md`):
the boot image's programs and `/etc/init.conf`'s `service` lines are
services, each with what it needs, the name it is up once it has
registered, and what is done when it ends. `svc` asks it, as `init`
(`quark_rt::services`).

- **Up is registered, by the task a service began as.** init asks the
  nameserver which name a task holds (`TAG_LOOKUP_TID`), which grants
  nothing; a lookup by name, every twentieth of a second, would have filled
  init's capabilities. A service that registers from another thread is
  never seen to be up.
- **What is started again is what nobody else holds a part of.** `auth`,
  `net` and `sound` are looked up by whoever wants them, each time; the
  nameserver, the console, the log, the device manager, `input` and the file server
  are named in other programs' descriptors and capabilities, and a new one
  would be a stranger to all of them. A new service that is to be started
  again is one its clients find by asking — and `quark_rt` asks each time;
  the C library asks again when a call to the stack it remembered fails.
  init keeps a copy of each boot program it starts again: GNU/Quark's root
  has no `/boot` to read one from.
- **init stays in the drivers' band**, because it starts `net` again and a
  spawner gives no better band than its own.
- **Stopping a service is ending a program**, and asks what that asks:
  TaskMgmt over every task, which init reads out of the caller's
  capabilities. A stop is answered once the service has gone — init holds
  the answer, not itself — and a service init could not start again is not
  stopped at all.
- **A service's output is its stream of the log** (`logd`, `quark_rt::logd`):
  descriptors 1 and 2 are IPC descriptors to it, tagged with the stream, set
  by init before the service starts. Not for what is part of the console:
  `input` echoes what is typed through its standard output, and in the log
  the echo vanished once the session had the console. A new boot program
  that prints for the user, not about itself, is wired to the console pipe.
- **The console is the session's once it has started** (`TAG_QUIET`): from
  then on what services print is kept and not shown. A line after the login
  prompt pushes the prompt off its line, and restarts print lines whenever
  they happen.
- **`logd` answers at once and waits on nothing.** Every service's every
  write is a call to it, so a `logd` that blocks stops them all: its main
  thread only receives, and the console and the file are written by threads
  of their own. A stream is a small number — init's is 0, a service's its
  place plus one — and what is kept is a list by number: init's was 0xFFFF
  once, and the list grown to it, seven megabytes in one allocation, held the
  main thread long enough for every service's first line to wait on it.
- **A shutdown stops the services in order before anything else**
  (`TAG_STOP_ALL`): what nothing still running needs first, then what it
  needed, then the log written and the files synced. Everything SIGTERMed at
  once had a service that writes as it stops racing the file server it
  writes through.
- **The session waits for services still starting, five seconds at most.**
  A line printed after the login prompt pushes the prompt off the line it is
  waited for on, and `net` said it was ready a moment after the prompt on a
  machine whose card's driver came up late. And `net` says it is ready
  before it registers, not after.

## Users

`docs/users.md` is the whole of it: read that before touching `auth`,
`login`, `su`, `passwd`, the account tools or `quark_rt::{auth, accounts,
session, crypt}`. What must not regress:

- **One program may say who a task is, and it is a server.** `auth` holds
  `SetUid`; `login`, `su` and `passwd` hold nothing, and neither does
  `getty`. There is no setuid bit and there cannot be: a program is loaded
  by whoever starts it, so nothing can vouch that what runs is the file
  whose mode said so, and a spawner hands on only what it holds. A program
  that "needs to be root for a moment" asks `auth`.
- **It blesses a child that has not started, and narrows nothing.** The
  asker builds the child holding nothing; `auth` checks the password, says
  who the child is (`sys_identify`, which the kernel checks against whose
  child it is *at that moment*) and hands it what the account's sessions
  hold. Everything that waits — files, hashing — comes before that step, and
  the grants come straight after it: a task id is recycled, and a grant made
  after a wait is a grant to whatever has the number by then.
- **A child that is built and not wanted goes back whole**
  (`Spawned::discard`). A wrong password leaves a task and an address space
  with an image in it; sixty-four of them were the last password anybody
  typed. `spawn::load` does the same when loading fails half way.
- **What a session may do is what it was handed**, by the account's line in
  `/etc/rights` — and with no such file, everything for user 0 and nothing
  for anybody else. Never test a user id to decide what a *program* may do;
  test it to decide whose a *resource a server owns* is (the file server, a
  disk driver), or to say early what a server would say late (`mount`).
- **A name nobody has is treated as a name somebody has**, all the way: it
  is asked for a password, hashed against, counted as wrong and answered
  with the same words. A shortcut for "no such user" anywhere on that path
  tells whoever is at the login prompt which names there are.
- **Wrong passwords are refused early, never slept on**, counted by who
  asked and by the name typed, in places that are never given up to make
  room. Each of those is there because the alternative was tried or thought
  through: a server that sleeps is asleep for everybody; counted by account
  alone, any user locks root out; and a table of "the last few" had root's
  count pushed out by guesses at other names.
- **A password is read with `stdio::read_secret`** and its buffer is zeroed
  after its one use. The old console's `input` server takes a flag on a read
  that stops it echoing; a pty has its `ECHO` cleared and put back.
- **The account files are Unix's**, in the forms a C library reads, and
  `/etc/shadow` is 0600 from the moment it exists, under any name: the new
  file is *made* with its mode (`vfs::open_new`) beside the old one, and
  renamed over it. Made 0644 and changed afterwards, it was every hash to
  anybody waiting for the name to appear. `auth` clears what a request lent
  it — a password — whichever way the request ends. `ctests/crypttest.c`
  holds the C library to reading what this writes.
- **A terminal is its session's, and the console its seat's.** A session
  is one login's: see *Starting programs*. On a terminal the kernel gives
  the slave only to the session that has it. The console's keyboard,
  pointer and display — and on the console with no terminal, a typed line —
  are the seat's (`quark_rt::seat`): the console's, always; the session a
  login began, from before it prompts; and once somebody has logged in
  there, every program of theirs, whatever session it is in — a terminal's
  shell under a compositor is in one of its own. init says whose it is
  (`TAG_SEAT`), to a login the session service started and to nothing
  else, and takes the user from the child `auth` blessed, not from the
  login's word; when the login ends the seat is the session service's
  session again, which holds nothing but the service. The console is named
  by its process id, because no session is the system's: every program init
  starts begins one of its own. `input` and `fb`
  ask at every claim and every line, and when it moves take it from whoever
  may no longer have it, as from a claimant that died. A new server that
  hands out any of the three asks `Seat::allows`. On the console with no
  terminal, what a program left running still holds of the console's pipe
  it can still print on: a system with more than one user runs its sessions
  on a terminal (`session /usr/bin/getty`), as an installed one does.
- **`stat` needs no right to read the file.** `OPEN_ASK` is how it is asked
  (`vfs::lstat`, the C layer's `stat`, `access` and `statfs`). Without it a
  user's `ls -l /home` was an error for every home but their own. It was
  invisible while everybody was root.
- **The root file server's rules are the mounted ones' too.** A request
  through a mount carries who is asking — user, group and groups
  (`TAG_IDENTITY`) — and `dtest`'s `mounts` section runs the same checks as
  `users` through one. FAT has no owners: it is anybody's to read and user
  0's to change.

## The screen

`fb` is the framebuffer device: it owns the hardware the way `/dev/fb0`
does, knows the mode, and decides who draws. It has no opinion about windows.

Who may be a client is the seat's to say (*Users*): `fb` gives the display,
and `input` the keyboard and the pointer, to the console and to whoever is
logged in at it. A claim from anybody else is refused, and when somebody
logs out what their programs claimed goes, as a claimant's that died does.

Everything else is a client of it. `qtty` is the text console: it claims
the display at boot and draws fullscreen — that is what the machine boots into,
a plain TTY. To the services started before there are users it is a pipe, and
what they write down it is drawn. To a session it is a terminal: asked
(`TAG_TTY_OPEN`), it makes a pseudo-terminal, keeps the master, draws what
comes out of it and types into it what is typed. It holds the keyboard for
that the way a compositor does — a claim on `input`, under the compositor's
when one is running — and turns keys into the bytes a Linux console sends for
them. It waits for all of it rather than looking: a claim made with the right
to call the claimant on offer is told when keys come (`input`'s
`keys_waiting`), and the pipe and the terminal are watched by a thread of the
console's own (`watch`, a set of edges), which tells it with a notification —
so the console wakes for what happens and for its cursor, where it looked at
all three a hundred times a second and the machine was never idle. It gives its terminal to the first program that asks and to nobody else
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

On a PC both come from one driver. A PS/2 mouse is not a second device: it is
the same i8042 answering on the same data port 0x60, with IRQ 12 instead of 1
and bit 5 of the status port saying which device a byte came from. `keyboard`
holds both lines and routes on that bit — never on which interrupt fired,
because a byte for one device can be waiting when the other's interrupt
arrives. Two drivers sharing port 0x60 would take each other's bytes, and the
symptom of losing that race is a keyboard that types rubbish or stops.

And from every USB keyboard and mouse: `input` asks each *source* in turn —
the i8042's driver, found by name when it starts, and any driver the device
manager says it started that offers itself (`TAG_INPUT_SOURCE`), which a USB
controller's does. A key is said the same way whichever keyboard it was
typed on: its Linux code and the ASCII it types, by one table
(`quark_rt::keys`), which the i8042's scancodes and a USB keyboard's usages
are both turned into. A machine with no i8042 at all has its keys from USB
and nowhere else, and `input` starts all the same.

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
- **A display can come from a driver, and then what is drawn is said.** A
  machine whose only display draws from memory — a virtio GPU — gives the
  bootloader no framebuffer: `fb` starts with none and the console waits,
  keeping what is written, until the display's driver (`virtgpu`) offers
  one. The driver is taken only if the device manager says it started it
  (`TAG_FB_DRIVER`), and the display it offers (`TAG_FB_DISPLAY`) is a mode
  and a `PhysRange` over the screen's memory, which the kernel gives the
  driver (`SYS_DISPLAY_MEMORY`) and keeps for the device: it is lent as the
  bootloader's framebuffer is. Such a display shows what its driver copies
  to it and nothing else, so whoever draws says where — the tiles of a
  grid eight across and seven down that it drew on
  (`quark_rt::display::drew`), as a notification to `fb`, which passes it
  to the driver. Seven down because a notification word's bits 16 to 18 are
  the kernel's task signals, and a word with any of them in it is refused
  whole: with eight rows, every repaint of the whole screen was. A notification
  because it never waits: `fb` calls its claimants when the display changes
  hands, and one that was in a call to `fb` at that moment could answer
  nothing until the call timed out. `present` in the compositor and the
  console's every write to the screen say it; something new that draws on
  the screen says it too, or what it draws is never seen there.
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

## Devices

The kernel finds every PCI device at boot and keeps what it found: ids,
class, interrupt line, BARs sized. A program reaches a device with the
capability for it (`CAP_TYPE_PCI_DEVICE`), and nothing else reaches one —
the ports devices were configured through are the kernel's now. `devmgr`,
the device manager, holds every device (`CapReq::pci_devices()`, which only
`init` can give) and is the one program that starts a driver for one.
`quark_rt::devices` is its protocol and `docs/devices.md` says it whole;
`quark_rt::pci` is what a driver uses.

- **A driver says what it drives, in its manifest**: `CapReq::drives(vendor,
  device)`, `drives_class(class, subclass)`, `drives_interface(…)`. The
  device manager starts it once for each such device that has no driver,
  holding that device and no other, with the device's address as its first
  argument (`pci::this_device()`), the interrupt line the firmware wired the
  device to where it has one, its standard output, and what else its
  manifest asks for — granted as any spawner grants, so a driver may ask for
  nothing the device manager does not hold: a driver's band, frames
  (`phys_alloc`), and nothing wider. No spawner but the device manager gives
  a device: `grant_image` skips a match request, and a driver started some
  other way finds it holds nothing and says so.
- **A driver never scans.** It is told which device is its own, and
  everything about the device goes through that one capability: its
  configuration (`pci::read32`, `write16`), its BARs (`pci::map_bar`,
  `pci::ports` mint inside them and nowhere else), its claim
  (`pci::claim`), and its interrupt (`pci::interrupt`). `net` held every
  port on the machine and every interrupt line once, and `edu` every
  device's registers, because which were theirs was not known until they
  looked.
- **Where a device is and where its message goes are not a driver's to
  change.** The kernel refuses a write to a BAR or to the MSI capability,
  and refuses to let a device master the bus before its program has claimed
  it (`pci::enable` with `COMMAND_MASTER` fails until `pci::claim`).
- **A device's interrupt is the best it has, and one kind of it.** A
  message the kernel aims it at where it has MSI; else entry 0 of its MSI-X
  table, the kernel's message written in by the driver; else its line
  (`pci::interrupt`, for every driver). Never MSI-X on a device with MSI:
  the kernel turns MSI on as it aims it, and a device with both on does as
  it likes. And no message for a device that cannot send one —
  `pci::message` gave one once, the kernel aimed nothing, and a driver
  would have waited on it for good.
- **A wait for a device keeps what else the kernel says.** A driver waits
  for its interrupt by receiving from the kernel, and the kernel says
  everything else there too, a claimant's death among it. The wait keeps
  it (`ipc::keep`) and `block::serve` answers what was kept before it
  receives again. Dropped, a death left a claim with a task that had gone
  — and with its task id, which the next task made is given and a write is
  judged by. `dtest disks` ends a claimant in the middle of its reads,
  eight times over.
- **Drivers come from the boot image or from `/usr/lib/drivers`.** One the
  root is on has to be running before there is a filesystem, so it is a
  boot service: `init` offers each boot-image program whose manifest says it
  drives something to the device manager, lent with the call, instead of
  starting it. The rest are installed in `/usr/lib/drivers` (`DRIVERS` in
  the Makefile), which the device manager reads when `init` says the root is
  up — before anything in `/etc/init.conf` runs — and it answers only once
  every driver it started, the boot image's too, says it is up (it calls
  each, which waits until the driver receives, two seconds at most): a
  driver's line about itself, printed a moment after the login prompt,
  pushed the prompt off the line it was waited for on. A USB controller's
  driver answers once what was plugged in as the machine started has been
  seen to, which is also when its keyboard can be typed on. It takes either only from its parent: a program that
  could hand it a driver would be handed a device.
- **The device manager is the drivers' parent**, watches them, and collects
  one that ends. One that failed — a status that is not 0 — is started
  again for its device, from a copy of its image the device manager keeps:
  a second later, twice as long after each failure within a minute of
  starting, up to a minute, and the fifth such failure in a row leaves the
  device without one. What the driver served is the claimant's to find
  again: `net` watches its card's driver and claims `eth0` anew once a
  driver has registered it (`nettest again`); a disk's claimant — the file
  server of the root — does not, and a root whose disk's driver failed is
  gone until the machine starts again.
- **What is in the machine is a question** (`lspci`, `lspci -v`): the
  device manager answers it, and holds nothing back but the devices.
- **A virtio device is a PCI device like any other** (`quark_rt::virtio`):
  its structures are named by capabilities of the vendor's kind and mapped
  from its BARs, its queues are pages of the driver's own memory, and its
  interrupt is one MSI-X message, which each queue is told to send, or its
  line. `virtblk`, `virtnet` and `virtgpu` are its block device, network
  card and display; QEMU's first two are transitional, and only their
  modern half is driven.
- **A USB controller is one program, and so is everything plugged into
  it** (`usb`, for an xHCI controller). Its first thread has the
  controller: commands, the event ring, giving each device plugged in an
  address, and every transfer — a hub's ports are driven as the
  controller's own are, a keyboard's and a mouse's reports (the boot
  protocol's) become keys and movement for `input`. A disk is a thread of
  the program running `block::serve` as `diskN`, which hands each read and
  write to the first thread and waits; pulled out, it ends at once
  (`block::Device::gone`) and its name with it. It takes the *last* free
  name (`block::register_removable_disk`, `disk3` downwards), so the disks
  the machine started with keep theirs whenever it is plugged in, and one
  put back has the name it had. A second thread answers `input` and anybody asking what is
  plugged in (`usb0`, `quark_rt::usb`, `lsusb`), so that nothing waits on
  the controller to be answered. It is in the boot image, because a USB
  keyboard may be the only keyboard there is.
## The network

`net` is the stack, and holds no device. The protocols are smoltcp's —
Ethernet, ARP, IPv4 and IPv6, ICMP, UDP, TCP, DHCPv4 and a resolver —
used as a dependency and not patched: what is this system's is the card
under them and what programs ask of them. Under it is a card's driver,
which serves
`quark_rt::nic`: `rtl8139`, `virtnet` and `e1000` so far, each registered
as the first of `eth0` to `eth7` nobody has. The stack claims `eth0` when
it has been registered. The cards' drivers are in the boot image, as the
disks' are, though the root is not on them: started from
`/usr/lib/drivers` they came up after the root, and the network said it
was ready over the login prompt. Beside the card is `lo`: 127.0.0.1 and
::1, and the card's own addresses too, so that a connection to this
machine by the address it has on its network is answered here rather than
sent out to be answered by nobody.

- **A card answers its claimant and nobody else.** A program that could send
  a frame, or read what comes, would be the network; `qfuzz` checks the
  card refuses it.
- **Frames are pulled, never pushed.** The stack lends a frame to send and
  a buffer to receive into; when frames come, the driver *notifies* the
  stack (`nic::ARRIVED`), which asks for them until there are none.
  Neither ever waits on the other: the driver would be stopping its card for
  one client, and the stack every client for one card.
- **Nothing waits inside a request.** One that waits for the network — a
  connection to be made or to come, bytes or room for them, a name — is
  held with its reply and answered after the turn of the loop that brings
  what it waits for; a task that asks something else, or dies, is waiting
  for nothing. The stack before this one waited inside a request for the
  card's notice, and had to see to everything else the kernel said
  meanwhile: a client's death taken there and dropped left its
  connections for ever.
- **IPv6 is what a router says.** smoltcp answers neighbour solicitations
  and does not listen to routers, so the stack does (`net/src/ndp.rs`): an
  address on the link from the card's, a router solicitation, and from an
  advertisement — read from a raw socket that sees every ICMPv6 packet
  beside smoltcp — a default route, an address on each prefix made from
  the card's EUI-64, and the DNS servers it names, each for as long as it
  said. Advertisements are multicast, and so is finding a neighbour, so a
  card's driver lets every multicast frame in: the e1000's MPE, without
  which it let in none, and the RTL8139's hash registers all ones, which
  QEMU's starts with and a card need not.
- **The machine has a resolver** (`net/src/resolver.rs`): DNS on
  127.0.0.1:53 and [::1]:53, which musl asks when `/etc/resolv.conf` names
  nobody — and nothing writes one — and which the old protocol's lookups
  go through. It forwards, to DHCP's servers and the routers', and keeps
  an answer for as long as its TTLs say (a negative one by its SOA, and
  without one not at all, RFC 2308), handing it out again with what is
  left of them. Only `lo` hears it: it answers this machine and nobody on
  the network.
- **What comes in is asked about first** (`net/src/filter.rs`): a list of
  rules, the first that matches deciding, on every frame the card brings
  and every packet `lo` carries, before smoltcp sees either — so a rule
  about a port is about connections to it from this machine too. It is
  changed only by a caller that offers `NetAdmin` with the call, which the
  stack takes, looks at and lets go of: the kernel never acts on that
  type, and a session holds it when its account has the `network` right.
  What the stack is doing — interfaces, ways out, sockets, the filter —
  anybody may ask (`netctl`).
- **`lo` says everything it has in one turn.** What `lo` sends it receives
  in the same poll, again and again until nothing is in flight: smoltcp's
  own loopback keeps its queue to itself, and a reset or an echo's answer
  waited there for a timer nobody had set.
- **A socket is a descriptor the stack serves** (`docs/net.md`): read and
  written through the kernel with the task's buffer lent, polled by what
  the stack says it is ready for, shared by `dup` and `fork`, and closed
  when its last descriptor goes. A request about one names its cookie and
  is believed only of a program that holds it (`SYS_FD_HOLDS`).
  `quark_rt::socket`, Rust's std and the C library speak this; the tags
  before it — a stream a task owned by a handle, a descriptor read forty
  bytes a call — stay for programs built before.
- **A port listened on is several sockets.** smoltcp's socket listens for
  one connection and then is it, and keeps no queue of connections half
  made, so a second SYN while the only socket listening answers the first
  is refused. The stack takes packets in one at a time and, after every
  one, has a socket of each side listening again for each listener with
  room in its backlog (`Stack::refill`).
- **A C program's socket call never waits in the stack.** What the layer
  asks (`linux-abi/src/inet.c`) the stack answers at once, and where Linux's
  call would wait the layer waits in a poll of the descriptor — which a
  signal ends, and SO_RCVTIMEO bounds. A call held by a server cannot be
  ended by a signal (the kernel's *Known gaps*), so a request that waits is
  for programs written for this system, which say so.
- **A datagram for nobody holds nothing up** (`Stack::send_datagram`). It
  waits at the head of its socket for an answer to "who has it?", and
  smoltcp sends a socket's queue in order: a fuzzer's datagram for an
  address on the network that nothing answers for held up everything behind
  it, to anybody, for good, and an echo request did the same to every ping
  after it. Worse, the stuck socket asked "who has it?" again every second,
  and smoltcp asks about one address a second for the whole card: once the
  gateway's answer expired, a minute later, it was never asked about again,
  and nothing beyond the machine could be reached — the hostile sweep's
  `nettest`, ten minutes after the fuzzer, timed out every time. So every
  datagram the card's sockets queue goes through the
  stack, which keeps account of what is queued where; a socket that sends
  nothing for three seconds has its queue let go of, and the address at its
  head is given up for thirty — what is sent there meanwhile is dropped where
  it is sent. A way out is never given up: what holds one up is more often
  the card's one question a second being spent on somebody else. A new place that queues a datagram on the card goes through it
  too, and one that takes a datagram socket out uses `remove_datagram`. The
  old protocol's ports and pings are bounded the same way: an echo has a
  socket of its own, and no more than sixty-four ports are kept.
- **A stream let go of says goodbye before it goes** (`Stack::let_go`): its
  FIN, or the reset for one closed with something unread, as Linux does,
  and then it is taken out of its set — after a minute whether it has
  finished or not, so a peer that never answers keeps nothing. One still
  connecting goes at once, and a reset not sent in three seconds is given
  up: a reset for an address nothing answers for waited the minute, asking
  "who has it?" every second, and after a fuzzer's connections to
  10.0.2.99 the gateway's answer expired and could not be asked for again —
  nothing beyond the machine answered a ping until they had gone. The card
  asks about one address a second, IPv6's neighbours included.
- **The session waits for the network to say it is ready** — init's
  service manager waits for every service still starting, five seconds at
  most (*Services*) — and `net` says so before it registers and nothing
  after: on the IOMMU machine, whose card's driver comes up late, the ready
  line came after the login prompt and pushed it off its line, every run.

## Sound

`sound` is the mixer, and holds no device. Under it is a sound card's
driver, which serves `quark_rt::pcm` and registers as the first of `pcm0`
to `pcm7` nobody has: `hda` so far, Intel's HD audio, which plays one
stream out of the first codec's first pin that can play. A program opens a
stream of its own — any rate from 8 to 96 kHz, one channel or two, sixteen
bits — and writes to it (`quark_rt::sound`; `play` and `mixer` are the
programs). The mixer keeps a third of a second of each stream, and mixes a
period out of all of them, resampled to the card's 48 kHz, whenever the
card has room for one.

- **Nothing waits on anybody.** A write takes what fits and says how much;
  a program told "full" is notified when there is room, and `write_all`
  waits for that notice or a tenth of a second. The card notifies the mixer
  when it has played a period (`pcm::PLAYED`) and the mixer writes until
  the card says it is full. A mixer that waited on a program would be
  every program's sound stopping for one; a card that waited on the mixer,
  sound that stuttered for nothing.
- **A card answers its claimant and nobody else**, and the mixer claims it
  first: it looks for `pcm0` every second for the minute after it starts,
  and when a stream is opened. `qfuzz` checks the card refuses anybody else.
- **A card plays only while it has something to.** After a turn of its
  ring with nothing written it stops, and the next write starts it again
  from its first period; so a machine with nothing to say takes no
  interrupts for it, and QEMU's recording (`AUDIO=1` in ExplOSion) holds
  what was played and nothing else.
- **A stream is its program's.** It is named by its slot and the opening it
  was, so a closed one's id names nothing; it is refused to every other
  program, and goes when its program does. Four a program.

## Disks

A disk driver serves *volumes* (`quark_rt::block`): volume 0 is the whole
device and volume N its Nth partition, read from the GPT, or an MBR if
there is no GPT. A request names a volume, and its sector numbers count
from that volume's start; the driver refuses what is past its end. So a
client given a partition cannot reach outside it and does not know where it
is. Each disk's driver registers as the first of `disk0` to `disk3` that
nobody has (`block::register_disk`): `disk` for an IDE controller's first
channel, `ahci` for the first disk on an AHCI controller, `nvme` for the
first namespace of an NVMe controller, `virtblk` for a virtio disk.

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
  driver asks the kernel who the caller is. And a tool says which it was:
  `parts` showed a disk a user may not read as one with no partition table,
  and `mount` started a file server to be refused the disk. "Refused" and
  "nothing there" are different answers, and each tool now gives the right
  one (`only root reads a disk`, `only root mounts a filesystem`).
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
- **The VFS's tables grow, and a record never moves** (`vfs/src/blocks.rs`):
  handles, what each was opened on, working directories and mapped files
  are made a block of 256 at a time as they are wanted, to a ceiling each
  table names, and nothing in them is copied somewhere bigger — a request
  holds one handle while it opens another, and a table that reallocated
  would leave it holding what was left behind. A list nobody holds into
  (the orphans) is a `Vec`; and nothing the size of a table goes on the
  stack: a snapshot to walk while the table changes is taken on the heap.
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

`vfs` serves ext2, ext4 and FAT (12, 16 and 32), and ext2 in memory of
its own; `docs/vfs.md` is its protocol. What a
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
- **So is `/proc`** (`vfs/src/procfs.rs`), the same way: the walk hands
  what is left of a path to it at the root's `proc` directory and takes the
  path back at a `..` that comes up out of a program's directory. A file
  there is made when it is read, from what the kernel says then, in
  Linux's form. A program's directory is no inode, so as a working
  directory or the start of a relative path it is a number past every
  inode's (`procfs::base`, `0xC000_0000` and the process id), which the
  walk turns back into `/proc/PID/` — anything new that takes a directory
  handle as a base has to let it through to the walk, not read it as an
  inode. A root with no `proc` directory gets `/proc` matched as written,
  as one with no `dev` gets `/dev`.
- **A full volume says it is full.** An allocator out of blocks or inodes
  says `ERR_NO_SPACE`, and everything between it and the client passes that
  on: a C program is told `ENOSPC`, which is what a program checks for. For
  a long time it was `ERR_IO`, and a full disk read as a failing one.
- **What the server says is on the disk is on the disk, not in the disk's
  cache.** A drive answers a write once it has the data, which may be in
  memory of its own; `block::TAG_FLUSH` asks it to write that out, and
  every driver here answers it (`FLUSH CACHE`, its EXT form, NVMe's flush,
  virtio's `T_FLUSH` where the device offers it, SCSI's `SYNCHRONIZE
  CACHE`). The journal asks (`journal::lasting`) where its order matters:
  before the commit block and after it, after the blocks are written in
  place, between the superblock that says the journal is empty and the one
  that says nothing needs recovering, and after a replay. `TAG_SYNC` asks
  for everything.
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
- **What the server keeps of the filesystem is read again once the journal
  is replayed.** The free counts, the first orphan and every group's
  descriptor live in memory (`Ext2State`), and whatever changes them changes
  that copy and writes it back. Read before the replay, the copy is the
  filesystem from before the transaction a crash left behind: written back
  with the next change — freeing the orphan the crash also left was the
  first — it undid the transaction in the counts, and a directory made in it
  had its inode and its block marked free, for the next file to be given.
  `ext2::read_state` is called at mount and after a replay, and anything new
  the server keeps of the disk belongs in it. `tools/crash-test.sh` in
  ExplOSion makes such a disk with `debugfs` and boots it.
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
- **Static.** `-Ddefault_library=static -Db_staticpic=false` is on every
  meson build, and a module that would be `dlopen`ed is built in instead —
  gdk-pixbuf's loaders are, which is also why no loader cache is needed.
  Shared libraries exist now (*Shared libraries*, below) and the toolkits
  do not use them: moving them over is a port of its own.
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

## Shared libraries

A C program linked `-dynamic` (the musl wrapper's flag, in
`../quark-toolchain`) names an interpreter, `/usr/lib/ld-musl-x86_64.so.1` —
`libc.so` by another name, a copy or a link as a distribution lays it out,
which is musl and the C layer in one shared object and is its own dynamic
loader, as on Linux. Both loaders here read the name (`spawn::interpreter`,
and `execve`), load that file too, at a random base in a terabyte of its own
(`spawn::INTERP_BASE`, `QUARK_INTERP_BASE`), start the task in it, and tell
it where the program is (`AT_BASE`, `AT_ENTRY`, `AT_PHDR` — the copy on the
argument page, whose `PT_PHDR` says it is there, so the loader computes that
the program was not moved). The loader maps each library through `mmap`,
which places a mapping exactly where it is told now (`MAP_FIXED`, and
`MAP_FIXED_NOREPLACE`) and steps the arena past one it places ahead of where
the arena has got to. `dlopen` works in such a program. `ctests/dltest.c` is
the proof: linked to `libdltwo.so`, opening `libdlone.so` by path and by
name, and finding a function, a datum, a constructor that ran, a
thread-local that is each thread's own, and one errno.

Static is still the default, and everything else here is static. A program
built before any of this reads its arguments from the argument page, which
every loader still maps.

**A program linked to be put anywhere is put somewhere at random.** A PIE
(`ET_DYN`, as a program built for Linux usually is, and `-dynamic -fPIE
-pie` here) is loaded a random number of pages into a terabyte of its own
(`spawn::PIE_BASE`, `QUARK_PIE_BASE`), and its loader is told where: its
entry moved (`AT_ENTRY`), and the copy of its headers' `PT_PHDR` written as
the table's address less the distance moved, so that musl computes the
base as where it was put. `ctests/pietest.c` checks what depends on that.

**A program built for Linux runs here, its own system calls answered.**
One linked for Linux's musl asks for its loader by Linux's name,
`/lib/ld-musl-x86_64.so.1` (`QUARK_LINUX_INTERP`, `spawn::LINUX_INTERP`),
where a distribution keeps the same `libc.so`; both loaders tell such a
program so in its auxiliary vector (`QUARK_AT_LINUX`, 0x5155 — Quark's
own key). Its code may make Linux's calls itself — rustix does, in every
Rust program, and tempfile's rename and `terminal_size` among them — and
those would reach the kernel with Linux's numbers. So the C layer, before
main (`quark_start`, `__quark_linux_start`), tells the kernel that this
program's calls are made from libc.so's own code (`SYS_SYSCALL_TRAP`, over
its executable segment, found from its `__ehdr_start`), and a call from
anywhere else comes back as SIGSYS with every register as it was, which
`__quark_sig_run` answers through `__quark_syscall` as it answers musl's
own — the answer in RAX, the rest left alone. SIGSYS stays the layer's in
such a program: a `sigaction` for it to the default or to ignore changes
what a SIGSYS that is not a call does, not that calls are answered. A
static program built for Linux carries its own C library and cannot be
run; one built here asks for `/usr/lib/...` and is never trapped.
`ctests/linuxtest.c`, linked with Linux's loader name, checks the raw calls,
the registers, six arguments, a thread, a fork and an exec.

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
- **`/etc/machine-id` is the machine's, made once** by `init`, the first
  time it starts with a root it can write: a random UUID's thirty-two hex
  digits, from the kernel's random numbers, as systemd makes one — D-Bus
  reads it, and so does whatever was written for systemd's Linux. Made when
  an image is built, it would be every installation's from that image; one
  that is there is left alone.

At the edge of a mount two things are not as one kernel holding every
filesystem would have them, and both are in `docs/vfs.md`: `..` at a
mounted filesystem's root leads out only where the server above can see it
coming, and an absolute symbolic link inside a mount is followed from the
mount's root. A file there cannot be mapped, and a named pipe there cannot
be opened.

**A filesystem in memory is a mount like the others** (`mount -t tmpfs
SIZE DIR`): `vfs mem MEGABYTES mount`, whose volume is a region of its own
memory reserved and not backed (`disk::in_memory`), with ext2 made on it as
it starts (`vfs/src/mkfs.rs`) — blocks of four kilobytes, so that a block
is a page. A page has a frame once something is written to it, and gives
it back when the block it holds is freed (`disk::discard`, from
`ext2_alloc::free_block`), so a file removed is memory the machine has
again. It is listed as `tmpfs`, of kind `KIND_TMPFS`, and what is in it goes
with its server, at `umount` and when the machine stops. It is no bigger
than the machine's memory. `init` mounts none: `/tmp` is on the root, where
the crash test and `dtest pieces` leave files for the host's `e2fsck`, and a
system that wants `/tmp` in memory says so where it starts things.

A mount's server is told who is asking, and holds them to it: `dtest` runs
a program as a user who is not root against a directory laid out in a
mounted ext4 — a file of root's, one of a group the user is in besides its
own, a sticky directory, a directory of root's — and against a FAT
filesystem, which is anybody's to read and root's to change.

**FAT is three filesystems**, told apart by how many clusters there are
and nothing else (`parse_bpb`): FAT12 and FAT16 keep the root directory in
a region of its own, outside every cluster — written here as cluster 0 —
and their entries are twelve and sixteen bits, where FAT32 keeps its root
in clusters and its entries in thirty-two. `mformat` makes anything under
half a gigabyte FAT16 unless told otherwise, so an image's own EFI
partition is one. For a long time all three were read as FAT32, and the
other two mounted and then failed every read.

FAT, which had only ever been a root nobody wrote much to, is what an EFI
system partition is, so it has to be right enough to be written by
anything and have `fsck.fat` find nothing (`vfs/src/fat.rs`, and `dtest
mounts`):

- **A name is a long name.** Every name is kept as VFAT keeps it: the long
  one in entries of thirteen UTF-16 units before the short one, each with
  the short one's checksum, and a short one made to match (`BASIS~N`, the
  first free). A name that fits eight and three in one case is the short
  one alone, with the case in the byte Windows keeps it in. A long name
  read back is the long name, and a file is found by either, without
  regard to case, as Windows finds one.
- **A file with nothing in it has no cluster**: one with a cluster and no
  length is an error to a checker.
- **The count of free clusters is brought up to date after every request
  that changes it**, and the search for a free cluster starts where the
  last one ended and stops at the last cluster the volume has, not the last
  the table has room for.
- **A rename moves the entry and keeps the cluster**, a directory's `..`
  is pointed at its new parent, and what is open by its old name is open
  by its new one: a handle knows a file by its directory and its short
  name, and that is two things a rename changes.
- **A file is made longer by being cut longer**, with clusters of noughts
  — which is what `truncate` and `ftruncate` past the end mean.

## Known gaps

- Servers still know their clients by TID (`Message.sender`). The kernel will
  not deliver a call the caller had no capability for, but a server that keeps
  a client's TID past one call — a lease, a registration, a foreground task —
  must watch it with `sys_task_watch` and forget it on death. Otherwise it
  treats whatever takes the TID next as the same client.
- **Memory is written out slowly, to a file that grows.** `swapd` writes a
  page a call through the file server, which journals it: `dtest pressure`
  takes half a minute where the disk is IDE's, driven a word at a time, and
  six to twelve seconds where the device copies for itself (NVMe, AHCI,
  virtio), in a virtual machine. Its file is made empty and takes room as
  pages are written, the lowest numbers first, so it is as long as the most
  that was ever out at once; on a full disk a page that cannot be written
  stays in memory, which is right and is also a machine that is still short.
  Nothing stops it or takes its file away while the machine is on: ended,
  the pages it held are gone, and a program that touches one ends with a bus
  error. A partition of its own and pages written several at a time are each
  faster, and neither is here.
- Focus is a single stack with little policy: Tab cycles, a new window takes it,
  and a click raises the one under the pointer. Keyboard focus and pointer focus
  are tracked separately, as Wayland requires, but there is no follow-mouse and
  no focus stealing prevention.
- **A call to a server is not cut short by a signal**: a C program's read
  of a file, or its wait for a lock, runs its handler when the server has
  answered. A real-time signal queues, as on Linux, but in a queue of 64 a
  program, and one more is refused with `EAGAIN`. `alarm` and `setitimer` are
  the kernel's one alarm for a program, in real time, to the nanosecond, and
  `timer_create` is 32 more: the timers that count time spent running are
  refused, on either.
- **A program's first thread is not numbered as its process.** `getpid` is
  a process id, 32,768 or more, and `gettid` a task id, below 32,768 (there
  were sixty-four tasks, and the line was 64, until the kernel's 4.1). On Linux the
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
  Which is why a user's session has to *begin* holding nothing: there is no
  later point at which it is taken away.
- **The device manager starts 128 drivers at most**, its table's size; a
  program without `TaskMgmt` may have 4,096 tasks, where the kernel's
  sixteen children were the limit until 4.1. A driver that ends by
  itself, or fails five times in a row, leaves its device without one until
  the machine starts again; and what a failed driver served is its
  claimants' to ask for again, which the network stack does and a disk's
  file server does not. A device's driver is one program, so a second card
  of a kind has a second driver that cannot register the first one's name.
- **USB is what a PC's keyboard, mouse and disks need, and no more.**
  Keyboards and mice that speak the boot protocol — not a tablet, whose
  absolute pointer needs its report descriptor read — disks of 512-byte
  blocks and one unit, hubs of USB 2 and 1, and nothing isochronous. A
  USB 3 hub is not driven, and what is behind one is not seen. A keyboard's
  lights are not lit. One program drives a controller and all of it, so a
  fault in one device's handling takes every device on the controller.
- **A virtio GPU has no cursor plane here.** The compositor draws its
  pointer into the picture, which works on every display, and on one a
  driver copies, each move is a region copied. The device could move a
  cursor of its own, if it were given the image and the positions in a way
  that never waits — which the damage notification, a word of tile bits,
  cannot carry.
- **A seat takes back no mapping.** A program that mapped the framebuffer
  while it had the display goes on drawing on it after the seat has moved:
  it is told it has lost it and cannot claim it again, and that is all —
  revoking a capability takes back no mapping made with it. It reads no
  key. And on the console with no terminal, the console's pipe is drawn
  whoever writes it: a program left running there can still print. The
  terminal is the session's (*Starting programs*) and nobody else's.
- **Users**: one id where Unix has three, no list of commands a user may run
  as another, no password ageing, no `groupdel` or `usermod`, sixteen-bit
  ids on disk. `docs/users.md` has the list and the reasons.
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
- **A mounted filesystem's server gives memory like any program**: `mount`
  is an ordinary program and cannot start one in a better band than its
  own. On a machine that writes memory out (`swapd`), a page of a mounted
  server's that is out when the root's server is waiting on it is read back
  through the root's server, which is waiting — for a minute, and then the
  mount is taken for gone. A filesystem in memory is the likeliest to be
  found that way: its files are pages nothing has touched lately.
- **`/proc` is what this system can say**, which is less than Linux: no
  `fd`, `exe`, `environ`, `maps` or `cwd` in a program's directory, no
  `loadavg` for the machine, and no time a program started. `stat` counts
  no time as niced, waiting for a disk, or stolen by a hypervisor, and
  `meminfo` knows nothing of caches.
- `O_CREAT` through a symbolic link whose target does not exist says EEXIST,
  where Linux makes the target, and `linkat` cannot name its source by
  descriptor (`AT_EMPTY_PATH`). FAT has no links.
- A mapped file's pages stay cached until nothing maps the file any more, and
  the VFS pages 1,024 objects at once — what the kernel lets a pager have.
  A private writable mapping copies a page when it is first touched, read or
  write.
- `mprotect` says yes and does nothing: a mapping is made with the protection
  it will keep, so a program that maps read-only and then asks for write gets
  a mapping that still faults on the write — and a shared library's
  relocated data that its loader would make read-only (RELRO) stays
  writable. Shortening a file does not take
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
  once. An `fcntl` lock is the program's and goes when the program becomes
  another, where POSIX keeps it across `exec`; `flock`'s is the open file's
  and stays with the descriptor.
- **A child is the thread's that forked it**: `waitpid` from another thread
  of the program is `ECHILD`.
- A program has as many descriptors as its limit, files included — 1,024 to
  start, raised as far as 65,536; the C layer keeps what it knows of each
  in a record made the first time it is spoken of (`linux-abi/src/fdside.c`).
  The VFS has 16,384 handles for
  everybody, made as they are wanted, and 4,096 for any one program that
  holds them without descriptors (a descriptor's are bounded by the
  program's limit). A pipe, a terminal and a stream all
  say they are a character device to `fstat`: nothing tells the layer what
  kind a kernel descriptor is.
- **No OpenGL.** GTK starts without it and says so, and GSK draws through
  cairo. Nor is there a session bus unless the session asks for one: D-Bus
  is a distribution's to stage, and a session started without a bus —
  `dbus-run-session` starts one — has GTK say that it cannot be reached.
  GTK is static, so `g_module_symbol` still complains about a NULL module
  twice: `dlopen` works only in a program linked `-dynamic`. Each is a real
  absence rather than a stub, and each is a thing a bigger application may
  ask for and not get.
- **A shared library built here is C's alone.** libstdc++ is static and
  nothing builds a C++ shared object (no crtbeginS); the Rust runtime is
  static. A program built for Linux brings its own — rustc's
  `librustc_driver` — and is loaded with it like any other. A program
  that is not on the root filesystem's `/usr/lib` names its libraries by
  path or by `LD_LIBRARY_PATH`, and nothing caches where libraries are.
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
