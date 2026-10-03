# quarkutils

Everything that runs on [Quark](https://github.com/MagicJester2764/quark), an
x86-64 microkernel: the runtime, `init`, the drivers, the servers, the C
library, the shell and the programs. On a microkernel that is most of the
operating system.

It builds against an ABI it does not define. The kernel is another repository
and this one never looks inside it; what it knows is the system call numbers,
which it carries a copy of and checks against what the kernel installed.

## What is here

```
quark-rt/           The Rust runtime: system calls, IPC, the allocator, stdio,
                    threads and locks, the spawner, the VFS and net clients,
                    what a driver does with its PCI device, virtio, and the
                    protocols between a disk or a card and what is above it.
                    The std fork's PAL for x86_64-unknown-quark is built on it.

init/               The first program. Loads the services from the boot image,
                    grants each what its manifest asks for, and starts a
                    session: `login`, or what `/etc/init.conf` names.
nameserver/         Register a name, look one up, be granted the endpoint.
devmgr/             What is in the machine: holds every PCI device and starts
                    the driver for each, holding that device and no other —
                    from the boot image, or `/usr/lib/drivers`. `lspci` asks.

keyboard/           The i8042: keyboard and PS/2 mouse, one driver for both.
disk/               ATA PIO, for an IDE controller: `disk0`, the whole disk and
                    each partition as a volume.
virtblk/            A virtio disk, the same way.
ramdisk/            The same, in memory: an empty disk, or a root the bootloader brought.
disks/              What disks there are, what is on each, and who has it.
parts/              A disk's partition table: print it, make one, add to it.
mount/  umount/     A filesystem put at a directory, by starting a file server for it.
net/                The network stack: Ethernet/ARP/IPv4/ICMP/UDP/TCP, DHCP and
                    a resolver, above whatever card is `eth0`.
rtl8139/  virtnet/  Network cards: the RTL8139, and virtio's.
edu/                QEMU's teaching device: the smallest driver there is for
                    a device whose registers are memory and whose interrupt
                    is a message. Installed in /usr/lib/drivers, where the
                    device manager finds it.
swapd/              Where memory goes when there is not enough of it: the
                    pager the kernel writes programs' unused pages out to,
                    and a file to keep them in. Started by a `start` line in
                    init.conf.

fb/                 The framebuffer device: owns the display, decides who draws.
qtty/               The text console. What the machine boots into, and to
                    a session a terminal; `termcap` beside it says which.
                    It is UTF-8, and draws what `setfont` gives it a font for.
input/              Line discipline for whoever reads, raw events for whoever
                    has claimed the keyboard.
vfs/                ext2, ext4 and FAT32, and the pager for mapped files.
wm/                 A Wayland compositor you run: `wm <program>`.

libc/               A small C library against the raw ABI.
linux-abi/          The Linux system-call surface, answered by Quark. musl is
                    built on this, and every ported program on musl.

auth/               Who somebody is: the one program that may say, which
                    checks passwords and makes a new session's shell its user.
getty/              Put a session on the console's terminal.
login/  qsh/        Log in; the shell.
su/  passwd/        Be somebody else for a while; change a password. Neither
                    holds anything: they ask `auth`.
useradd/ userdel/ groupadd/ gpasswd/ id/
                    Who the users are, in Unix's files, by Unix's names.
ls/ cat/ echo/ ps/ ping/ shutdown/ setfont/ date/ free/ lspci/
                    Programs, in Rust without std.
hello/  httpget/    Programs in Rust with std, built against the fork.
cwc/  envtest/      Programs in C.
dtest/  dchild/     828 checks of the kernel, made through the ABI; twelve
                    more on a machine with a device to ask `edu` about,
                    eight where an IOMMU guards it, and twenty-three where
                    `swapd` is running.
ctests/             The C library's own tests, one per lie a ported program
                    has caught it telling; `tools/build-ctests.sh` builds them.
runtests/  qfuzz/   Run a list of test programs; fuzz every service.
fstest/ nettest/ socktest/ threadtest/ mousetest/ disktest/ ipcping/
capdemo/ wmdemo/ wmtype/                One subsystem each, exercised.

rootfs/             What goes in /etc.
linker.ld           The link script every program here is linked with.
x86_64-unknown-quark.json   The hosted Rust target.
rust-std-patches/   The std fork's port to Quark, mirrored: the commit it left
                    upstream at, a patch for the files upstream has, and the
                    files it adds. Generated; see its README.
docs/               C on Quark, and where it is not Linux; the VFS
                    protocol; the compositor; users; devices.
tools/check-abi.sh  The numbers here agree with each other and with the kernel.
tools/std-patches.sh  The mirror above agrees with the fork.
```

## Building

Dependencies:

- **Rust nightly**, the one `rust-toolchain.toml` pins, with the
  `x86_64-unknown-none` target, `rust-src` and `llvm-tools-preview`
- **a C compiler**, for `libc/` and `linux-abi/`. With the `x86_64-quark`
  cross toolchain on `PATH`
  ([quark-toolchain](https://github.com/MagicJester2764/quark-toolchain)
  builds it) the C programs use it; without, the host compiler does the same
  job with the flags spelled out
- **the std fork** at `../rust`, only for `hello` and `httpget`. Without it
  those two are skipped and the build says so

```bash
make                         # every program
make install DESTDIR=<dir>   # stage them for a distro
make check-abi               # the system call numbers, checked
make clean
```

`make install` lays out `drivers/init.elf`, `boot/` for the services that go
in the boot image, `usr/bin/` for the rest, and `etc/`. The kernel installs
into the same directory from its own repository.

## The ABI

`quark-rt/src/syscall.rs` and `libc/include/quark/syscall.h` are this tree's
copies of the system call numbers. `tools/check-abi.sh` runs first in every
build: the two agree, no number is used twice, and — given the header the
kernel's `make install` writes — the runtime's table is the kernel's, call for
call.

```bash
make -C ../quark install DESTDIR=/tmp/stage
make install DESTDIR=/tmp/stage          # finds /tmp/stage/usr/include/quark/abi.h
```

With no kernel installed the comparison is skipped and said to be;
`REQUIRE_ABI=1` makes that an error.

The two repositories need not be at the same commit, only at compatible
versions. The runtime says which ABI it was built for
(`quark_rt::syscall::ABI_VERSION_MAJOR` and `_MINOR`), and the rule is the one
`docs/abi.md` in the kernel gives: the major must be equal; a kernel whose
minor is ahead has every call the runtime knows, where the runtime knows it;
a kernel that is behind is refused. The build checks that against the
installed header, and `init` checks it again against the kernel it is
actually running on — and stops, saying so on the kernel's console, rather
than start a userland on a kernel that numbers its calls differently.

## Running

Nothing here runs on its own. [ExplOSion](https://github.com/MagicJester2764/explosion)
stages this, the kernel and the [Bang](https://github.com/MagicJester2764/bang)
bootloader, assembles an image and boots it:

```bash
cd ../explosion
make run
```

[GNU/Quark](https://github.com/MagicJester2764/gnu-quark) is the other thing
built on it: the boot services and four programs from here — `getty`,
`login`, `ps`, `shutdown` — under GNU's bash and coreutils. Its programs are
other people's C, unpatched, so it is what the C library and the Linux layer
in this tree are held to.

## Disclaimer

This is primarily an AI-assisted experimental project, not a production system. It was built as a vehicle for exploring OS development concepts with AI tooling. Use at your own risk.
