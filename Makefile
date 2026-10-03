# quarkutils — everything that runs on Quark.
#
# The runtime, init, the nameserver, the drivers, the servers, the C library,
# the shell and the programs. The kernel is a repository of its own and this
# one never looks inside it: what it knows about the kernel is the ABI, which
# it carries a copy of and checks against what the kernel installed.
#
#   make            every program
#   make install DESTDIR=<dir>   stage them for a distro to assemble
#   make check-abi  the numbers here agree with each other, and with the
#                   kernel's if it has been installed where QUARK_ABI says
#   make check-std-patches   rust-std-patches/ is what the std fork carries

TARGET := x86_64-unknown-none

# Hosted target (requires std fork at QUARK_RUST_STD_PATH)
HOSTED_TARGET := x86_64-unknown-quark
QUARK_RUST_STD_PATH ?= $(CURDIR)/../rust/library

# Stage build artifacts for whoever assembles an image out of them.
#
#   $(DESTDIR)/drivers/init.elf   loaded by the bootloader beside the kernel
#   $(DESTDIR)/boot/         essential services, staged into boot.img
#   $(DESTDIR)/usr/bin/      everything else, staged into the root filesystem
#   $(DESTDIR)/usr/lib/drivers/  drivers the device manager reads once the
#                            root is up: a device's driver that is not
#                            needed before there is a filesystem
#   $(DESTDIR)/etc/
#
# The kernel installs into the same directory, from its own repository, and
# the two do not overlap: it brings kernel.bin, its two modules and the ABI.
DESTDIR ?= dist

# What the kernel installed, if it has been: the header `make install` in the
# kernel's tree writes. Looked for in the directory this installs into, because
# that is where a distro puts one before it builds the other — and nowhere
# else, because a path into somebody's checkout is exactly what this is not
# allowed to know. Without it the comparison is skipped, and said to be; set
# REQUIRE_ABI to make that an error, which is what an integrated build wants.
QUARK_ABI   ?= $(wildcard $(DESTDIR)/usr/include/quark/abi.h)
REQUIRE_ABI ?=

# Programs, as source directory and the name they are installed under. The
# directory is also the crate and the binary.
BOOT_SERVICES := nameserver:NAMESRVR keyboard:KEYBOARD qtty:QTTY \
                 input:INPUT disk:DISK vfs:VFS net:NET fb:FB ramdisk:RAMDISK \
                 auth:AUTH devmgr:DEVMGR virtblk:VIRTBLK ahci:AHCI \
                 nvme:NVME usb:USB rtl8139:RTL8139 virtnet:VIRTNET \
                 virtgpu:VIRTGPU
USR_PROGRAMS  := disktest:DISKTEST qsh:QSH echo:ECHO ls:LS cat:CAT \
                 login:LOGIN getty:GETTY ps:PS ipcping:IPCPING ping:PING \
                 shutdown:SHUTDOWN dtest:DTEST dchild:DCHILD qfuzz:QFUZZ \
                 capdemo:CAPDEMO threadtest:THREADTEST socktest:SOCKTEST \
                 fstest:FSTEST wm:WM wmdemo:WMDEMO wmtype:WMTYPE \
                 mousetest:MOUSETEST runtests:RUNTESTS nettest:NETTEST \
                 setfont:SETFONT ramdisk:RAMDISK disks:DISKS parts:PARTS \
                 vfs:VFS mount:MOUNT umount:UMOUNT su:SU passwd:PASSWD \
                 useradd:USERADD userdel:USERDEL groupadd:GROUPADD \
                 gpasswd:GPASSWD id:ID date:DATE swapd:SWAPD \
                 free:FREE lspci:LSPCI lsusb:LSUSB fbmode:FBMODE

# Drivers for devices nothing needs while the system starts: the device
# manager reads them from /usr/lib/drivers and starts each for the devices it
# says it drives. A device's driver that the root is on goes in BOOT_SERVICES,
# and so does a network card's, so that the network is up — and has said so
# on the console — before anybody is asked to log in, and a USB controller's,
# whose keyboard may be the only one there is.
DRIVERS       := edu:EDU

# Programs written in C, built against libc/.
C_PROGRAMS    := cwc:CWC envtest:ENVTEST

# Programs that need the std fork next door. Built when the fork is present and
# skipped when it is not, so this tree stands alone.
HOSTED_PROGRAMS := hello httpget

names = $(foreach p,$(1),$(firstword $(subst :, ,$(p))))

RUST_PROGRAMS := init $(call names,$(BOOT_SERVICES) $(USR_PROGRAMS) $(DRIVERS))
RUST_ELFS     := $(foreach p,$(RUST_PROGRAMS),$(p)/target/$(TARGET)/release/$(p))
C_ELFS        := $(foreach p,$(call names,$(C_PROGRAMS)),$(p)/$(p))

HAVE_STD_FORK := $(wildcard $(QUARK_RUST_STD_PATH)/std/Cargo.toml)
ifeq ($(HAVE_STD_FORK),)
HOSTED_ELFS :=
else
HOSTED_ELFS := $(foreach p,$(HOSTED_PROGRAMS),$(p)/target/$(HOSTED_TARGET)/release/$(p))
endif

LIBC_A      := libc/libquark.a
# The Linux system call surface, which musl programs are linked against by the
# cross toolchain's specs file. Built here even though nothing in this tree
# links it: the specs file names the archive by path, so a stale one is linked
# into every musl program silently, and the symptom is a bug you already fixed
# still happening.
LINUX_ABI_A := linux-abi/liblinux-abi.a

.PHONY: all check-abi check-std-patches install clean rootfs FORCE

# `all` is not the first target in this file, so say which one is.
.DEFAULT_GOAL := all

check-abi:
	@REQUIRE_ABI="$(REQUIRE_ABI)" ./tools/check-abi.sh $(QUARK_ABI)

# rust-std-patches/ is a mirror of what the std fork carries, and a mirror
# nothing checks goes stale: this one had, by ten files of sixteen. So when the
# fork is here to compare against, it is compared — and the compiler with it,
# which has to be the one built from the commit the fork is based on.
check-std-patches:
ifeq ($(HAVE_STD_FORK),)
	@echo "std-patches: no std fork at $(QUARK_RUST_STD_PATH) — the mirror was not checked"
else
	@./tools/std-patches.sh check $(QUARK_RUST_STD_PATH)/..
endif

all: check-abi check-std-patches $(RUST_ELFS) $(HOSTED_ELFS) $(C_ELFS) $(LINUX_ABI_A) rootfs
ifeq ($(HAVE_STD_FORK),)
	@echo "note: no std fork at $(QUARK_RUST_STD_PATH); skipped $(HOSTED_PROGRAMS)"
endif

# One recipe per kind of program rather than one per program. A pattern rule
# cannot say it: the name appears twice in the path and make allows a single %.
define RUST_BUILD_RULE
$(1)/target/$$(TARGET)/release/$(1): FORCE
	cd $(1) && cargo build --release
endef

$(foreach p,$(RUST_PROGRAMS),$(eval $(call RUST_BUILD_RULE,$(p))))

# quark-rt reaches a hosted binary only through the fork's library/Cargo.toml
# patch, and `cargo -Z build-std` does not propagate that dependency into its
# fingerprints: editing quark-rt leaves the program linked against the previous
# copy, and cargo reports "Finished" without rebuilding. That silently produced
# a hello carrying the pre-Phase-0 syscall numbers while the kernel had moved to
# the new ones, which faulted as #UD out of the alloc error handler.
#
# Hash the quark-rt sources and clean the hosted build when they change. std
# genuinely has to be recompiled in that case — it links quark-rt — so the cost
# is inherent, not overhead. The stamp is written only after a successful
# build, so an interrupted one does not mark itself current.
# The whole of the fork's `sys` tree, not just its quark-named files. Listing
# those by hand missed sys/net/connection/mod.rs, which is where a platform is
# routed to its own module: adding Quark there changed nothing, cargo reported
# "Finished", and httpget went on linking std's `unsupported` socket stubs —
# compiling perfectly and failing at run time.
QUARK_RT_SRCS := $(wildcard quark-rt/src/*.rs) quark-rt/Cargo.toml \
                 $(shell find $(QUARK_RUST_STD_PATH)/std/src/sys -name '*.rs' 2>/dev/null | sort)

define HOSTED_BUILD_RULE
$(1)/target/$$(HOSTED_TARGET)/release/$(1): FORCE
	@new=`cat $$(QUARK_RT_SRCS) | md5sum | cut -d' ' -f1`; \
	 old=`cat $(1)/target/.quark-rt-stamp 2>/dev/null || echo none`; \
	 if [ "$$$$new" != "$$$$old" ]; then \
	   echo "  quark-rt changed since the last hosted build - cleaning std for $(1)"; \
	   (cd $(1) && cargo clean); \
	 fi
	cd $(1) && __CARGO_TESTS_ONLY_SRC_ROOT=$$(realpath $$(QUARK_RUST_STD_PATH)) cargo build --release --target ../x86_64-unknown-quark.json -Z build-std=std,panic_abort -Z build-std-features=compiler-builtins-mem -Z json-target-spec
	@cat $$(QUARK_RT_SRCS) | md5sum | cut -d' ' -f1 > $(1)/target/.quark-rt-stamp
endef

$(foreach p,$(HOSTED_PROGRAMS),$(eval $(call HOSTED_BUILD_RULE,$(p))))

# The C library, and the C programs built against it. A libc is a consumer of
# the Quark ABI exactly as the Rust runtime is; neither is privileged over the
# other, and both are built here.
$(LIBC_A): FORCE
	$(MAKE) -C libc

$(LINUX_ABI_A): FORCE
	$(MAKE) -C linux-abi

define C_BUILD_RULE
$(1)/$(1): $$(LIBC_A) FORCE
	$$(MAKE) -C $(1)
endef

$(foreach p,$(call names,$(C_PROGRAMS)),$(eval $(call C_BUILD_RULE,$(p))))

rootfs:
	@mkdir -p rootfs/etc
	@# Unix's seven fields: what a C library reads to turn a file's owner
	@# into a name. Root has no password until somebody gives it one.
	@echo 'root:x:0:0:root:/home/root:/usr/bin/QSH.ELF' > rootfs/etc/passwd
	@echo 'root:x:0:' > rootfs/etc/group
	@echo 'root::0::::::' > rootfs/etc/shadow

install: all
	@mkdir -p $(DESTDIR)/drivers $(DESTDIR)/boot $(DESTDIR)/usr/bin $(DESTDIR)/usr/lib/drivers $(DESTDIR)/etc
	@# Take back what a previous install put there, so that a program renamed
	@# or removed here does not linger in a staging directory for ever. Only
	@# `.ELF` is cleared, which is exactly the set this target owns: coreutils
	@# and the Wayland clients are staged by ExplOSion afterwards, under their
	@# own names, and must survive this.
	@rm -f $(DESTDIR)/boot/*.ELF $(DESTDIR)/usr/bin/*.ELF $(DESTDIR)/usr/lib/drivers/*.ELF
	@# init is the one program the bootloader hands the kernel, so it sits
	@# beside the kernel's own modules rather than in the boot image.
	@cp init/target/$(TARGET)/release/init $(DESTDIR)/drivers/init.elf
	@for p in $(BOOT_SERVICES); do \
		src=$${p%%:*}; dst=$${p##*:}; \
		cp $$src/target/$(TARGET)/release/$$src $(DESTDIR)/boot/$$dst.ELF; \
	done
	@for p in $(USR_PROGRAMS); do \
		src=$${p%%:*}; dst=$${p##*:}; \
		cp $$src/target/$(TARGET)/release/$$src $(DESTDIR)/usr/bin/$$dst.ELF; \
	done
	@for p in $(DRIVERS); do \
		src=$${p%%:*}; dst=$${p##*:}; \
		cp $$src/target/$(TARGET)/release/$$src $(DESTDIR)/usr/lib/drivers/$$dst.ELF; \
	done
	@# C programs are not cargo crates, so their binaries sit beside their
	@# sources rather than under a target directory.
	@for p in $(C_PROGRAMS); do \
		src=$${p%%:*}; dst=$${p##*:}; \
		cp $$src/$$src $(DESTDIR)/usr/bin/$$dst.ELF; \
	done
	@# Gate on the fork, not on the files: a hosted binary left over from an
	@# earlier build cannot be shown to match the current tree, and shipping a
	@# stale one is how hello ended up calling pre-Phase-0 syscall numbers.
ifeq ($(HAVE_STD_FORK),)
	@echo "  (no std fork - $(HOSTED_PROGRAMS) omitted rather than shipped stale)"
else
	@for p in $(HOSTED_PROGRAMS); do \
	   cp $$p/target/$(HOSTED_TARGET)/release/$$p \
	      $(DESTDIR)/usr/bin/`echo $$p | tr a-z A-Z`.ELF; \
	 done
endif
	@cp rootfs/etc/passwd $(DESTDIR)/etc/PASSWD
	@cp rootfs/etc/group $(DESTDIR)/etc/GROUP
	@# Hashes of passwords: root's alone to read.
	@install -m 600 rootfs/etc/shadow $(DESTDIR)/etc/SHADOW
	@# What the console is, to a program that asks: the entry is the console's
	@# and is kept beside it.
	@cp qtty/termcap $(DESTDIR)/etc/termcap
	@echo "installed to $(DESTDIR)"

clean:
	@for p in $(RUST_PROGRAMS) $(HOSTED_PROGRAMS) quark-rt; do (cd $$p && cargo clean); done
	$(MAKE) -C libc clean
	$(MAKE) -C linux-abi clean
	@for p in $(call names,$(C_PROGRAMS)); do $(MAKE) -C $$p clean; done
	rm -rf rootfs

FORCE:
