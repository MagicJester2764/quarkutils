/* Errors in the values Linux uses, because they are what musl compares
 * against. Shared between the dispatcher and the parts it dispatches to.
 */
#ifndef _QUARK_LINUX_ABI_H
#define _QUARK_LINUX_ABI_H

#define LX_EPERM     1
#define LX_ENOENT    2
#define LX_EBADF     9
#define LX_ENOMEM   12
#define LX_ENOEXEC   8
#define LX_EFAULT   14
#define LX_EINVAL   22
#define LX_EMFILE   24
#define LX_ENOTTY   25
#define LX_ESPIPE   29
#define LX_EROFS    30
#define LX_ENOSYS   38
#define LX_EACCES   13
#define LX_EIO       5
#define LX_ENOTDIR  20
#define LX_EISDIR   21
#define LX_ENODEV   19
#define LX_EAGAIN   11
#define LX_ECHILD   10
#define LX_EEXIST   17
#define LX_EXDEV    18
#define LX_ENAMETOOLONG 36
#define LX_ENOTEMPTY 39
#define LX_EOPNOTSUPP 95
#define LX_ERANGE   34
#define LX_ENOSPC   28
#define LX_EMLINK   31
#define LX_ELOOP    40
#define LX_EDEADLK  35
#define LX_ENOLCK   37
#define LX_ETIMEDOUT 110
#define LX_ESRCH     3
#define LX_EINTR     4
#define LX_ENXIO     6
#define LX_EFBIG    27
#define LX_EPIPE    32

/* How many descriptors a program has: as many as the kernel's table holds. */
#define MAX_FDS 64

/* A terminal by its name, for a `stat` and for `ttyname`: in pty.c. */
long __quark_pty_slave_number(long fd);
long __quark_pty_held(const char *path);

/* A process is named by its process id: a number the kernel never gives out
   twice, where a task id is given to the next task made. `fork` answers with
   one, `wait4` and `kill` take one, and `getpid` is one. A thread is still
   named by its task id, which is what a C library locks with. */
long __quark_getpid(void);
long __quark_fork(void);

/* Signals, in signal.c. What rt_sigaction carries on x86-64 is the kernel's
   own layout, not the C library's `struct sigaction`. */
struct lx_ksigaction {
    unsigned long handler;
    unsigned long flags;
    unsigned long restorer;
    unsigned long mask;
};
long __quark_sigaction(long sig, const struct lx_ksigaction *act, struct lx_ksigaction *old,
                       unsigned long size);
long __quark_sigprocmask(long how, const unsigned long *set, unsigned long *old,
                         unsigned long size);
long __quark_sigpending(unsigned long *set, unsigned long size);
long __quark_sigsuspend(const unsigned long *mask, unsigned long size);
long __quark_sigtimedwait(const unsigned long *set, void *info, const long *timeout,
                          unsigned long size);
long __quark_kill(long pid, long sig);
long __quark_tkill(long tid, long sig);
/* Is there a handler to run on the way out of a call, and run them. */
int __quark_sig_due(void);
/* What `__quark_sig_deliver` and `__quark_sig_interrupted` answer: a handler
   ran, and one that ran wants a restartable call to fail instead. */
#define QUARK_SIG_RAN   1
#define QUARK_SIG_EINTR 2
int __quark_sig_deliver(void);
/* A wait the kernel ended for a signal: what ran, as above; 0 to wait again. */
int __quark_sig_interrupted(void);
unsigned long __quark_sig_swap_mask(unsigned long mask);
void __quark_sig_forked(void);
/* A write nobody will read: SIGPIPE, which by default is the end. */
void __quark_sig_pipe(void);

/* "From the working directory", where an *at call takes a descriptor. */
#define LX_AT_FDCWD (-100)
#define LX_AT_SYMLINK_FOLLOW 0x400
#define LX_AT_SYMLINK_NOFOLLOW 0x100
#define LX_AT_EMPTY_PATH 0x1000

/* eventfd's flags. EFD_CLOEXEC is accepted and ignored, like O_CLOEXEC. */
#define LX_EFD_SEMAPHORE 1
#define LX_EFD_NONBLOCK  0x800

/* Open flags this layer acts on. Shared because `pipe2` and `fcntl` have to
   agree about what O_NONBLOCK means. */
#define LX_O_NONBLOCK  04000
#define LX_O_RDWR      2

/* Files, implemented in files.c. Each returns a Linux-style result: a count
   or a value on success, and a negated errno on failure. */
long __quark_open(const char *path, long flags, long mode);
long __quark_openat(long dirfd, const char *path, long flags, long mode);
long __quark_mknodat(long dirfd, const char *path, long mode);
long __quark_close(long fd);
long __quark_read(long fd, void *buf, unsigned long n);
long __quark_write(long fd, const void *buf, unsigned long n);
long __quark_lseek(long fd, long offset, long whence);
long __quark_fstat(long fd, void *statbuf);
long __quark_stat(long dirfd, const char *path, void *statbuf, int follow);
long __quark_access(long dirfd, const char *path, long mode);
long __quark_mkdir(long dirfd, const char *path, long mode);
long __quark_umask(long mask);
long __quark_chmod(long dirfd, const char *path, long mode, int nofollow);
long __quark_chown(long dirfd, const char *path, long uid, long gid, int nofollow);
long __quark_utimens(long dirfd, const char *path, const long *times, int nofollow);
/* Whether a descriptor is a file the VFS serves, as opposed to something the
   kernel keeps; and a way to say a number has changed what it names. */
int __quark_fd_is_file(long fd);
void __quark_fd_forget(long fd);
long __quark_unlink(long dirfd, const char *path);
long __quark_rmdir(long dirfd, const char *path);
long __quark_rename(long fromfd, const char *from, long tofd, const char *to);
long __quark_link(long fromfd, const char *from, long tofd, const char *to, int follow);
long __quark_symlink(const char *target, long dirfd, const char *path);
long __quark_chdir(const char *path);
long __quark_flock(long fd, long op);
long __quark_file_map(long fd, int write_shared, unsigned long *cap);
long __quark_fchdir(long fd);
long __quark_getcwd(char *buf, unsigned long size);
long __quark_truncate(const char *path, long length);
long __quark_file_truncate(long fd, long length);
long __quark_getdents(long fd, void *buf, unsigned long count);
long __quark_dup(long fd, long to);
long __quark_pread(long fd, void *buf, unsigned long n, long offset);
long __quark_pwrite(long fd, const void *buf, unsigned long n, long offset);
long __quark_readlink(long dirfd, const char *path, char *buf, unsigned long size);
long __quark_statfs(const char *path, void *buf);
long __quark_fstatfs(long fd, void *buf);

/* Streams, descriptor passing and waiting, in net.c. */
long __quark_memfd(const char *name, long flags);
long __quark_execve(const char *path, char *const argv[], char *const envp[]);
long __quark_pty_open(const char *path);
int __quark_pty_path(const char *path);
long __quark_ioctl(long fd, unsigned long request, unsigned long arg);
long __quark_ftruncate(long fd, long length);
long __quark_fcntl(long fd, long cmd, long arg);
int __quark_fd_is_nonblock(long fd);
void __quark_fd_set_nonblock(long fd, int on);
long __quark_pipe(int *fds, long flags);
long __quark_socketpair(long domain, long type, long protocol, int *sv);
long __quark_sendmsg(long fd, const void *msg, long flags);
long __quark_recvmsg(long fd, void *msg, long flags);
long __quark_poll(void *fds, long nfds, long timeout_ms);
long __quark_epoll_create(void);
long __quark_epoll_ctl(long epfd, long op, long fd, void *event);
long __quark_epoll_wait(long epfd, void *events, long maxevents, long timeout_ms);

#endif
