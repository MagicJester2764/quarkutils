/* The VFS protocol, in C. docs/vfs.md is the contract.
 *
 * A file on Quark is a handle held by a server, so `open` is a message, and a
 * path or the bytes of a read travel in a buffer lent with it. The handle may
 * be the program's own, or it may be what a descriptor in the kernel's table
 * names — `quark_vfs_open_fd` — which is what makes a file something a child
 * inherits and a shell redirects. That is the same protocol whichever C
 * library is on top of it — ours, or musl through the Linux translation
 * layer — so it is written down once here rather than once per library. Two
 * copies of a wire format drift.
 *
 * Nothing in here needs a C library: only Quark's own system calls.
 */
#ifndef _QUARK_VFS_H
#define _QUARK_VFS_H

#include <quark/syscall.h>

#define QUARK_VFS_TAG_OPEN    1
#define QUARK_VFS_TAG_READ    2
#define QUARK_VFS_TAG_CLOSE   3
#define QUARK_VFS_TAG_STAT    5
#define QUARK_VFS_TAG_WRITE   6
#define QUARK_VFS_TAG_MKDIR   9
#define QUARK_VFS_TAG_UNLINK  10
#define QUARK_VFS_TAG_RMDIR   11
#define QUARK_VFS_TAG_RENAME  12
#define QUARK_VFS_TAG_TRUNCATE 13
#define QUARK_VFS_TAG_READDIR  8   /* the bulk read; 4 is retired */
#define QUARK_VFS_TAG_STATFS  14
#define QUARK_VFS_TAG_LINK    15
#define QUARK_VFS_TAG_SYMLINK 16
#define QUARK_VFS_TAG_READLINK 17
#define QUARK_VFS_TAG_CHDIR   18
#define QUARK_VFS_TAG_FCHDIR  19
#define QUARK_VFS_TAG_GETCWD  20
#define QUARK_VFS_TAG_GIVE_CWD 21
#define QUARK_VFS_TAG_LOCK    22
#define QUARK_VFS_TAG_MAP     23
#define QUARK_VFS_TAG_SEEK    24
#define QUARK_VFS_TAG_SETATTR 25
#define QUARK_VFS_TAG_MKNOD   26

/* QUARK_VFS_TAG_LOCK's flags: wait to be granted; the lock belongs to the
   handle, not the program; grant nothing and say what is in the way. */
#define QUARK_VFS_LOCK_WAIT  1UL
#define QUARK_VFS_LOCK_OFD   2UL
#define QUARK_VFS_LOCK_QUERY 4UL

/* A directory record, as QUARK_VFS_TAG_READDIR fills a buffer with them:
   `id`, `next` and `size` (8 bytes each), `reclen` (2), `type` (1) and
   `namelen` (1), then the name and a NUL, padded to 8. `type` uses Linux's
   DT_ values. */
#define QUARK_VFS_DIRENT_HEADER 28

/* What `quark_vfs_open` may be asked to do besides open. */
#define QUARK_VFS_OPEN_CREATE    1UL  /* make the file if the name is free */
#define QUARK_VFS_OPEN_EXCLUSIVE 2UL  /* with CREATE: the name must be free */
#define QUARK_VFS_OPEN_TRUNCATE  4UL  /* empty a regular file */
#define QUARK_VFS_OPEN_DIRECTORY 8UL  /* it must be a directory */
#define QUARK_VFS_OPEN_NOFOLLOW 16UL  /* a link at the end is opened itself */
#define QUARK_VFS_OPEN_DESCRIPTOR 32UL /* held by a descriptor, which is given */
#define QUARK_VFS_OPEN_APPEND   64UL  /* every write goes to the end */
#define QUARK_VFS_OPEN_READ    128UL  /* the descriptor may read */
#define QUARK_VFS_OPEN_WRITE   256UL  /* the descriptor may write */
#define QUARK_VFS_OPEN_NOWAIT  512UL  /* a named pipe: do not wait for the other end */

/* Set in a word of permission bits to say they are meant: a word of 0 is a
   caller that says nothing, and gets 0644 for a file and 0755 for a
   directory. */
#define QUARK_VFS_MODE_GIVEN 0x10000UL

/* In place of an offset, on a descriptor's handle: wherever the descriptor
   is, which then moves. In place of a directory read's start, the same. */
#define QUARK_VFS_AT_POSITION (~0UL)

#define QUARK_VFS_SEEK_SET 0UL
#define QUARK_VFS_SEEK_CUR 1UL
#define QUARK_VFS_SEEK_END 2UL

/* Which of SETATTR's five words — mode, uid, gid, atime, mtime — are meant. */
#define QUARK_VFS_ATTR_MODE       1UL
#define QUARK_VFS_ATTR_UID        2UL
#define QUARK_VFS_ATTR_GID        4UL
#define QUARK_VFS_ATTR_ATIME      8UL
#define QUARK_VFS_ATTR_MTIME     16UL
#define QUARK_VFS_ATTR_ATIME_NOW 32UL
#define QUARK_VFS_ATTR_MTIME_NOW 64UL

/* What the server reports. Its own small integers, not anybody's errno —
   each library maps them to whatever it calls those conditions. */
#define QUARK_VFS_NOT_FOUND      1
#define QUARK_VFS_INVALID_HANDLE 2
#define QUARK_VFS_IO             3
#define QUARK_VFS_TOO_MANY_OPEN  4
#define QUARK_VFS_INVALID_PATH   5
#define QUARK_VFS_NOT_DIR        6
#define QUARK_VFS_IS_DIR         7
#define QUARK_VFS_PERMISSION     8
#define QUARK_VFS_READ_ONLY      9
#define QUARK_VFS_EXISTS        10
#define QUARK_VFS_NOT_EMPTY     11
#define QUARK_VFS_NOT_SUPPORTED 12
#define QUARK_VFS_NAME_TOO_LONG 13
#define QUARK_VFS_NO_SPACE      14
#define QUARK_VFS_LOOP          15
#define QUARK_VFS_WOULD_BLOCK   16
#define QUARK_VFS_DEADLOCK      17
#define QUARK_VFS_TOO_MANY_LINKS 18
#define QUARK_VFS_NO_PEER       19 /* a named pipe opened to write, unread, by one who will not wait */

/* The server's devices have ids from here up, in the order null, zero, full,
   random, urandom. */
#define QUARK_VFS_DEVICE_ID 0xFFFFFF00ul

/* A path is lent with the call that names it. One longer than this is
   refused, never shortened. */
#define QUARK_VFS_MAX_PATH 4095

/* Returned when the call itself could not be made — no VFS, or the message
   did not go. Distinct from every code the server reports. */
#define QUARK_VFS_UNREACHABLE 255

/* `access` is what *this* caller may do with the file — the rwx bits
   `access(2)` asks about, 4 read, 2 write, 1 execute — answered by the server
   rather than worked out here. It depends on the file's owner and on who is
   asking, and the server is the only party that knows both; a client deriving
   it from `mode` would be keeping a second copy of the permission policy. */
#define QUARK_VFS_R_OK 4
#define QUARK_VFS_W_OK 2
#define QUARK_VFS_X_OK 1

struct quark_vfs_file {
    unsigned long handle;
    unsigned long size;
    int is_dir;
    unsigned int mode;   /* with the file-type bits */
    unsigned int access; /* QUARK_VFS_{R,W,X}_OK, for the caller */
    unsigned long id;    /* the inode number, stable while the file exists */
};

/* What STAT fills in: eleven words, in this order. Times are seconds since
   boot, the scale time() counts on here. */
struct quark_vfs_stat {
    unsigned long id;
    unsigned long size;
    unsigned long mode;
    unsigned long links;
    unsigned long uid;
    unsigned long gid;
    unsigned long atime;
    unsigned long mtime;
    unsigned long ctime;
    unsigned long blocks;   /* 512-byte units */
    unsigned long blksize;
};

/* Every one of these returns 0, or a positive error code from the list
   above. Counts come back through the out-parameter, so that a short read is
   never confused with a small error number. */
int quark_vfs_open(const char *path, unsigned long flags, struct quark_vfs_file *out);
int quark_vfs_stat(unsigned long handle, struct quark_vfs_stat *out);
int quark_vfs_mkdir(const char *path);
int quark_vfs_unlink(const char *path);
int quark_vfs_rmdir(const char *path);
int quark_vfs_rename(const char *from, const char *to);
/* A second name; `follow` follows a symbolic link at `from`. */
int quark_vfs_link(const char *from, const char *to, int follow);
int quark_vfs_symlink(const char *target, const char *path);
/* What the link at `path` says: up to `len` bytes into `out`, no NUL. The
   answer is the target's whole length, or a negated error. */
long quark_vfs_readlink(const char *path, char *out, unsigned long len);
int quark_vfs_truncate(unsigned long handle, unsigned long size);

/* The same, with a relative path starting from `base`: 0 is the program's
   working directory, and an open directory's handle plus one is that
   directory. The plain names above pass 0. */
int quark_vfs_open_at(unsigned long base, const char *path, unsigned long flags,
                      struct quark_vfs_file *out);
int quark_vfs_mkdir_at(unsigned long base, const char *path);
int quark_vfs_unlink_at(unsigned long base, const char *path);
int quark_vfs_rmdir_at(unsigned long base, const char *path);
int quark_vfs_rename_at(unsigned long from_base, const char *from, unsigned long to_base,
                        const char *to);
int quark_vfs_link_at(unsigned long from_base, const char *from, unsigned long to_base,
                      const char *to, int follow);
int quark_vfs_symlink_at(const char *target, unsigned long base, const char *path);
long quark_vfs_readlink_at(unsigned long base, const char *path, char *out, unsigned long len);

/* The program's working directory. getcwd writes the path, with no NUL, and
   returns its length or a negated error: NOT_FOUND once the directory has
   been removed, NAME_TOO_LONG when `len` cannot hold it. give_cwd starts a
   program this one is making, before it runs, in this one's directory. */
int quark_vfs_chdir(const char *path);
int quark_vfs_fchdir(unsigned long handle);
long quark_vfs_getcwd(char *out, unsigned long len);
int quark_vfs_give_cwd(unsigned long child);

/* A record lock on `handle`'s file: `kind` 0 unlock, 1 shared, 2 exclusive,
   over `start` for `len` bytes (0: to the end and beyond). With
   QUARK_VFS_LOCK_QUERY, `out` gets the kind, start, length and holding
   program of the first lock in the way (kind 0 if none). */
int quark_vfs_lock(unsigned long handle, unsigned long kind, unsigned long start,
                   unsigned long len, unsigned long flags, unsigned long out[4]);

/* A capability to map `handle`'s file, granted into this task's CSpace:
   `*slot` is where, `*size` the file's length. `write_shared` asks for write
   access through a shared mapping, which needs a writable handle. */
int quark_vfs_map(unsigned long handle, int write_shared, unsigned long *slot,
                  unsigned long *size);

/* Fill `buf` (at most a page) with the records of a directory from entry
   `start`. `*next` is where the next call should start, `*end` says there is
   nothing more. A buffer too small for even one record comes back empty with
   `*end` clear. */
int quark_vfs_readdir(unsigned long handle, unsigned long start, void *buf, unsigned long len,
                      unsigned long *used, unsigned long *next, int *end);

/* What the mounted filesystem is and how full: eight words, in this order. */
struct quark_vfs_statfs {
    unsigned long magic;   /* 0xEF53 for ext2/ext4, 0x4d44 for FAT */
    unsigned long bsize;
    unsigned long blocks;
    unsigned long bfree;
    unsigned long bavail;
    unsigned long files;
    unsigned long ffree;
    unsigned long namemax;
};
int quark_vfs_statfs(struct quark_vfs_statfs *out);
/* A read or write carries at most QUARK_VFS_MAX_IO bytes, which the VFS is
   lent for the call: it fills `buf` or copies out of it, and never maps it. */
#define QUARK_VFS_MAX_IO 4096UL
int quark_vfs_read(unsigned long handle, void *buf, unsigned long offset,
                   unsigned long len, unsigned long *got);
int quark_vfs_write(unsigned long handle, const void *buf, unsigned long offset,
                    unsigned long len, unsigned long *put);
int quark_vfs_close(unsigned long handle);

/* Open a file as a descriptor. `*fd` is its number in this program's table,
   and `out->handle` what to name it by in the calls above; it is closed by
   closing the descriptor, never with quark_vfs_close. `flags` should say
   QUARK_VFS_OPEN_READ, _WRITE or both. `mode` is a word of permission bits
   for a file this makes, with QUARK_VFS_MODE_GIVEN, or 0. */
int quark_vfs_open_fd(unsigned long base, const char *path, unsigned long flags,
                      unsigned long mode, struct quark_vfs_file *out, long *fd);
/* Make something that is neither a file nor a directory. `mode` is a mode
   word with its type bits, and a named pipe (S_IFIFO, 0010000) is the only
   type there is.

   A named pipe opened with quark_vfs_open_fd for reading or for writing is
   an end of a pipe, not a file: `out->handle` means nothing, and `out->size`
   is what to wait on — 0 if somebody holds the other end, and otherwise the
   number SYS_PIPE_PEER takes to wait for it to be opened. */
int quark_vfs_mknod(unsigned long base, const char *path, unsigned long mode);
/* mkdir with the permission bits said; `mode` as for quark_vfs_open_fd. */
int quark_vfs_mkdir_mode(unsigned long base, const char *path, unsigned long mode);
/* Move a descriptor's position; `*pos` is where it now is, and `*how`, if
   wanted, what it was opened to do: bit 0 read, bit 1 write, bit 2 append. */
int quark_vfs_seek(unsigned long handle, long offset, unsigned long whence,
                   unsigned long *pos, unsigned long *how);
/* Change a file's mode, owner or times. `attrs` is mode, uid, gid, atime,
   mtime, of which `which` (QUARK_VFS_ATTR_*) says which are meant. With a
   NULL `path`, the file is the one open as the handle `base` names (a handle
   plus one). */
int quark_vfs_setattr(unsigned long base, const char *path, unsigned long which,
                      int nofollow, const unsigned long attrs[5]);
/* Which of this program's descriptors a server's object is, the other way
   round: the handle descriptor `fd` names if the file server serves it.
   Returns 0 and fills `*handle`, or -1 for anything else. */
int quark_vfs_handle(long fd, unsigned long *handle);

#endif
