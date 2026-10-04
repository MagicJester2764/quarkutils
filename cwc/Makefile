# A C program for Quark.
#
# Built with the x86_64-quark cross compiler when there is one, and that is
# what the near-empty flags are: the target knows where a Quark program loads,
# which code model it needs, where its C library is and what to link it with,
# so a program does not have to. Without the toolchain it falls back to the
# host compiler and says all of it by hand — the same binary, spelled out.

CROSS  := $(shell command -v x86_64-quark-gcc 2>/dev/null)
LIBC   := ../libc

ifneq ($(CROSS),)
CC      := x86_64-quark-gcc
CFLAGS  := -std=c11 -O2 -Wall -Wextra
LDFLAGS :=
LIBS    :=
DEPS    :=
else
CC      ?= gcc
CFLAGS  := -ffreestanding -nostdlib -fno-builtin -fno-stack-protector \
           -fno-pic -no-pie -mcmodel=large -mno-red-zone \
           -std=c11 -O2 -Wall -Wextra -I$(LIBC)/include
LDFLAGS := -nostdlib -no-pie -T ../linker.ld -Wl,--build-id=none
LIBS    := $(LIBC)/src/start.o $(LIBC)/libquark.a
DEPS    := $(LIBC)/libquark.a $(LIBC)/src/start.o
endif

# Relinked every time. Under the cross compiler nothing on the command line
# names the C library or the link script — the target knows where they are —
# so make has no file to compare this against, and a binary linked before
# either changed went on being installed: a month-old copy without the frame
# tables the link script had since started keeping. One C file is cheap.
cwc: main.c $(DEPS) FORCE
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(firstword $(LIBS)) main.c $(wordlist 2,99,$(LIBS))

clean:
	rm -f cwc

FORCE:

.PHONY: clean FORCE
