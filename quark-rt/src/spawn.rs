//! Loading an ELF and starting it as a new task.
//!
//! There is no fork or exec: a parent creates a task and an address space,
//! builds the program's pages in its own memory, moves them into the child,
//! and starts it. That is a hundred lines of fiddly page arithmetic, and
//! it was written out three times — in `init`, `shell` and `login` — with the
//! shell's and login's copies byte-identical and init's differing only in
//! comments and hardcoded scratch addresses. Drift between them was a matter of
//! time.
//!
//! Staging needs scratch addresses in the *caller's* address space, and those
//! must differ per caller, so they are a parameter rather than a constant here.

use crate::syscall;

pub const PAGE_SIZE: usize = 4096;

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const ET_DYN: u16 = 3;
const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

/// Where the argv page lands in the *child's* address space, by convention
/// with `quark_rt::args`.
pub const ARGS_PAGE_ADDR: usize = 0x80_8000_0000;

/// Program headers carried to the child, at most.
///
/// A static binary built here has four or five; a Linux one with everything
/// the toolchain adds has about a dozen.
pub const MAX_PHDRS: usize = 16;

/// Where on the argument page the program header table goes: a fixed place at
/// the end, so that a long command line can never crowd it out.
///
/// Layout from this offset: the entry size, the entry count, then the table.
/// It is what a C library's `AT_PHDR` points into, and it has to exist because
/// a program's own headers are not in any segment it loads — musl finds its
/// thread-local template through them, and without them every thread-local in
/// a C program landed outside its thread's block. Mirrored as
/// `QUARK_PHDRS_AT` in `quark/layout.h`.
pub const PHDRS_AT: usize = PAGE_SIZE - 16 - MAX_PHDRS * PHDR_SIZE;

const PT_PHDR: u32 = 6;

/// The highest a child's stack may reach. Where in the two gigabytes below
/// it each child's ends is chosen at random (`layout`), and is the child's
/// `Spawned::stack_top`. Stacks grow down.
pub const STACK_TOP: usize = 0x7FFF_FFFF_F000;
const STACK_WINDOW_PAGES: usize = 1 << 19;
/// 1 MiB, matching the kernel's own `USER_STACK_PAGES`. A spawner maps this
/// eagerly, so it is memory spent per task rather than reserved address
/// space — see the note there for why it is this size and not eight
/// megabytes.
pub const STACK_PAGES: usize = 256;
/// The top of a child's stack that holds what a C program finds there when
/// it starts ([`set_args_env`]): 128 KiB, musl's `ARG_MAX`, as the C
/// library's `execve` allows. The count and the pointers begin at its
/// bottom, which is where the child's stack pointer starts whatever the
/// arguments are, and the strings end at its top. It was one page, and what
/// did not fit was left off; a list that does not fit is refused now.
pub const ARGS_PAGES: usize = 32;

/// Where a program's interpreter — the dynamic loader a program built to
/// use shared libraries names (`PT_INTERP`) — is put: a random number of
/// pages into the terabyte from two terabytes up, clear of where programs
/// are linked, their heaps, the C layer's arena, the stacks and `execve`'s
/// staging. Mirrored as `QUARK_INTERP_BASE` in `quark/layout.h`.
pub const INTERP_BASE: usize = 0x200_0000_0000;
const INTERP_WINDOW_PAGES: usize = 1 << 28;

/// Where a program linked to be put anywhere is put — a PIE, `ET_DYN`, as a
/// program built for Linux usually is: a random number of pages into the
/// terabyte above the interpreter's, as everything a program has is chosen.
/// Mirrored as `QUARK_PIE_BASE` in `quark/layout.h`.
pub const PIE_BASE: usize = 0x300_0000_0000;
const PIE_WINDOW_PAGES: usize = 1 << 28;

/// The loader a program linked for Linux's musl asks for, by Linux's name.
/// Quark's own programs ask for `/usr/lib/ld-musl-x86_64.so.1`; a system
/// that runs Linux's keeps the same file at this name too. Mirrored as
/// `QUARK_LINUX_INTERP` in `quark/layout.h`.
pub const LINUX_INTERP: &[u8] = b"/lib/ld-musl-x86_64.so.1";

/// The key in the auxiliary vector that says a program asked for
/// [`LINUX_INTERP`]: it was built for Linux, and its own code may make
/// Linux's system calls, which the C library it runs on then answers
/// (`SYS_SYSCALL_TRAP`). Quark's own number, far above Linux's. Mirrored
/// as `QUARK_AT_LINUX` in `quark/layout.h`.
pub const AT_QUARK_LINUX: u64 = 0x5155;

/// The most address space a program may span, from its first loaded page to
/// its last: a gigabyte. The image is built laid out as the child will see it,
/// so this is what a spawner must leave free at `Scratch::elf`.
pub const MAX_IMAGE_SPAN: usize = 1 << 30;

/// Loadable segments a program may have. A program built here has three or
/// four.
const MAX_SEGMENTS: usize = MAX_PHDRS;

/// The most pages `sys_mmap`, `sys_munmap` and `sys_addrspace_give` take at
/// once.
const CHUNK: usize = 256;

/// Scratch virtual addresses in the caller's own address space, where a
/// program is built before it is moved into the child.
///
/// Each caller needs its own, since two spawners building at the same address
/// would collide. A range of `STACK_PAGES` pages must be free at `stack`, one
/// page at `args`, and [`MAX_IMAGE_SPAN`] at `elf`. All three are empty again
/// when a load returns, whether or not it succeeded.
#[derive(Clone, Copy)]
pub struct Scratch {
    pub elf: usize,
    pub stack: usize,
    pub args: usize,
}

/// A task that has been created and loaded but not yet started.
#[derive(Clone, Copy)]
pub struct Spawned {
    pub tid: usize,
    /// Where the task starts: the program's entry, or its interpreter's.
    pub entry: u64,
    pub stack_top: u64,
    pub cr3: usize,
    /// The program's own header table, verbatim, for the argument page.
    phdrs: [u8; MAX_PHDRS * PHDR_SIZE],
    phnum: usize,
    /// The program's own entry, which an interpreter is told (`AT_ENTRY`).
    program_entry: u64,
    /// Where the interpreter was put, or nought (`AT_BASE`).
    interp_base: u64,
    /// How far the program was moved from where it was linked: nought, but
    /// for a PIE.
    program_base: u64,
    /// It asked for [`LINUX_INTERP`] ([`AT_QUARK_LINUX`]).
    linux: bool,
}

impl Spawned {
    /// A placeholder for an array of spawned tasks not yet filled in. It names
    /// no task; starting it fails.
    pub const EMPTY: Spawned = Spawned {
        tid: 0,
        entry: 0,
        stack_top: 0,
        cr3: 0,
        phdrs: [0; MAX_PHDRS * PHDR_SIZE],
        phnum: 0,
        program_entry: 0,
        interp_base: 0,
        program_base: 0,
        linux: false,
    };

    /// Run it. Nothing happens until this is called, which is what lets a
    /// caller wire capabilities, file descriptors and pipes first.
    ///
    /// The stack pointer starts where [`set_args_env`] put what a C program
    /// reads there, [`BLOCK_AT`] into the stack's top [`ARGS_PAGES`]. The
    /// kernel takes the value it is given down to sixteen and eight below
    /// that, as a call would have left it, so it is given the next sixteen
    /// up.
    pub fn start(&self) -> Result<(), ()> {
        let rsp = self.stack_top - (ARGS_PAGES * PAGE_SIZE) as u64 + BLOCK_AT as u64 + 8;
        syscall::sys_task_start(self.tid, self.entry, rsp, self.cr3)
    }

    /// Take back a child that will not be started after all: the task, and
    /// the address space with everything that was moved into it.
    ///
    /// A spawner that builds a program and then is told no — `login`, when
    /// the password was wrong — has one of these each time, and a task slot
    /// and an image's worth of memory with it.
    pub fn discard(self) {
        let _ = syscall::sys_task_kill(self.tid);
        let _ = syscall::sys_wait_for(self.tid);
        let _ = syscall::sys_addrspace_destroy(self.cr3);
    }
}

#[repr(C)]
struct Elf64Header {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
}

#[repr(C)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

/// A loadable segment, checked.
#[derive(Clone, Copy)]
struct Segment {
    /// The pages it occupies in the child, `[first, end)`.
    first: usize,
    end: usize,
    vaddr: usize,
    /// Where its initialised bytes end in the child.
    vend: usize,
    offset: usize,
    filesz: usize,
    writable: bool,
}

impl Segment {
    const EMPTY: Segment = Segment {
        first: 0,
        end: 0,
        vaddr: 0,
        vend: 0,
        offset: 0,
        filesz: 0,
        writable: false,
    };
}

/// Create a task and load `elf` into a fresh address space for it.
///
/// The returned task is not running; call [`Spawned::start`].
///
/// The image is built in the caller's own memory, laid out as the child will
/// see it, and then moved into the child with `sys_addrspace_give`. Moved
/// pages are the child's: they are freed when it is gone, and the caller keeps
/// no way to reach them. Every spawner used to lend the child frames it had
/// allocated itself, which kept them for as long as the *spawner* lived — a
/// megabyte and a half for every program the shell ran, until nothing more
/// could be loaded.
///
/// On failure the task and address space created so far are left behind.
/// That matches what the three copies did, and cleaning it up properly wants a
/// teardown syscall that does not exist yet. The caller's scratch ranges are
/// always emptied.
pub fn load(elf: &[u8], scratch: &Scratch) -> Result<Spawned, ()> {
    load_with(elf, None, scratch)
}

/// [`load`], and the interpreter the program names, if it names one
/// ([`interpreter`]): a shared object, loaded at a random base in a window
/// of its own ([`INTERP_BASE`]), which the task starts in and which is told
/// where the program is.
pub fn load_with(elf: &[u8], interp: Option<&[u8]>, scratch: &Scratch) -> Result<Spawned, ()> {
    if elf.len() < EHDR_SIZE || elf[0..4] != ELF_MAGIC {
        return Err(());
    }

    let hdr = unsafe { &*(elf.as_ptr() as *const Elf64Header) };
    let entry = hdr.e_entry;
    let phoff = hdr.e_phoff as usize;
    let phentsize = hdr.e_phentsize as usize;
    let phnum = hdr.e_phnum as usize;

    // A program header shorter than the struct would have us read past the
    // entry into whatever follows.
    if phentsize < PHDR_SIZE {
        return Err(());
    }

    // The table, for the child to be told about. All of it or none: a
    // partial table is a program being told it has fewer segments than it
    // does, and the one it is looking for may be past the cut.
    let mut phdrs = [0u8; MAX_PHDRS * PHDR_SIZE];
    let mut kept = 0usize;
    let table_end = phoff
        .checked_add(phnum.saturating_mul(phentsize))
        .unwrap_or(usize::MAX);
    if phnum <= MAX_PHDRS && table_end <= elf.len() {
        for i in 0..phnum {
            let from = phoff + i * phentsize;
            phdrs[i * PHDR_SIZE..(i + 1) * PHDR_SIZE]
                .copy_from_slice(&elf[from..from + PHDR_SIZE]);
        }
        kept = phnum;
    }

    let mut segs = [Segment::EMPTY; MAX_SEGMENTS];
    let n = segments(elf, phoff, phentsize, phnum, &mut segs).ok_or(())?;
    // A program linked to be put anywhere is put somewhere of its own, at
    // random; one linked to be somewhere is put there.
    let moved = if hdr.e_type == ET_DYN {
        let at = PIE_BASE + crate::layout::random_pages(PIE_WINDOW_PAGES) * PAGE_SIZE;
        let by = at.wrapping_sub(segs[0].first);
        for s in &mut segs[..n] {
            s.first = s.first.wrapping_add(by);
            s.end = s.end.wrapping_add(by);
            s.vaddr = s.vaddr.wrapping_add(by);
            s.vend = s.vend.wrapping_add(by);
        }
        by
    } else {
        0
    };
    let entry = entry.wrapping_add(moved as u64);
    let segs = &segs[..n];
    let base = segs[0].first;

    // The interpreter, at its base: its segments are where it was linked,
    // from nought, plus that.
    let mut isegs = [Segment::EMPTY; MAX_SEGMENTS];
    let mut ientry = 0;
    let mut inum = 0;
    let interp_base = match interp {
        None => 0,
        Some(i) => {
            if i.len() < EHDR_SIZE || i[0..4] != ELF_MAGIC {
                return Err(());
            }
            let ih = unsafe { &*(i.as_ptr() as *const Elf64Header) };
            if ih.e_type != ET_DYN || (ih.e_phentsize as usize) < PHDR_SIZE {
                return Err(());
            }
            inum = segments(i, ih.e_phoff as usize, ih.e_phentsize as usize, ih.e_phnum as usize, &mut isegs)
                .ok_or(())?;
            let at = INTERP_BASE + crate::layout::random_pages(INTERP_WINDOW_PAGES) * PAGE_SIZE;
            for s in &mut isegs[..inum] {
                s.first += at;
                s.end += at;
                s.vaddr += at;
                s.vend += at;
            }
            ientry = at as u64 + ih.e_entry;
            at
        }
    };
    let isegs = &isegs[..inum];

    let cr3 = syscall::sys_addrspace_create()?;
    let Ok(tid) = syscall::sys_task_create_in(cr3 as u64) else {
        let _ = syscall::sys_addrspace_destroy(cr3);
        return Err(());
    };

    // Where the child's stack ends, which is its own: chosen for it.
    let stack_top = STACK_TOP - crate::layout::random_pages(STACK_WINDOW_PAGES) * PAGE_SIZE;
    // The program, then the interpreter, each built in the same scratch
    // range and moved out of it before the next.
    let ibase = isegs.first().map_or(0, |s| s.first);
    let loaded = build(elf, segs, base, scratch.elf)
        .and_then(|()| give_image(cr3, segs, base, scratch.elf))
        .and_then(|()| match interp {
            Some(i) => build(i, isegs, ibase, scratch.elf).and_then(|()| give_image(cr3, isegs, ibase, scratch.elf)),
            None => Ok(()),
        })
        .and_then(|()| give_stack(cr3, scratch.stack, stack_top));
    if loaded.is_err() {
        // What was not given is still ours. Left mapped it would be in the
        // way of the next load, which never maps over anything.
        for s in segs {
            release(scratch.elf + (s.first - base), (s.end - s.first) / PAGE_SIZE);
        }
        for s in isegs {
            release(scratch.elf + (s.first - ibase), (s.end - s.first) / PAGE_SIZE);
        }
        release(scratch.stack, STACK_PAGES);
        // And the child that was being built, which nobody else can name:
        // left, it was a task and an address space for every program that
        // would not load.
        let _ = syscall::sys_task_kill(tid);
        let _ = syscall::sys_wait_for(tid);
        let _ = syscall::sys_addrspace_destroy(cr3);
        return Err(());
    }

    Ok(Spawned {
        tid,
        entry: if inum > 0 { ientry } else { entry },
        stack_top: stack_top as u64,
        cr3,
        phdrs,
        phnum: kept,
        program_entry: entry,
        interp_base: interp_base as u64,
        program_base: moved as u64,
        linux: interpreter(elf) == Some(LINUX_INTERP),
    })
}

/// The interpreter `elf` names (`PT_INTERP`), if it names one: a path,
/// without its nought.
pub fn interpreter(elf: &[u8]) -> Option<&[u8]> {
    if elf.len() < EHDR_SIZE || elf[0..4] != ELF_MAGIC {
        return None;
    }
    let hdr = unsafe { &*(elf.as_ptr() as *const Elf64Header) };
    let (phoff, phentsize) = (hdr.e_phoff as usize, hdr.e_phentsize as usize);
    if phentsize < PHDR_SIZE {
        return None;
    }
    (0..hdr.e_phnum as usize).find_map(|i| {
        let at = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if at.checked_add(PHDR_SIZE)? > elf.len() {
            return None;
        }
        let ph = unsafe { &*(elf.as_ptr().add(at) as *const Elf64Phdr) };
        if ph.p_type != PT_INTERP {
            return None;
        }
        let (from, len) = (ph.p_offset as usize, ph.p_filesz as usize);
        let path = elf.get(from..from.checked_add(len)?)?;
        let path = &path[..path.iter().position(|&b| b == 0).unwrap_or(path.len())];
        (!path.is_empty()).then_some(path)
    })
}

/// Read and check the loadable segments into `out`, returning how many there
/// are. `None` if there are none, or too many, or any is malformed: out of
/// address order, overlapping, claiming bytes past the end of the file, or
/// making the image wider than [`MAX_IMAGE_SPAN`].
fn segments(
    elf: &[u8],
    phoff: usize,
    phentsize: usize,
    phnum: usize,
    out: &mut [Segment; MAX_SEGMENTS],
) -> Option<usize> {
    let mut n = 0;
    for i in 0..phnum {
        let at = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if at.checked_add(phentsize)? > elf.len() {
            return None;
        }
        let ph = unsafe { &*(elf.as_ptr().add(at) as *const Elf64Phdr) };
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let vaddr = ph.p_vaddr as usize;
        let memsz = ph.p_memsz as usize;
        let filesz = ph.p_filesz as usize;
        let offset = ph.p_offset as usize;
        if filesz > memsz || offset.checked_add(filesz)? > elf.len() {
            return None;
        }
        let vend = vaddr.checked_add(memsz)?;
        let seg = Segment {
            first: vaddr & !(PAGE_SIZE - 1),
            end: vend.checked_add(PAGE_SIZE - 1)? & !(PAGE_SIZE - 1),
            vaddr,
            vend,
            offset,
            filesz,
            writable: ph.p_flags & 2 != 0,
        };
        // In address order and apart, which the format requires. Two can
        // still share a page — one ending part way through it and the next
        // beginning there — and building the image as one piece is what makes
        // that work: both write into the same page.
        if n > 0 && seg.vaddr < out[n - 1].vend {
            return None;
        }
        let base = if n == 0 { seg.first } else { out[0].first };
        if n == MAX_SEGMENTS || seg.end - base > MAX_IMAGE_SPAN {
            return None;
        }
        out[n] = seg;
        n += 1;
    }
    if n == 0 { None } else { Some(n) }
}

/// Lay the segments out at `at`, as the child will see them from `base`.
fn build(elf: &[u8], segs: &[Segment], base: usize, at: usize) -> Result<(), ()> {
    let mut mapped = base;
    for s in segs {
        // A page shared with the segment before is already there, holding
        // that segment's bytes, which must survive.
        let start = s.first.max(mapped);
        if start < s.end {
            map_fresh(at + (start - base), (s.end - start) / PAGE_SIZE)?;
        }
        mapped = s.end;
        // Fresh memory is zeroed, so the .bss after the file bytes needs
        // nothing more.
        unsafe {
            core::ptr::copy_nonoverlapping(
                elf.as_ptr().add(s.offset),
                (at + (s.vaddr - base)) as *mut u8,
                s.filesz,
            );
        }
    }
    Ok(())
}

/// Move the image built at `at` into the child.
fn give_image(cr3: usize, segs: &[Segment], base: usize, at: usize) -> Result<(), ()> {
    let mut given = base;
    for (i, s) in segs.iter().enumerate() {
        let start = s.first.max(given);
        if start >= s.end {
            continue;
        }
        // A last page that the next segment begins in has to suit both of
        // them, so it goes on its own, writable if any segment in it is.
        let shared = segs.get(i + 1).is_some_and(|next| next.first < s.end);
        let whole = if shared { s.end - PAGE_SIZE } else { s.end };
        give(cr3, start, at + (start - base), (whole - start) / PAGE_SIZE, s.writable)?;
        if shared {
            let last = s.end - PAGE_SIZE;
            let writable = segs.iter().any(|t| t.writable && t.first <= last && last < t.end);
            give(cr3, last, at + (last - base), 1, writable)?;
        }
        given = s.end;
    }
    Ok(())
}

/// Build the stack at `at` and move all of it but its top [`ARGS_PAGES`]
/// into the child. Fresh memory is zeroed, which is all a stack needs; the
/// top is [`set_args_env`]'s, which puts there what a C program finds on its
/// stack when it starts.
fn give_stack(cr3: usize, at: usize, top: usize) -> Result<(), ()> {
    map_fresh(at, STACK_PAGES - ARGS_PAGES)?;
    give(cr3, top - STACK_PAGES * PAGE_SIZE, at, STACK_PAGES - ARGS_PAGES, true)
}

/// Map `pages` of fresh, zeroed memory at `at`, or nothing.
/// Map `pages` pages of fresh memory at `at`, a chunk at a time: where a
/// spawner reads an image to load.
pub fn map_fresh(at: usize, pages: usize) -> Result<(), ()> {
    let mut done = 0;
    while done < pages {
        let n = (pages - done).min(CHUNK);
        if syscall::sys_mmap(at + done * PAGE_SIZE, n).is_err() {
            release(at, done);
            return Err(());
        }
        done += n;
    }
    Ok(())
}

/// Move `pages` pages at `from` in the caller to `virt` in `cr3`.
fn give(cr3: usize, virt: usize, from: usize, pages: usize, writable: bool) -> Result<(), ()> {
    let mut done = 0;
    while done < pages {
        let n = (pages - done).min(CHUNK);
        syscall::sys_addrspace_give(
            cr3,
            virt + done * PAGE_SIZE,
            from + done * PAGE_SIZE,
            n,
            writable as u64,
        )?;
        done += n;
    }
    Ok(())
}

/// Unmap and free whatever is still mapped of `pages` pages at `at`.
pub fn release(at: usize, pages: usize) {
    let mut done = 0;
    while done < pages {
        let n = (pages - done).min(CHUNK);
        let _ = syscall::sys_munmap(at + done * PAGE_SIZE, n);
        done += n;
    }
}

/// The largest program [`load_path`] will read: thirty-two megabytes.
///
/// It was four, which was generous for a program written here and not nearly
/// enough for one that links a toolkit: pango and its dependencies come to
/// seven megabytes of static library before anything is drawn, and GTK is
/// several times that. The cost is address space in the spawner while the
/// image is being read, not memory — the pages are mapped as they are filled
/// and given to the child.
pub const MAX_IMAGE_PAGES: usize = 8192;

/// Read a program from the filesystem, load it, and give back the memory it
/// was read into.
///
/// Every spawner used to stage a program at one fixed address and never free
/// the frames: the shell leaked a program's size in memory every time it ran
/// one. That went unnoticed while programs were a few pages; a test suite of
/// thirty half-megabyte binaries would not have survived it.
///
/// The image is read into ordinary memory, a page per call, each page lent to
/// the VFS to fill. It needs no capability to allocate frames, and nothing
/// here ever knows where the image is in physical memory.
///
/// `image_at` is a free range of `MAX_IMAGE_PAGES` pages in the caller.
/// `grant` sees the image before it is released, which is when a spawner reads
/// the program's manifest; it is called only if loading succeeded.
pub fn load_path(
    vfs_tid: usize,
    path: &[u8],
    image_at: usize,
    scratch: &Scratch,
    grant: impl FnOnce(&[u8], usize),
) -> Result<Spawned, ()> {
    let (image, pages) = read_image(vfs_tid, path, image_at, MAX_IMAGE_PAGES).ok_or(())?;

    // A program built to use shared libraries names the loader that finds
    // them, and is started in it: that file is read next, after the
    // program in the same range.
    let mut interp = None;
    let mut interp_pages = 0;
    if let Some(name) = interpreter(image) {
        match read_image(vfs_tid, name, image_at + pages * PAGE_SIZE, MAX_IMAGE_PAGES - pages) {
            Some((i, n)) => {
                interp = Some(i);
                interp_pages = n;
            }
            None => {
                release(image_at, pages);
                return Err(());
            }
        }
    }

    let loaded = load_with(image, interp.as_deref(), scratch);
    if let Ok(info) = loaded {
        grant(image, info.tid);
    }

    // The child has pages of its own now; this was only ever a copy.
    release(image_at, pages + interp_pages);
    loaded
}

/// Read the file at `path` into fresh pages at `at`, at most `most` of
/// them: the file, and how many pages it took. Nothing is left mapped if it
/// cannot be read whole.
fn read_image(vfs_tid: usize, path: &[u8], at: usize, most: usize) -> Option<(&'static mut [u8], usize)> {
    let (handle, size, _) = crate::vfs::open(vfs_tid, path).ok()?;
    let size = size as usize;
    let pages = size.div_ceil(PAGE_SIZE);
    if pages == 0 || pages > most || map_fresh(at, pages).is_err() {
        let _ = crate::vfs::close(vfs_tid, handle);
        return None;
    }
    let image = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, size) };
    let read_whole = image.chunks_mut(PAGE_SIZE).enumerate().all(|(p, page)| {
        crate::vfs::read(vfs_tid, handle, page, (p * PAGE_SIZE) as u32) == Ok(page.len() as u32)
    });
    let _ = crate::vfs::close(vfs_tid, handle);
    if !read_whole {
        release(at, pages);
        return None;
    }
    Some((image, pages))
}

/// Write `args` and `env` into the child's argument page, read back by
/// `quark_rt::args`.
///
/// Layout: a count, then each entry as a length followed by its bytes; the
/// arguments first and the environment after them. An entry that would
/// overflow the page is dropped rather than truncated — half an environment
/// variable is worse than a missing one.
///
/// The page is mapped read-only in the child. That is safe for an environment
/// as well as for argv, because nothing writes through these bytes: a C
/// library builds its own array of pointers into them, and `setenv` allocates
/// a new string rather than editing one in place.
pub fn set_args_env(
    info: &Spawned,
    args: &[&[u8]],
    env: &[&[u8]],
    scratch: &Scratch,
) -> Result<(), ()> {
    syscall::sys_mmap(scratch.args, 1)?;

    let base = scratch.args as *mut u8;
    unsafe {
        let mut offset = 0usize;
        for section in [args, env] {
            let count_at = offset;
            offset += 8;
            let mut written = 0u64;
            for item in section {
                // Stop short of the program headers, which own the end of the
                // page whatever the arguments are.
                if offset + 8 + item.len() > PHDRS_AT {
                    break;
                }
                *(base.add(offset) as *mut u64) = item.len() as u64;
                offset += 8;
                core::ptr::copy_nonoverlapping(item.as_ptr(), base.add(offset), item.len());
                offset += item.len();
                written += 1;
            }
            *(base.add(count_at) as *mut u64) = written;
        }

        write_phdrs(base, info);
    }

    let given = syscall::sys_addrspace_give(info.cr3, ARGS_PAGE_ADDR, scratch.args, 1, 0);
    if given.is_err() {
        release(scratch.args, 1);
    }
    // And what it was started as, which is what `ps` and `/proc` say it is.
    // A kernel too old to keep it says no, and that is all.
    let _ = syscall::sys_program_name_set(info.tid, args);
    given.and_then(|()| stack_page(info, args, env, scratch))
}

/// The auxiliary vector's keys, Linux's numbers.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_HWCAP: u64 = 16;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;

/// `AT_HWCAP2`: bit 1, `HWCAP2_FSGSBASE`, where a program may read and write
/// its own FS and GS bases — the processor has the instructions, and the
/// kernel is one that turns them on (4.5 or later). Told otherwise, a program
/// that used them would be ended with SIGILL, or would think a kernel that
/// does not keep its GS base across a switch kept it.
fn hwcap2() -> u64 {
    let leaves = core::arch::x86_64::__cpuid(0).eax;
    let has = leaves >= 7 && core::arch::x86_64::__cpuid_count(7, 0).ebx & 1 != 0;
    let (major, minor) = syscall::sys_abi_version();
    if has && (major > 4 || (major == 4 && minor >= 5)) { 2 } else { 0 }
}

/// Where in the stack's top [`ARGS_PAGES`] the count of arguments is: eight
/// bytes in, eight below a multiple of sixteen. That is where a task begins
/// with its stack pointer — as a call leaves one, which is what the kernel
/// makes of whatever it is told (`SYS_TASK_START`) and what a Rust entry
/// point expects; a C library's entry aligns it again for itself.
pub const BLOCK_AT: usize = 8;

fn put_word(page: &mut [u8], i: usize, v: u64) {
    let at = BLOCK_AT + i * 8;
    page[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Write the stack's top [`ARGS_PAGES`] and move them into the child: what
/// Linux's kernel leaves on a program's stack, which is what a C library's
/// entry reads — and a dynamic loader's, which reads it before it can call
/// anything at all. From [`BLOCK_AT`] up: the number of arguments, a
/// pointer to each, a nought, a pointer to each variable of the
/// environment, a nought, and the auxiliary vector. At the very top,
/// sixteen random bytes (`AT_RANDOM`), and below them the strings. The task
/// starts with its stack pointer at the count ([`Spawned::start`]).
///
/// All of it or none: a list that does not fit is refused, as Linux refuses
/// one past its limit (E2BIG), where it used to be left off the end. The
/// program's headers are the copy on the argument page (`AT_PHDR`), whose
/// `PT_PHDR` says it is there — so a dynamic loader works out that the
/// program was not moved.
fn stack_page(info: &Spawned, args: &[&[u8]], env: &[&[u8]], scratch: &Scratch) -> Result<(), ()> {
    let span = ARGS_PAGES * PAGE_SIZE;
    let there = info.stack_top as usize - span;
    let (uid, gid) = syscall::sys_get_tuid(info.tid).unwrap_or((0, 0));
    let random_at = span - 16;
    let mut aux = [
        (AT_PHDR, (ARGS_PAGE_ADDR + PHDRS_AT + 16) as u64),
        (AT_PHENT, PHDR_SIZE as u64),
        (AT_PHNUM, info.phnum as u64),
        (AT_PAGESZ, PAGE_SIZE as u64),
        (AT_BASE, info.interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, info.program_entry),
        (AT_UID, uid as u64),
        (AT_EUID, uid as u64),
        (AT_GID, gid as u64),
        (AT_EGID, gid as u64),
        (AT_SECURE, 0),
        (AT_HWCAP, core::arch::x86_64::__cpuid(1).edx as u64),
        (AT_HWCAP2, hwcap2()),
        (AT_RANDOM, (there + random_at) as u64),
        (AT_QUARK_LINUX, info.linux as u64),
    ];
    // A program with too many headers to copy is told of none.
    if info.phnum == 0 {
        aux[0] = (AT_PHNUM, 0);
    }

    // Whether it all fits: words from the bottom, strings from the top.
    let words = 3 + args.len() + env.len() + 2 * (aux.len() + 1);
    let strings: usize = args.iter().chain(env.iter()).map(|item| item.len() + 1).sum();
    if BLOCK_AT + words * 8 + strings > random_at {
        return Err(());
    }

    map_fresh(scratch.stack, ARGS_PAGES)?;
    let block = unsafe { core::slice::from_raw_parts_mut(scratch.stack as *mut u8, span) };
    let _ = syscall::sys_getrandom(&mut block[random_at..]);
    put_word(block, 0, args.len() as u64);
    let mut top = random_at;
    let mut i = 1;
    for list in [args, env] {
        for item in list {
            top -= item.len() + 1;
            block[top..top + item.len()].copy_from_slice(item);
            block[top + item.len()] = 0;
            put_word(block, i, (there + top) as u64);
            i += 1;
        }
        put_word(block, i, 0);
        i += 1;
    }
    for (key, value) in aux {
        put_word(block, i, key);
        put_word(block, i + 1, value);
        i += 2;
    }
    put_word(block, i, AT_NULL);
    put_word(block, i + 1, 0);

    let given = give(info.cr3, there, scratch.stack, ARGS_PAGES, true);
    if given.is_err() {
        release(scratch.stack, ARGS_PAGES);
    }
    given
}

/// Put the program header table at the end of the argument page.
///
/// A `PT_PHDR` entry is rewritten to say the table is where this copy is,
/// less how far the program was moved. A C library takes the difference
/// between `AT_PHDR` and that entry's address as the load base, and for a
/// program that is not relocated the base has to come out as zero — for a
/// PIE, as where it was put; left as it was, every address derived from the
/// headers — the thread-local template among them — would be off by the
/// distance to this page.
unsafe fn write_phdrs(base: *mut u8, info: &Spawned) {
    let at = PHDRS_AT;
    let table = ARGS_PAGE_ADDR + at + 16;
    unsafe {
        *(base.add(at) as *mut u64) = PHDR_SIZE as u64;
        *(base.add(at + 8) as *mut u64) = info.phnum as u64;
        for i in 0..info.phnum {
            let dst = base.add(at + 16 + i * PHDR_SIZE);
            core::ptr::copy_nonoverlapping(
                info.phdrs.as_ptr().add(i * PHDR_SIZE),
                dst,
                PHDR_SIZE,
            );
            let ph = &mut *(dst as *mut Elf64Phdr);
            if ph.p_type == PT_PHDR {
                ph.p_vaddr = (table as u64).wrapping_sub(info.program_base);
                ph.p_paddr = ph.p_vaddr;
            }
        }
    }
}

/// As [`set_args_env`], with no environment.
pub fn set_args(info: &Spawned, args: &[&[u8]], scratch: &Scratch) -> Result<(), ()> {
    set_args_env(info, args, &[], scratch)
}
