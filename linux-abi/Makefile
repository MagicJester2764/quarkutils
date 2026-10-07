# The Linux system call surface, answered by Quark.
#
# Built freestanding and against nothing: this sits *under* a C library, so it
# cannot use one. The only headers are Quark's own, which describe the calls it
# translates into.

CC      ?= gcc
AR      ?= ar
CFLAGS  := -ffreestanding -nostdlib -fno-builtin -fno-stack-protector \
           -fno-pic -no-pie -mcmodel=large -mno-red-zone \
           -std=c11 -O2 -Wall -Wextra -I../libc/include
# For a debugging build. `EXTRA_CFLAGS=-DQUARK_ABI_TRACE` makes an
# unimplemented call name itself on stderr instead of reaching the program as a
# bare errno, which is how you find out which one a port is missing.
CFLAGS  += $(EXTRA_CFLAGS)
# The VFS client is shared with Quark's own C library rather than written
# twice: it is the same wire protocol either way, and two copies of a wire
# protocol drift. Compiled here into an object of our own, though — building it
# in place would have two Makefiles writing one path with their own flags, and
# whichever ran last would decide what the other one linked.
OBJS    := src/syscall.o src/start.o src/files.o src/net.o src/inet.o src/clone.o \
           src/clone-entry.o src/vfs-client.o src/process.o src/pty.o \
           src/signal.o src/fdside.o
# Linked for its section rather than its symbols, so it cannot live in the
# archive: nothing references it and it would never be pulled in.
MANIFEST := src/manifest.o

all: liblinux-abi.a $(MANIFEST)

# Every object is built again when any header is changed. The system calls
# are inline functions in `quark/syscall.h`, so a change there is a change
# to the code in every one of these — and for a long time nothing said so:
# an object was built again when its own `.c` was touched and not otherwise.
# The header was changed so that a call passes zero for an argument it does
# not give; the two files edited that day were built with it and the other
# eight were not, and went on leaving whatever was in the register. It
# showed weeks of work later, as soon as the kernel read a second argument
# to a call that used to take one — `raise` stopped working in every C
# program, because the library's own thread id was never recorded.
HEADERS := $(wildcard ../libc/include/quark/*.h) $(wildcard ../libc/include/*.h) $(wildcard src/*.h)
$(OBJS) $(MANIFEST): $(HEADERS)

liblinux-abi.a: $(OBJS)
	rm -f $@
	$(AR) rcs $@ $(OBJS)

%.o: %.c
	$(CC) $(CFLAGS) -c -o $@ $<

%.o: %.s
	$(CC) $(CFLAGS) -c -o $@ $<

src/vfs-client.o: ../libc/src/quark.c
	$(CC) $(CFLAGS) -c -o $@ $<

# The same objects again, position-independent, for the C library as a
# shared object: musl's `libc.so` takes all of this archive in, since every
# call musl makes is made through it and the dynamic loader in it is the
# first thing to make one. Code that does not care where it is does not need
# the large model, and is quicker without it.
PIC_CFLAGS := $(filter-out -fno-pic -no-pie -mcmodel=large,$(CFLAGS)) -fPIC
PIC_OBJS   := $(patsubst src/%.o,pic/%.o,$(OBJS))
$(PIC_OBJS): $(HEADERS)

liblinux-abi-pic.a: $(PIC_OBJS)
	rm -f $@
	$(AR) rcs $@ $(PIC_OBJS)

pic/%.o: src/%.c
	@mkdir -p pic
	$(CC) $(PIC_CFLAGS) -c -o $@ $<

pic/%.o: src/%.s
	@mkdir -p pic
	$(CC) $(PIC_CFLAGS) -c -o $@ $<

pic/vfs-client.o: ../libc/src/quark.c
	@mkdir -p pic
	$(CC) $(PIC_CFLAGS) -c -o $@ $<

pic: liblinux-abi-pic.a

SYSROOT ?= $(HOME)/opt/cross/x86_64-quark/sys-root

install-sysroot: all
	mkdir -p $(SYSROOT)/usr/lib
	cp liblinux-abi.a $(SYSROOT)/usr/lib/
	cp $(MANIFEST) $(SYSROOT)/usr/lib/quark-libc-manifest.o

clean:
	rm -f $(OBJS) $(PIC_OBJS) $(MANIFEST) liblinux-abi.a liblinux-abi-pic.a

.PHONY: all clean install-sysroot pic
