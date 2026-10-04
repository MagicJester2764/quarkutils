# The VFS protocol

The contract between the file server and its clients. `vfs` is the server and
`vfs/src/protocol.rs` its copy of the numbers below; `quark-rt`'s `vfs`
module, the C library (`libc/include/quark/vfs.h`) and the Linux translation
layer (`linux-abi`) are clients, each with a copy of its own. This document is
the one they are checked against.

A client finds the server by looking up `vfs` with the nameserver, which also
gives it the right to call it. Every request is a synchronous call. Where a
request carries more than six words — a path, a record, file data — the client
lends a buffer with the call (`SYS_CALL_LEND`, in the kernel's `docs/abi.md`) and the
server copies into or out of it before replying. The server never maps a
client's memory and cannot reach a lent buffer once it has answered.

## Replies

A reply's tag is `0` (OK) or `u64::MAX` (error). An error reply carries its
code in `data[0]`:

| Code | Name | Meaning |
|---|---|---|
| 1 | `NOT_FOUND` | No such file or directory |
| 2 | `INVALID_HANDLE` | Not a handle this client holds |
| 3 | `IO` | The disk failed, or the filesystem is damaged |
| 4 | `TOO_MANY_OPEN` | The server's handle table is full, or the caller holds a quarter of it |
| 5 | `INVALID_PATH` | Empty, not NUL-free, or names `.` or `..` where a new name is needed |
| 6 | `NOT_DIR` | A directory was required |
| 7 | `IS_DIR` | A directory was not allowed |
| 8 | `PERMISSION` | The caller may not do this |
| 9 | `READ_ONLY` | The filesystem is mounted read-only |
| 10 | `EXISTS` | The name is taken |
| 11 | `NOT_EMPTY` | The directory still has entries |
| 12 | `NOT_SUPPORTED` | This filesystem cannot do that |
| 13 | `NAME_TOO_LONG` | A path over 4095 bytes, or a name over 255 |
| 14 | `NO_SPACE` | Nowhere to put what was written, or no inode for a new file |
| 15 | `LOOP` | A lookup followed more than 40 symbolic links |
| 16 | `WOULD_BLOCK` | A lock is held that keeps this one out |
| 17 | `DEADLOCK` | Waiting for this lock would wait for ever |
| 18 | `TOO_MANY_LINKS` | The file has as many names as it can |
| 19 | `NO_PEER` | A named pipe opened to write, without waiting, that nobody is reading |
| 20 | `BUSY` | A disk somebody else is using, or the one this system runs from; a mounted filesystem something still uses |
| 21 | `CROSS_DEVICE` | Two names in two filesystems, asked for as one file |

Permission is checked against who the caller is: its user and group
(`SYS_GET_TUID`) and the groups it is in besides (`SYS_GROUPS`), which the
server asks the kernel for — the last only when a file's group is not the
caller's own. The rules are Unix's: the owner's three bits for the owner,
the group's for anybody in the file's group, the rest for everybody else,
and each directory on the way to a file has to let the caller through
(execute). User 0 is not checked. A caller that has gone by the time it is
asked about is nobody (65534), not user 0.

FAT has no owners and no modes to check anything against. It is anybody's
to read and user 0's alone to change — `PERMISSION` to anybody else for
every request that writes, makes, removes or renames — which is the least
that keeps a user off the partition a machine starts from.

## Paths

A request that names a path lends it for reading, with its length in
`data[0]`. It is not NUL-terminated and may be up to 4095 bytes; a longer one
is refused with `NAME_TOO_LONG` rather than shortened. A trailing `/` means
the path must name a directory.

A path that does not start with `/` starts from a *base*, which every request
naming a path carries in `data[5]`: 0 is the calling program's working
directory, and `h + 1` is the directory open as handle `h`. `RENAME` and
`LINK` carry their second path's base in `data[4]`. A base that is not an
open directory of the caller's program is `INVALID_HANDLE` or `NOT_DIR`; an
absolute path ignores it. FAT has no inodes to start from: a base that is
an open directory there is turned back into its path, by its `..` entries,
and the lookup starts from the root.

A symbolic link met on the way is followed: its target takes the place of the
part of the path that named it, from the root if the target starts with `/`
and from the directory holding the link if not. The last component is
followed too, unless the request says otherwise (`OPEN_NOFOLLOW`, and every
request that changes a name: `UNLINK`, `RENAME` and `LINK` act on a link
itself). A path that follows more than 40 links is `LOOP`. `..` is the
directory's own `..` entry, so it goes up from where a link led, not from
where the link was.

## Handles, and who holds them

An open file is a *handle*: an index into the server's table. It names an
inode, never a copy of one. A handle is held one of two ways, and the opener
chooses which.

**By a program.** The handle belongs to the program that opened it — every
thread of it may use it — and the server closes it when the program's last
task dies. It knows a program by its address space (`SYS_TASK_SPACE`) and
watches each one it gives a handle to (`SYS_SPACE_WATCH`). The client keeps
its own position and says an offset with every read and write. This is what
`quark-rt`'s `vfs` module and Quark's own C library use.

**By a descriptor** (`OPEN_DESCRIPTOR`). The handle is the cookie of a
descriptor the server puts in the caller's table (`SYS_FD_SERVE`, in the
kernel's `docs/abi.md`), and the kernel counts who holds it. So it is copied
by `fork`, kept by `exec`, duplicated, passed over a stream and put where a
program's standard output was, and the server hears nothing of any of that:
when a request names such a handle, the server asks the kernel whether the
task asking holds a descriptor for it (`SYS_FD_HOLDS`), and that is the whole
check. The position is the server's — one for the handle, shared by every
descriptor made from the first, which is what makes a child that writes
through what it inherited write *after* its parent. It is closed when the
kernel says the last descriptor has gone; `CLOSE` on one is refused. This is
what the Linux layer uses, so it is what a ported program's files are.

Every request below that takes a handle takes either kind.

## Requests

| Tag | Name | Words | Lent | Reply |
|---|---|---|---|---|
| 1 | `OPEN` | `[len, flags, mode]` | path | `[handle, size, is_dir, mode, access, id]` |
| 2 | `READ` | `[handle, -, offset, len]` | `len` bytes to fill (at most 4096) | `[bytes read]` |
| 3 | `CLOSE` | `[handle]` | — | — |
| 5 | `STAT` | `[handle]` | 88 bytes to fill | `[88]` |
| 6 | `WRITE` | `[handle, -, offset, len]` | `len` bytes to copy (at most 4096) | `[bytes written]` |
| 8 | `READDIR_BULK` | `[handle, start, len]` | `len` bytes to fill (at most 4096) | `[bytes, next, end]` |
| 9 | `MKDIR` | `[len, mode]` | path | — |
| 10 | `UNLINK` | `[len]` | path | — |
| 11 | `RMDIR` | `[len]` | path | — |
| 12 | `RENAME` | `[from_len, to_len]` | both paths, end to end | — |
| 13 | `TRUNCATE` | `[handle, size]` | — | — |
| 14 | `STATFS` | `[handle + 1, or 0]` | 64 bytes to fill | `[64]` |
| 15 | `LINK` | `[from_len, to_len, follow]` | both paths, end to end | — |
| 16 | `SYMLINK` | `[target_len, path_len]` | the target, then the path | — |
| 17 | `READLINK` | `[path_len, room]` | the path, then `room` bytes to fill | `[target_len]` |
| 18 | `CHDIR` | `[len]` | path | — |
| 19 | `FCHDIR` | `[handle]` | — | — |
| 20 | `GETCWD` | — | 4096 bytes to fill | `[len]` |
| 21 | `GIVE_CWD` | `[child_tid]` | — | — |
| 22 | `LOCK` | `[handle, kind, start, len, flags]` | — | `[kind, start, len, holder]` for a query |
| 23 | `MAP` | `[handle, flags]` | — | `[slot, size]` |
| 24 | `SEEK` | `[handle, offset, whence]` | — | `[position, how]` |
| 25 | `SETATTR` | `[path_len, which, nofollow]` | path, then five words | — |
| 26 | `MKNOD` | `[path_len, mode]` | path | — |
| 27 | `DEVCTL` | `[handle, operation]` | — | per operation |
| 28 | `ATTACH` | `[path_len, record_len, server]` | path, then the record; a capability offered | — |
| 29 | `DETACH` | `[path_len]` | path | — |
| 30 | `MOUNTS` | `[index]` | room for a record | `[record_len, kind, pid]` |
| 31 | `ADOPT` | — | — | `[root id, kind, read_only]` |
| 32 | `IDENTITY` | `[uid, gid, groups]` | the groups, four bytes each | — |
| 33 | `RETIRE` | — | — | — |
| 34 | `PATH_OF` | `[handle]` | 4096 bytes to fill | `[len]` |
| 35 | `SYNC` | — | — | — |
| 36 | `BIND` | `[path_len, descriptor, mode]` | path | `[id]` |
| 37 | `CONNECT` | `[path_len, descriptor]` | path | — |
| 38 | `INOTIFY_ADD` | `[path_len, mask, instance, 0, 0, base]` | path | `[watch]` |
| 39 | `INOTIFY` | `[op, watch, instance]` | — | per op |

28 to 30 are *Mounts*, below; 31 to 34 are what one file server says to
another, and a client that says them is refused.

Numbers are never reused. 4 was `READDIR`, which returned one entry per call
and cut its name to 32 bytes. 7 was `CREATE`, which carried its path in the
message and cut it to 40 bytes.

### OPEN

`flags` is a set of:

| Bit | Name | Effect |
|---|---|---|
| 1 | `CREATE` | Make the file if the name is free |
| 2 | `EXCLUSIVE` | With `CREATE`: fail with `EXISTS` if it is not |
| 4 | `TRUNCATE` | Empty a regular file the caller may write |
| 8 | `DIRECTORY` | Fail with `NOT_DIR` unless it is a directory |
| 16 | `NOFOLLOW` | A symbolic link at the end is opened itself |
| 32 | `DESCRIPTOR` | The handle is held by a descriptor, which the caller is given |
| 64 | `APPEND` | Every write through the descriptor goes to the end of the file |
| 128 | `READ` | The descriptor may read |
| 256 | `WRITE` | The descriptor may write |
| 512 | `NOWAIT` | The caller will not wait for what it opens: see *Named pipes* |
| 1024 | `PROXIED` | From the server this filesystem is mounted in: `READ` and `WRITE` are checked as for a descriptor, and a handle is given |
| 2048 | `ASK` | Only to be asked about: see below |

With `DESCRIPTOR` the reply's first word is `handle << 32 | descriptor`: the
number the caller now has, the lowest free from 3, and the handle to name in
other requests. `READ` and `WRITE` say what it is for. They are checked
against the file's mode when it is opened — `PERMISSION`, or `IS_DIR` for a
directory asked for writing — and again on every read and write, where a
descriptor that was not opened for it gets `INVALID_HANDLE`. `TOO_MANY_OPEN`
if the caller's descriptor table is full.

A file made by `CREATE` is a regular file, owned by the caller, with the
permission bits in `data[2]` if that word has bit 16 (`0x10000`) set, and
mode 0644 if the word is 0.
The reply's `mode` includes the file-type bits (`0o170000`), `access` is what
this caller may do (4 read, 2 write, 1 execute), and `id` is the inode number
(FAT: the first cluster), stable for as long as the file exists.

A handle that is not a descriptor's is one its program reads through, so
opening one needs the right to read the file — unless it is opened with
`ASK`, alone or with `NOFOLLOW` or `DIRECTORY`. That needs nothing of the
file itself, only the way to it, and the handle answers `STAT`, `STATFS` and
`CLOSE` and is `NOT_SUPPORTED` for anything else. It is what `stat` is: whose
a file is and how big is something its directory says, and a file nobody but
its owner may read is still listed by `ls -l`. The reply's `access` says what
the caller could do with the file, which is all `access` asks. A disk under
`/dev` is asked about by anybody this way and opened by user 0.

A symbolic link opened with `NOFOLLOW` answers `STAT` (mode `0120777`, its
size the target's length) and `CLOSE`, and `NOT_SUPPORTED` to everything else.
`CREATE` through a link whose target does not exist says `EXISTS`; Linux would
make the target.

### When a write is on the disk

A `WRITE` is answered before the filesystem has recorded it for good. On
ext4 the record is a transaction in the journal, and a file is written a
page at a time: a transaction to a page was fourteen blocks written to
store one. So the transaction is left open for the next write to join, and
is committed when sixty-four writes have joined it, when anything else
changes the filesystem, when nothing has been asked for a fiftieth of a
second, or half a second after the first of them, whichever is soonest.
Only a write waits like this. Every other change is committed
before it is answered, and takes the waiting writes with it.

`SYNC` waits for it: when it is answered, everything written before it is
recorded, here and in every filesystem mounted here. A machine that stops
before a commit has the filesystem as it was — whole, and without the last
few pages — which is what the journal is for.

Recorded means on the disk, and not only in the disk's own cache: a drive
answers a write once it has the data, which may be in memory of its own, so
the server asks the drive to write that out (`block::TAG_FLUSH`) wherever
the order of two writes matters — before a transaction's commit block and
after it, once its blocks are in place, and after a replay — and `SYNC`
asks too.

File data written a whole block at a time is not put through the journal at
all: it goes straight to its block. A new block is one nothing names until
the transaction that says so commits, so a crash leaves it nobody's. A
block the file already had is simply overwritten, and a crash does not
bring the old contents back — as on Linux, where only the filesystem's own
structures are journalled unless asked otherwise.

### READ, WRITE and SEEK

`READ` and `WRITE` take an offset, at most a page of data, and reply with how
much was transferred. A read at or past the end replies 0.

On a descriptor's handle the offset may be all ones (`u64::MAX`): wherever
the descriptor is. The transfer happens there — or, for a write through a
descriptor opened with `APPEND`, at the end of the file — and the position
moves past it. An offset that is a number is `pread` and `pwrite`: it happens
there and the position stays.

`SEEK` moves a descriptor's position: `whence` 0 from the start, 1 from where
it is, 2 from the end, with the offset signed. The reply is where it now is,
and what the descriptor was opened to do — bit 0 read, bit 1 write, bit 2
append — which a program that has just been exec'd into holding it has no
other way to learn. A directory's descriptor can be sent to 0 or to a `next`
a listing gave.

The kernel makes two requests of its own, when a task reads or writes a
served descriptor with `SYS_FD_READ` or `SYS_FD_WRITE`: tags `0xFFFF_0009` and
`0xFFFF_000A`, `[handle, length]`, with the task's buffer lent. They are a
`READ` and a `WRITE` at the descriptor's position, and are checked as those
are — the tag proves nothing; holding the handle does. That is how a program
that has never heard of this protocol writes to a file its parent put on its
standard output.

### STAT

The lent buffer is filled with eleven little-endian 64-bit words:

| Word | Field |
|---|---|
| 0 | `id` — as `OPEN` reports it |
| 1 | `size` in bytes |
| 2 | `mode`, with the file-type bits |
| 3 | `links` |
| 4 | `uid` |
| 5 | `gid` |
| 6 | `atime` |
| 7 | `mtime` |
| 8 | `ctime` |
| 9 | `blocks`, in 512-byte units |
| 10 | `block_size` |

Times are seconds since 1970, from the clock the kernel reads at boot
(`SYS_BOOT_TIME`). A machine whose clock cannot be read counts from boot
instead, and so does everything else on it.

### MKDIR

Makes a directory, owned by the caller, with the permission bits in `data[1]`
if that word has bit 16 set and mode 0755 if it is 0. `EXISTS` if the name is
taken.

### UNLINK, RMDIR, RENAME and LINK

`UNLINK` removes a name that is not a directory's; the file goes with its last
name, or, if a handle still names it, when that handle closes. Until then it
is on the filesystem's orphan list, so a machine stopped first frees it at the
next mount. `RMDIR` removes
an empty directory (`NOT_EMPTY` otherwise). Both need write permission on the
parent — and, where the parent is *sticky* (mode bit `01000`, as `/tmp` is),
the caller has to own the file or the directory, or be user 0. Anybody may
make a file in such a directory and nobody take another's out of it.

`RENAME` lends the source path followed directly by the destination, with the
two lengths in `data[0]` and `data[1]`. It replaces a destination of the same
kind — a file for a file, an empty directory for a directory — and refuses to
move a directory inside itself (`INVALID_PATH`). Moving a name is taking it
out of where it was, and replacing one is taking that one out: a sticky
directory holds both to its rule.

`LINK` lends its two paths the same way and gives the file at the first a
second name at the second. A link at the first path gets the name itself,
unless `data[2]` has bit 0 set, which follows it. A directory is refused (`IS_DIR`), and so is a name
that is taken (`EXISTS`) and a file with as many names as the filesystem
allows (`TOO_MANY_LINKS`: 32000 on ext2, 65000 on ext4). The new name's
directory needs write permission, as for `UNLINK`. FAT has one name to a
file and no links: `UNLINK` and `RMDIR` remove a name and give back what it
held — `BUSY` if the file is open, since there would be nowhere for it to go
on being — `RENAME` moves a name, and a directory's `..` with it, and what
is open goes on being open by the new name; `LINK` and `SYMLINK` are
`NOT_SUPPORTED`. A name on FAT is kept as a long name (VFAT) with an 8.3
one beside it, and a file is found by either, without regard to case.

Two paths that lead into two filesystems — one mounted in the other, or two
mounts — are `CROSS_DEVICE`, for `RENAME` and for `LINK`.

### SYMLINK and READLINK

`SYMLINK` lends the target followed by the new path, and makes the path a
symbolic link, mode `0777`, owned by the caller. The target is kept as given:
it is not resolved and need not exist, but it must not be empty (`NOT_FOUND`)
and must fit a block with room for a NUL (`NAME_TOO_LONG` past 1023 bytes on a
1 KiB-block filesystem). One shorter than 60 bytes is kept in the inode; a
longer one takes a block.

`READLINK` lends one buffer for reading and writing: the path, then `room`
bytes. The link's target is written into those bytes, as much as fits, and the
reply is its whole length. A path that is not a link is `INVALID_PATH`.

### TRUNCATE

Sets a writable handle's regular file to `size` bytes, which must fit in 32
bits. Growing it adds a hole, which reads as zeroes and takes no blocks until
it is written. FAT has no holes: a file grown there is given clusters of
zeroes. `OPEN_TRUNCATE` is the same operation to size 0.

### READDIR_BULK

Fills up to `len` bytes of the lent buffer with directory records, starting
with entry `start` (the first is 0). On a descriptor's handle `start` may be
all ones: from wherever the descriptor has got to, which then moves to the
reply's `next`. Each record is:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `id` — the entry's inode number, or FAT first cluster |
| 8 | 8 | `next` — the `start` that continues after this entry |
| 16 | 8 | `size` in bytes |
| 24 | 2 | `reclen` — this record's length, a multiple of 8 |
| 26 | 1 | `type` — `DT_DIR` 4, `DT_REG` 8, `DT_LNK` 10, `DT_CHR` 2, or 0 |
| 27 | 1 | `namelen` |
| 28 | | the name, a NUL, and padding to `reclen` |

The reply says how many bytes were written, where to continue, and whether
that is the end of the directory. A buffer too small for the next record
yields zero bytes and `end` clear. `.` and `..` are listed where the
filesystem stores them. A position is only meaningful for the directory it
came from, and entries made or removed between two calls may be missed or
seen twice, as with any `readdir`.

### Working directories

Where a program is, is a descriptor: number 64 of its table, one past the
ordinary ones, which the kernel keeps for exactly this. `CHDIR` opens the
directory — the caller must be able to search it — and puts a descriptor for
it there, replacing what was there; `FCHDIR` does the same with an open
directory handle's. A relative path with base 0 starts from whatever
directory the caller's descriptor 64 names, and from `/` if it names none.
`GETCWD` fills the lent buffer with that directory's path and replies with
its length; it is `NOT_FOUND` once the directory has been removed.

Being a descriptor is what makes it follow the program. A forked child has a
copy of it and the program it execs keeps it, with no request to this server:
the server learns where a caller is by asking the kernel what its descriptor
64 is (`SYS_FD_COOKIE`). A client holding a directory open as a descriptor
need not ask for `FCHDIR` at all — copying that descriptor onto 64
(`SYS_FD_DUP`) is the same thing. And a spawner puts a child where it is by
copying its own 64 into the child before starting it.

The server holds the directory by inode, as Linux does, so a rename above it
changes what `GETCWD` says and nothing else, and a directory removed while a
program is in it lasts until the last program in it leaves or goes.

FAT has no directory handles to make a descriptor of, so there the server
keeps each program's directory itself, as a path, by address space. That
record does not follow a `fork` or an `exec`. `GIVE_CWD` is for it: it puts a
program being made in the caller's directory. The child must be a task the
caller's program made (`SYS_TASK_CREATE_IN` lets that be before it runs) for
another program; anything else is `PERMISSION`. `quark-rt`'s `give_cwd` does
both — the copy and the request — so a spawner need not know which
filesystem it is on.

### LOCK

A record lock on the file open as `handle`: `kind` 0 unlocks, 1 is shared, 2
exclusive, over `len` bytes from `start` (`len` 0 runs to the end of the file
and past it). `flags` is a set of:

| Bit | Name | Effect |
|---|---|---|
| 1 | `WAIT` | Answer when the lock is granted, rather than `WOULD_BLOCK` now |
| 2 | `OFD` | The lock is the handle's, not the program's |
| 4 | `QUERY` | Grant nothing: reply with the first lock in the way |

A program's locks are Linux's POSIX locks: its own never conflict, a new one
replaces what the program held over its range and joins neighbours of the same
kind, and closing any of the program's handles on the file drops all of them.
A handle's locks (`OFD`, which is also what `flock` is) conflict with every
other owner, another handle of the same program included, and go when the
handle closes — for a descriptor's handle, when the last descriptor does, so
a forked child shares its parent's `flock`. A program's death drops
everything it held or was waiting for. The server never hears one descriptor
of several close, so a client that has taken a program's lock and closes a
descriptor for the file unlocks it itself, which is what POSIX has a close
do.

A query's reply names the lock in the way — `kind` (0 if none), `start`, `len`
(0 for "to the end") and the program holding it, all ones for a handle's
lock. A waiting request is answered when a release lets it in; closing the
handle it was made through answers it `INVALID_HANDLE`. Before a program
waits, the server follows the programs holding what it wants, and the
requests they are waiting on in turn; if that leads back to the program
asking, the answer is `DEADLOCK`. Locks live in the server's memory, 256 at
once (`NO_SPACE` beyond), and are keyed by inode — on FAT by first cluster,
or for an empty file by its directory and name.

### MAP

A capability to map the file open as `handle`, granted into a free slot of
the caller's CSpace: `MemObject` access 1 (read), and 2 (write) as well if
`flags` bit 0 asks to write through a shared mapping, which needs a writable
handle — and, of a descriptor's, one opened to write (`PERMISSION`
otherwise). A descriptor not opened to read cannot be mapped at all. The caller maps it with `SYS_OBJECT_MAP` and
may delete the capability after: the mapping keeps the object. Directories,
links, devices and FAT files are `NOT_SUPPORTED`.

The server is the object's pager (the kernel's `docs/abi.md`, "Memory
objects"): it answers
`TAG_PAGE_IN` with the page read from the file, zeroes past its end, and
releases an object once the kernel says nothing maps it, having first
written back every page written through a shared mapping; `TAG_OBJECT_SYNC`
writes them back on request (`msync`). A file stays in use while it is
mapped, as while a handle names it. The file and its mappings agree both
ways: what `WRITE` writes is copied into any of its pages the kernel has
cached, `READ` reads cached pages from the cache, and `TRUNCATE` resizes the
object (pages already mapped stay; a new fault past the end is SIGBUS).
Thirty files can be mapped at once.

### SETATTR

Changes what a file's inode says of it. The lent buffer is the path —
`data[0]` bytes of it — followed by five little-endian 64-bit words: mode,
uid, gid, atime, mtime. `which` says which of them are meant:

| Bit | Name | Sets |
|---|---|---|
| 1 | `MODE` | the permission bits (the type is kept) |
| 2 | `UID` | the owner |
| 4 | `GID` | the group |
| 8 | `ATIME` | the access time, to the word given |
| 16 | `MTIME` | the modification time, to the word given |
| 32 | `ATIME_NOW` | the access time, to now |
| 64 | `MTIME_NOW` | the modification time, to now |

The path starts from the base in `data[5]` like any other, and a link at its
end is followed unless `data[2]` is not 0. A `data[0]` of 0 means no path:
the file is the one open as the handle `data[5]` names (a handle plus one),
which is `fchmod`, `fchown` and `futimens`.

The rules are Unix's. A mode is its file's owner's to change, or user 0's;
a link has none to change (`NOT_SUPPORTED`). Giving a file to another user is
user 0's alone; an owner may move a file to their own group or to any group
they are in besides, or say what is already so. A time set to a value is the owner's to set; a time set to now is
also anybody's who may write the file. Every change sets the change time.
Owners and groups are sixteen bits (`INVALID_PATH` beyond). `/dev` and what
is in it are the server's and stay as they are (`PERMISSION`); FAT has
none of this (`NOT_SUPPORTED`).

### MKNOD and named pipes

`MKNOD` makes something that is neither a file nor a directory. `data[1]` is
a mode with its type bits, and the only type there is is a named pipe
(`0o010000`): anything else is `PERMISSION`, a device because the ones there
are are the server's own, and a regular file because that is made by opening
it. `EXISTS` if the name is taken; `NOT_SUPPORTED` on FAT.

A named pipe is an inode and nothing else: a name, an owner, a mode and
times, and no blocks. The pipe is the kernel's, made when the name is first
opened and gone, with whatever was in it, when its last end is closed
(`SYS_FD_SERVE_PIPE`, keyed by the inode number). What the server does is
decide who may open it.

`OPEN` of one with `DESCRIPTOR` and `READ` or `WRITE` — not both, which is
`NOT_SUPPORTED` — is checked against the inode's mode like any open, and
answered with an end of that pipe in place of a handle:

| Word | |
|---|---|
| `data[0]` | the descriptor; there is no handle above it |
| `data[1]` | what to wait on: 0 if the other end is held, else the number `SYS_PIPE_PEER` takes |
| `data[3]`, `[4]`, `[5]` | mode, access and id, as for a file |

The server sees no more of it: not what is read or written, and not the
close. Waiting for the other end is the caller's to do, since a server
cannot wait; with `NOWAIT` the caller is saying it will not, and then a
writer with nobody reading is refused (`NO_PEER`) rather than given an end
— a reader waiting for a writer would otherwise have seen one come and go.
A reader is given its end either way. `TRUNCATE` and `APPEND` mean nothing
to a pipe and are ignored, and one may be opened to write on a filesystem
mounted read-only: nothing is written to the disk.

Opened any other way — without `DESCRIPTOR`, or for neither reading nor
writing — a named pipe is an inode to ask about: `STAT` answers, and a read
finds it empty.

### BIND, CONNECT and local sockets

A local socket is the kernel's, and its name is this server's, as a named
pipe's is: an inode whose mode says it is a socket (`0o140000`), with a
name, an owner and a mode, and no blocks. The kernel knows a listening
socket by this server and a key, which is the inode's number.

`BIND` makes the name, at the path, with the permission bits in `data[2]` —
the caller's umask already taken off — and has the kernel know the caller's
socket `data[1]` by it (`SYS_SOCKET_BIND`). A name that is taken is `EXISTS`,
which is Linux's `EADDRINUSE`; so is one an earlier socket still listens at
by an inode number given out again, as `BUSY`. If `data[1]` is not a socket
that is nothing yet the name is taken back and the answer is
`INVALID_HANDLE`. The reply's word is the inode's number.

`CONNECT` connects the caller's socket `data[1]` to whatever listens at the
path (`SYS_SOCKET_CONNECT`), symbolic links followed. The caller must be
able to write to the name, as on Linux. A name that is not a socket's, or
one nothing listens at, is `NO_PEER` — `ECONNREFUSED`; a listener with as
many connections waiting as it has room for is `WOULD_BLOCK`; a socket
that cannot be connected — connected already, or named — is
`INVALID_HANDLE`. The connection is the kernel's from then on, and this
server sees none of it.

`UNLINK` removes a socket's name like any other, and the listener goes on
listening, reachable by nobody new; a new `BIND` at the path makes a new
name. A FAT filesystem has no sockets, and neither does a mounted one
(`NOT_SUPPORTED`): the kernel would know the listener by the mount's
server, and a connector is calling this one.

### INOTIFY_ADD, INOTIFY and what a watch is told

inotify is this server's: every request that changes a file, and every
open, read, write and close of one, is a request to it, so it can say
what happened to whom. An *instance* is a descriptor it makes for the
caller — `INOTIFY` op 0, whose reply is the caller's descriptor — served
with the flag that says this server will say when it is ready
(`SYS_FD_SERVE`, `SYS_FD_READY`). Its cookie has bit 40 set, which no
handle's has: the C library takes such a descriptor for one of the
kernel's, so a read of it goes through the kernel (`TAG_FD_READ`, with
whether the reader may wait in `data[2]`) and a poll asks the kernel,
which answers what this server said last.

`INOTIFY_ADD` watches the inode at the path for the instance named by
`data[2]` — its cookie, which the caller must hold (`SYS_FD_HOLDS`) —
with Linux's mask in `data[1]`, following a last link unless the mask
has `IN_DONT_FOLLOW`. It needs read permission on the inode, as on Linux;
`IN_ONLYDIR` of what is not a directory is `NOT_DIR`; the inode watched
already by the instance is the same watch, its mask replaced or, with
`IN_MASK_ADD`, added to, and `IN_MASK_CREATE` of it is `EXISTS`. The
reply is the watch's number, which the instance never gives out twice.
There are 2048 watches across every instance; with none left it is
`NO_SPACE`, as Linux's `ENOSPC`. A path in a filesystem mounted here, in
`/dev` or `/proc`, or on FAT, is `NOT_SUPPORTED`: what is watched is
this server's own ext2.

`INOTIFY` op 1 ends watch `data[1]` (`NOT_FOUND` if the instance has
none of that number), and the instance is told `IN_IGNORED`; op 2 says
how many bytes of events wait to be read, which is `FIONREAD`.

What a watch is told, as Linux tells it: of a directory, `IN_CREATE`,
`IN_DELETE`, `IN_MOVED_FROM` and `IN_MOVED_TO` (one cookie for the two
halves of a rename) with the entry's name, and the open, read, write,
change of attributes and close of what is in it, by name — the name the
directory has for the inode, looked for when a watch wants it; of
anything, those of itself without a name, `IN_MOVE_SELF`, and when its
last name goes `IN_DELETE_SELF` and then `IN_IGNORED`, together — Linux
waits for its last descriptor to close before the second. `IN_ISDIR` says
the subject is a directory, and `IN_ONESHOT` ends a watch at its first
event. An instance has 4096 bytes for events not read: one the same as
the last still waiting is not queued again, and one with no room is
`IN_Q_OVERFLOW`, once, until that has been read.

A read is of whole events; one with room for none is refused, which the
C library says as `EINVAL`. A read that may wait and finds nothing is
answered when the next request that made an event is done; one that may
not is answered "nothing yet" (`0xFFFF_FFFE`), the would-block of every
other descriptor. A held read is let go, unanswered, when its task asks
anything else or dies. The instance goes with its last descriptor, and
its watches with it.

### Devices

`/dev` is the server's own, whatever the root filesystem holds there: the
lookup answers for its names itself, so a path reaches the devices however it
is spelled and whatever links it passes through. (A root with no `/dev`
directory, and FAT, match the path as written instead.) It holds five character devices, mode `0666`,
with ids from `0xFFFF_FF00` in this order:

| Name | Read | Write |
|---|---|---|
| `null` | nothing: 0 bytes | accepted and dropped |
| `zero` | zeroes | accepted and dropped |
| `full` | zeroes | `NO_SPACE` |
| `random` | random bytes (`SYS_GETRANDOM`) | accepted and dropped |
| `urandom` | the same | accepted and dropped |

`/dev` itself (id `0xFFFF_FF05`, mode `0755`) lists `.`, `..` and the five,
typed `DT_CHR`. Nothing can be made, removed or renamed under it
(`PERMISSION`); `OPEN_CREATE` on a device opens it, and with `OPEN_EXCLUSIVE`
says `EXISTS`. `STAT` gives a device size 0 and the current time. The Linux
layer reports Linux's device numbers for them (1:3, 1:5, 1:7, 1:8, 1:9). The
images carry an empty `/dev` directory so that listing `/` shows it.

### Disks

`/dev` also lists the disks there are, each a block device (mode `060600`,
root's): `disk0` is the whole of the first disk and `disk0p2` its second
partition; `ram0` is a disk made of memory. Each is a volume of a block
driver (`quark_rt::block`), found by asking the nameserver for the driver
when the name is looked up, so the listing is what is there now. Their ids
begin at `0xFFFF_FD00`, thirty-two to a driver, the disks before the RAM
disks.

One can be read and written at any offset and for any length up to a page a
request; a sector the request only partly covers is read first and written
back whole. A descriptor's position on one is not limited to four
gigabytes. `STAT` gives its size in bytes, and `SEEK` to the end finds it.
Only root opens one (`PERMISSION`).

- **Opening one to write claims its volume** from the driver until the last
  such handle closes. A driver gives a volume to one client at a time, so a
  volume a file server has mounted cannot be opened to write (`BUSY`), and
  a disk cannot while any partition of it is. Opening to read claims
  nothing: a driver lets root read what somebody else holds.
- **The disk this server's own root is on is read-only here** — its
  volume, and the whole disk around it (`BUSY` for a write). The server
  holds that claim itself, so the driver would not refuse it.
- **A handle is on the driver it was opened on.** If that driver goes, the
  handle reads and writes nothing (`IO`), whatever has taken its name or its
  task's number since; the handle still closes.
- **`DEVCTL` operation 1** has the driver of a whole disk read its
  partition table again: what a program does after it has written one. The
  handle is one that was opened to write the whole disk — `PERMISSION` for
  one that only reads, `INVALID_PATH` for a partition's. `BUSY` if a
  partition is in use. The reply is how many volumes there now are.

### /proc

`/proc` is the server's too, found the way `/dev` is (and on a root with no
`proc` directory, matched as written). It is in Linux's form, for the
programs written to read it:

| Name | What it says |
|---|---|
| `self` | a link to the caller's process id |
| `cpuinfo` | a paragraph for each processor: what `cpuid` says, its features as Linux names them, less what the kernel has not turned on |
| `meminfo` | `MemTotal`, `MemFree`, `MemAvailable`, `SwapTotal` and `SwapFree`, from the kernel; `Buffers` and `Cached` are nought |
| `mounts` | a line for each mounted filesystem, as `MOUNTS` counts them: source, place, kind, `rw 0 0` |
| `uptime` | seconds since the machine started, and an idle time of nought |
| `version` | `Quark version` and the system calls' version |
| `PID/` | a directory for each program by its process id, a program's own: `cmdline` (what it was started as, each argument ended by a NUL), `comm`, `mounts`, `stat`, `status` and `task` |
| `PID/task/TID/` | a directory for each of the program's threads by its task id, holding its `comm` |

A file is made when it is read, so each read is of it as it is then, and
`STAT` says it is empty. A program is called what its first thread is —
the task it began as — and a thread what it has named itself
(`SYS_TASK_NAME`), or its program's name until it does: `comm`, and the
name `stat` and `status` give. A `comm` is the one thing written there:
by a thread of its own program, which names the thread — up to a newline,
fifteen bytes — through the kernel, which asks that the writer be calling
this server and be of the thread's program. Nothing else is written, made,
removed or renamed there (`PERMISSION`); a program or a thread that has
ended is `NOT_FOUND`. `/proc` is mode
`0555`, a program's directory is its user's and group's, and both can be a
working directory and the start of a relative path.

### STATFS

The lent buffer is filled with eight little-endian 64-bit words: the
filesystem's magic (`0xEF53` for ext2 and ext4, `0x4d44` for FAT), block
size, block count, free blocks, blocks free to anyone (less those reserved
for user 0), inodes, free inodes, and the longest name. FAT reports its
cluster size and zeroes for the counts.

`data[0]` is 0 for the server's own filesystem, or an open file's handle and
one for the filesystem that file is in — which is how a client asks about a
mounted one.

## Mounts

**A mount is a server.** A filesystem mounted in this one is served by a file
server of its own — `vfs DRIVER VOLUME mount`, the same program started on
another volume, or `vfs mem MEGABYTES mount`, started on memory of its own
with an empty ext2 made there — and the server whose directory it is mounted on stands
between it and every client. A path that walks into that directory goes on
in the other server; a file opened there is a handle here that names a
handle there. A client sees none of it: it calls the server it always
called, with the requests above, and a descriptor is the root server's
whatever is behind it.

What a client can see:

- **`STAT`'s `id` says which filesystem.** The low forty bits are the file's
  number in its own filesystem; above them are the mounts it is under, four
  bits to a mount, the innermost in the lowest four. 0 there is the root.
  Directory entries' ids are the same numbers.
- **A mounted filesystem's root takes the directory's place.** What the
  directory held is out of sight until the filesystem is taken away. Its
  name cannot be removed or renamed (`BUSY`).
- **`..` at a mounted filesystem's root leads out where this server can see
  it coming**: written straight after the mount's own name, or first in a
  path that starts from that root. A path that goes down into the mount and
  climbs back out past its root stays at the root.
- **A symbolic link in a mounted filesystem whose target begins with `/`**
  is followed from that filesystem's root, not the system's.
- **`MAP` is `NOT_SUPPORTED`** for a file in a mounted filesystem, and so
  are named pipes there: the memory object and the pipe would be the other
  server's, and what a client is given is this one's.

### ATTACH, DETACH and MOUNTS

`ATTACH` mounts the filesystem a server serves on the directory at the path.
The caller started that server and so may hand it on: the call offers a
capability for it (`SYS_CALL_WITH`, which lends and offers at once), and
`data[2]` is its task. After the path the caller lends a record — where the
filesystem came from and where it is being put, each ended by a NUL — which
is what `MOUNTS` gives back. Only user 0 mounts (`PERMISSION`). The
directory must exist and be one (`NOT_FOUND`, `NOT_DIR`), must not be the
root, `/dev`, `/proc` or mounted on already (`BUSY`), and must not be in a FAT
filesystem, which has no directory that could say what is on it
(`NOT_SUPPORTED`). A server that does not answer `ADOPT` as a filesystem
waiting to be mounted, within a third of a second, is not mounted (`IO`).

`DETACH` takes the filesystem at the path away, and its server ends. `BUSY`
while anything in it is open, a program is in it, or another filesystem is
mounted inside it; `INVALID_PATH` for a directory nothing is mounted on.

`MOUNTS` counts every mounted filesystem: the root first, then each mount
followed by whatever is mounted inside it. The record is written into what
is lent; `kind` is 1 ext2, 2 ext4, 3 FAT, 4 ext2 in memory (`tmpfs`), and `pid` the process that
serves it. Past the last, the error is `NOT_FOUND` and its second word says
how many there are.

### Between servers

A mounted filesystem's server has no name to be looked up by. It is called
by whoever started it and by the server it was handed to, and by nobody
else.

- **`ADOPT`** makes the caller the server above. Only a server started to be
  mounted accepts it (`PERMISSION` from the root's), and only once (`BUSY`).
  When the server above goes, so does this one.
- **`IDENTITY`** says whose requests follow: a user, a group, and the groups
  the user is in besides, lent as that many 32-bit numbers. A server checks
  permissions as that user, and believes it of the server above alone. It
  is sent again only when who is asking changes.
- **`RETIRE`** has it let its volume go and end; `BUSY` while anything is
  open or mounted in it.
- **`PATH_OF`** is the path of an open directory from this filesystem's
  root, which the server above puts after the mount's own path to answer
  `GETCWD`.

The server above passes each request on with a deadline of a minute, and
takes a server that misses it, or that cannot be called, for one that has
gone: everything through that mount is `IO` until it is detached.
