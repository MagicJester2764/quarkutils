# rust-std-patches

What the std fork carries on top of upstream Rust — exactly, and nothing else.

`hello` and `httpget` are ordinary Rust programs with `std`, built for the
`x86_64-unknown-quark` target. Upstream Rust has no such target, so they are
compiled against a fork of rust-lang/rust — `../rust`, the `quark` branch of
[MagicJester2764/rust](https://github.com/MagicJester2764/rust) — whose `std`
has a platform layer for Quark written on top of `quark-rt`.

**The fork is the truth and this directory is a mirror of it.** It is here
because that platform layer is written against `quark-rt` and has to change
with it, and because a checkout of the whole of rust-lang/rust is a lot to ask
of somebody who wants to read a hundred lines of `thread.rs`. It is generated,
never edited:

```bash
tools/std-patches.sh sync     # rewrite this directory from ../rust
tools/std-patches.sh check    # fail if it is not what ../rust carries
```

`make` runs `check` whenever the fork is on disk, so a change to the fork that
is not mirrored here stops the build. This directory was once the seed the fork
was made from, with notes describing the plan; nothing checked it afterwards,
and by the time anyone looked ten of its sixteen files had diverged and the
notes described changes to files the fork never touched.

## What is in it

```
BASE             the upstream commit the fork left from
upstream.patch   what the fork changes in files upstream already has (29 of them)
library/         the files the fork adds (16), as they are in the fork
```

`BASE` is also the toolchain pin. `library/` only compiles with the `rustc`
built from the commit it is based on — a newer compiler rejects its own `core`
— so `rust-toolchain.toml` has to name the nightly built from that commit, and
`check` compares the two.

Applying the patch to `BASE` and adding the files gives a tree identical to the
fork's, object for object:

```bash
git clone https://github.com/rust-lang/rust ../rust && cd ../rust
git checkout -b quark "$(cat ../quarkutils/rust-std-patches/BASE)"
git apply ../quarkutils/rust-std-patches/upstream.patch
cp -r ../quarkutils/rust-std-patches/library/. library/
git submodule update --init --depth 1 library/backtrace
```

## What std is on Quark

Each of these is a file under `library/std/src/sys/` here, selected by an arm
`upstream.patch` adds to the `cfg_select!` beside it:

| | |
|---|---|
| `pal/quark/` | The entry point: `_start` calls `quark_rt::rt::init`, then `main`, then exits with what it returned. Also the working directory, the process id, and the text of an error number |
| `alloc` | The global allocator, which is `quark-rt`'s |
| `args` | Read from the argument page a spawner maps |
| `env` | Nothing, yet: `var` answers `None`, `vars` panics and setting one is unsupported. A spawner does pass an environment, and the C library reads it; std does not look |
| `stdio`, `fd` | Descriptors 0, 1 and 2, and reading and writing any other |
| `io/error`, `io/is_terminal` | Error numbers. Nothing is a terminal as far as std knows: `is_terminal` is always false |
| `thread` | A thread is a task sharing this one's address space, with native thread-local storage whose destructors std runs itself |
| `time` | `Instant` counts the kernel's ticks; `SystemTime` is the date |
| `random` | The kernel's generator |
| `net/connection` | TCP through the net server |

The rest of the patch puts Quark into groups that already exist. The locks —
mutex, condition variable, read-write lock, `Once` and thread parking — are the
futex implementations, on `quark_rt::rt::futex`. An `OsStr` is UTF-8. Unwinding
is the aborting stub, since a Quark program is `panic = "abort"`. And
`std::os::fd` exists, with `RawFd` an `i32`.

## What is not wired in

`fs/quark.rs`, `process/quark.rs` and `pipe/quark.rs` are in the fork, and
nothing selects them: the `mod.rs` beside each has no arm for Quark, so
`std::fs`, `std::process` and `std::io::pipe` are the `unsupported` ones on this
target and those three files are not compiled. That is the gap written down in
`../CLAUDE.md` — a hosted Rust program reads and writes through descriptors it
is given, not through `File::open` — and the files are what is left of the
first attempt at closing it.

For the same reason `std::os::fd` has no conversions to or from `File` or the
socket types: the patch excludes Quark from each of them.

## Moving to a newer upstream

`upstream.patch` will not apply to a commit other than `BASE`, and that is
expected: std's internals move. The allocator's shape, the futex module's
location, where `RawOsError` lives and `BorrowedCursor`'s parameters have all
changed under this port before. Rebase the fork's branch, fix what each
conflict asks for, move the pin in every repository's `rust-toolchain.toml` to
the nightly built from the new base, and run `sync`.
