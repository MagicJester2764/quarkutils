//! ext4: feature flags and extent trees.
//!
//! ext4 is ext2 with the same superblock, the same block groups, the same
//! inode table and the same directory entries — so most of this server did not
//! have to change. What changed is how an inode says where its data is.
//!
//! ext2 stores fifteen block pointers: twelve direct, then single, double and
//! triple indirect. A large file therefore costs an extra read per indirect
//! level, and a contiguous file wastes a pointer per block describing what a
//! range could have said in one entry.
//!
//! ext4 reuses the same sixty bytes as the root of a B-tree of *extents*, each
//! naming a run of contiguous blocks. A file laid out contiguously needs one
//! entry however large it is, and four of them fit in the inode itself, so
//! most files need no extra read at all.
//!
//! ```text
//!     i_block[15], 60 bytes:
//!     +--------------+----------+----------+----------+----------+
//!     | extent hdr   | entry 0  | entry 1  | entry 2  | entry 3  |
//!     | magic 0xF30A |          |          |          |          |
//!     | entries,depth|          |          |          |          |
//!     +--------------+----------+----------+----------+----------+
//!        depth == 0: entries are extents, naming physical runs
//!        depth  > 0: entries are indices, naming blocks of more entries
//! ```

use crate::ext2::{read_block_bytes, Ext2Inode, Ext2State};
use crate::ext2_alloc;
use crate::{ERR_IO, ERR_NOT_FOUND, ERR_NOT_SUPPORTED};

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------
//
// Three sets, and the difference between them is the whole compatibility
// story. A *compat* feature can be ignored entirely. A *ro_compat* feature
// changes something a writer must maintain, so an implementation that does not
// know it may still read. An *incompat* feature changes the format itself, so
// an implementation that does not know it must not touch the filesystem at all
// — reading would return the wrong bytes, silently.

pub const COMPAT_HAS_JOURNAL: u32 = 0x0004;
pub const COMPAT_DIR_INDEX: u32 = 0x0020;

pub const INCOMPAT_FILETYPE: u32 = 0x0002;
/// The journal holds committed data the filesystem does not yet reflect.
///
/// Listed as supported because supporting it *is* replaying the journal, which
/// happens at mount. A filesystem that says this and has no journal to replay
/// it with is mounted read-only instead: it is describing work nothing can
/// finish, and writing over it would bury whatever was interrupted.
pub const INCOMPAT_RECOVER: u32 = 0x0004;
pub const INCOMPAT_META_BG: u32 = 0x0010;
pub const INCOMPAT_EXTENTS: u32 = 0x0040;
pub const INCOMPAT_64BIT: u32 = 0x0080;
pub const INCOMPAT_MMP: u32 = 0x0100;
pub const INCOMPAT_FLEX_BG: u32 = 0x0200;
/// The checksum seed is stored in the superblock rather than derived from the
/// UUID. Incompatible because a writer that did not know it would compute
/// checksums against the wrong seed.
pub const INCOMPAT_CSUM_SEED: u32 = 0x2000;
pub const INCOMPAT_INLINE_DATA: u32 = 0x8000;

pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
pub const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
pub const RO_COMPAT_HUGE_FILE: u32 = 0x0008;
pub const RO_COMPAT_GDT_CSUM: u32 = 0x0010;
pub const RO_COMPAT_DIR_NLINK: u32 = 0x0020;
pub const RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;
pub const RO_COMPAT_BIGALLOC: u32 = 0x0200;
pub const RO_COMPAT_METADATA_CSUM: u32 = 0x0400;

/// Incompatible features this server understands well enough to mount.
///
/// `FILETYPE` and `FLEX_BG` cost nothing: the first is a field this already
/// reads, and the second only moves where a group's bitmaps live, which the
/// descriptor already says. `EXTENTS` and `64BIT` are what this module adds.
///
/// `CSUM_SEED` is here on a condition rather than on its own merits: it says
/// only where the checksum seed comes from, which matters to a writer of
/// checksums and to nobody else. `METADATA_CSUM` is not in
/// [`RO_COMPAT_WRITE_SUPPORTED`], so a filesystem using either is mounted
/// read-only and this server never computes a checksum at all. Implementing
/// checksums means honouring the seed in the same change.
pub const INCOMPAT_SUPPORTED: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_FLEX_BG
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_RECOVER;

/// Read-only-compatible features that also do not stop us *writing*.
///
/// The rest are not refusals of the filesystem, only of modifying it: an
/// unlisted one means something a writer would have to maintain and this does
/// not, so the mount stays read-only rather than quietly corrupting it.
pub const RO_COMPAT_WRITE_SUPPORTED: u32 = RO_COMPAT_SPARSE_SUPER
    | RO_COMPAT_LARGE_FILE
    | RO_COMPAT_HUGE_FILE
    | RO_COMPAT_DIR_NLINK
    | RO_COMPAT_EXTRA_ISIZE
    | RO_COMPAT_METADATA_CSUM;

/// This group's inode bitmap has never been written, so what is on disk for it
/// says nothing about which inodes are free.
pub const BG_INODE_UNINIT: u16 = 0x0001;
/// The same for its block bitmap.
pub const BG_BLOCK_UNINIT: u16 = 0x0002;

/// Set in `i_flags` when an inode's `i_block` is an extent tree.
pub const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
/// Set when the inode's data lives in the inode itself.
pub const EXT4_INLINE_DATA_FL: u32 = 0x1000_0000;

// ---------------------------------------------------------------------------
// Extent tree
// ---------------------------------------------------------------------------

const EXTENT_MAGIC: u16 = 0xF30A;
const EXTENT_HEADER_SIZE: usize = 12;
const EXTENT_ENTRY_SIZE: usize = 12;

/// A length above this marks the extent *uninitialised*: the blocks are
/// allocated but have never been written, and read as zeros.
const INIT_MAX_LEN: u16 = 32768;

/// How deep a tree this will walk before deciding it is looping.
///
/// ext4 never builds deeper than five levels, and a corrupt `eh_depth` that
/// pointed at itself would otherwise spin forever inside a filesystem server
/// every other program is blocked on.
const MAX_DEPTH: u16 = 5;

struct ExtentHeader {
    entries: u16,
    max: u16,
    depth: u16,
}

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn parse_header(buf: &[u8]) -> Result<ExtentHeader, u64> {
    if buf.len() < EXTENT_HEADER_SIZE || read_u16(buf, 0) != EXTENT_MAGIC {
        return Err(ERR_IO);
    }
    Ok(ExtentHeader {
        entries: read_u16(buf, 2),
        max: read_u16(buf, 4),
        depth: read_u16(buf, 6),
    })
}

/// Does this inode use extents rather than the ext2 pointer array?
pub fn uses_extents(inode: &Ext2Inode) -> bool {
    inode.i_flags & EXT4_EXTENTS_FL != 0
}

/// The inode's `i_block` as the 60 raw bytes the extent root occupies.
fn root_bytes(inode: &Ext2Inode) -> [u8; 60] {
    let mut out = [0u8; 60];
    for j in 0..15 {
        out[j * 4..j * 4 + 4].copy_from_slice(&inode.i_block[j].to_le_bytes());
    }
    out
}

/// Physical block holding logical block `logical` of `inode`, or 0 for a hole.
///
/// Walks from the root in the inode down to a leaf, choosing at each level the
/// last entry whose first logical block is not past the target. That is the
/// only entry that can cover it: entries are sorted and cover disjoint ranges.
pub fn extent_lookup(
    ext2: &Ext2State,
    inode: &Ext2Inode,
    logical: u32,
) -> Result<u32, u64> {
    let root = root_bytes(inode);
    let mut header = parse_header(&root)?;

    // The root lives in 60 bytes of inode, so it holds at most four entries.
    let entry_at = |i: usize, out: &mut [u8; EXTENT_ENTRY_SIZE]| -> Result<(), u64> {
        let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        if off + EXTENT_ENTRY_SIZE > root.len() {
            return Err(ERR_IO);
        }
        out.copy_from_slice(&root[off..off + EXTENT_ENTRY_SIZE]);
        Ok(())
    };

    let mut in_root = true;
    let mut block: u32 = 0;
    let mut depth_guard = 0u16;

    loop {
        if depth_guard > MAX_DEPTH {
            return Err(ERR_IO);
        }
        depth_guard += 1;

        // An interior node's entry count must fit the space it has, or a
        // corrupt value would walk us off the end of the block.
        let capacity = if in_root {
            (60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE
        } else {
            (ext2.block_size as usize - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE
        };
        if header.entries as usize > capacity || header.max as usize > capacity {
            return Err(ERR_IO);
        }

        // Last entry whose first block is <= the one we want.
        let mut chosen: Option<[u8; EXTENT_ENTRY_SIZE]> = None;
        let mut buf = [0u8; EXTENT_ENTRY_SIZE];
        for i in 0..header.entries as usize {
            if in_root {
                entry_at(i, &mut buf)?;
            } else {
                let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
                read_block_bytes(ext2, block, off, &mut buf)?;
            }
            if read_u32(&buf, 0) > logical {
                break;
            }
            chosen = Some(buf);
        }

        let entry = match chosen {
            Some(e) => e,
            // Before the first extent: a hole at the front of the file.
            None => return Ok(0),
        };

        if header.depth == 0 {
            // Leaf. ee_block, ee_len, ee_start_hi, ee_start_lo.
            let ee_block = read_u32(&entry, 0);
            let raw_len = read_u16(&entry, 4);
            let start_hi = read_u16(&entry, 6) as u64;
            let start_lo = read_u32(&entry, 8) as u64;

            // An uninitialised extent is allocated but unwritten. Reporting a
            // hole is exactly right for a reader: its blocks read as zeros,
            // and that is what a hole already means here.
            if raw_len > INIT_MAX_LEN {
                return Ok(0);
            }
            let len = raw_len as u32;
            if len == 0 || logical < ee_block || logical - ee_block >= len {
                return Ok(0); // hole between extents
            }

            let start = (start_hi << 32) | start_lo;
            let phys = start + (logical - ee_block) as u64;
            return u32::try_from(phys).map_err(|_| ERR_IO);
        }

        // Interior. ei_block, ei_leaf_lo, ei_leaf_hi.
        let leaf_lo = read_u32(&entry, 4) as u64;
        let leaf_hi = read_u16(&entry, 8) as u64;
        let next = (leaf_hi << 32) | leaf_lo;
        block = u32::try_from(next).map_err(|_| ERR_IO)?;
        if block == 0 {
            return Ok(0);
        }

        let mut hdr_buf = [0u8; EXTENT_HEADER_SIZE];
        read_block_bytes(ext2, block, 0, &mut hdr_buf)?;
        let next_header = parse_header(&hdr_buf)?;

        // Depth must strictly decrease on the way down.
        if next_header.depth >= header.depth {
            return Err(ERR_IO);
        }
        header = next_header;
        in_root = false;
    }
}

/// Set up an inode's `i_block` as an empty extent root.
///
/// A file created on an ext4 filesystem must use extents: the block pointer
/// array is not a valid alternative once `INCOMPAT_EXTENTS` is set, and a
/// reader would try to parse the pointers as an extent header.
pub fn init_extent_root(inode: &mut Ext2Inode) {
    let mut root = [0u8; 60];
    root[0..2].copy_from_slice(&EXTENT_MAGIC.to_le_bytes());
    root[2..4].copy_from_slice(&0u16.to_le_bytes()); // entries
    let max = ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16;
    root[4..6].copy_from_slice(&max.to_le_bytes());
    root[6..8].copy_from_slice(&0u16.to_le_bytes()); // depth: a leaf
    root[8..12].copy_from_slice(&0u32.to_le_bytes()); // generation

    for j in 0..15 {
        inode.i_block[j] = read_u32(&root, j * 4);
    }
    inode.i_flags |= EXT4_EXTENTS_FL;
}

/// Record that logical block `logical` of `inode` now lives at `phys`.
///
/// Only the root leaf is written, which is what a file that fits in four
/// extents needs. Growing past that wants a tree, and [`can_add_extent`] says
/// when a caller has run out of room rather than letting it corrupt one.
///
/// Extending the last extent by one block is the common case — a file written
/// sequentially into freshly allocated, mostly contiguous blocks — and costs
/// no new entry at all.
pub fn extent_insert(
    inode: &mut Ext2Inode,
    logical: u32,
    phys: u32,
) -> Result<(), u64> {
    let mut root = root_bytes(inode);
    let header = parse_header(&root)?;
    if header.depth != 0 {
        // A tree deep enough to have interior nodes needs a real insert, and
        // guessing at one would corrupt a file rather than fail to grow it.
        return Err(ERR_NOT_FOUND);
    }

    let capacity = ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16;
    let entries = header.entries;
    if entries > capacity {
        return Err(ERR_IO);
    }

    // Extend the last extent if this block continues it, both logically and
    // physically. Anything else needs an entry of its own.
    if entries > 0 {
        let off = EXTENT_HEADER_SIZE + (entries as usize - 1) * EXTENT_ENTRY_SIZE;
        let ee_block = read_u32(&root, off);
        let raw_len = read_u16(&root, off + 4);
        if raw_len < INIT_MAX_LEN {
            let start = ((read_u16(&root, off + 6) as u64) << 32) | read_u32(&root, off + 8) as u64;
            let len = raw_len as u32;
            if logical == ee_block + len && phys as u64 == start + len as u64 && raw_len < INIT_MAX_LEN - 1
            {
                root[off + 4..off + 6].copy_from_slice(&(raw_len + 1).to_le_bytes());
                store_root(inode, &root);
                return Ok(());
            }
        }
    }

    if entries >= capacity {
        return Err(ERR_NOT_FOUND);
    }

    // In logical order: a block written into a hole goes before the extents
    // that follow it, or a reader's search would never find it.
    let mut at = entries as usize;
    while at > 0 {
        let prev = EXTENT_HEADER_SIZE + (at - 1) * EXTENT_ENTRY_SIZE;
        if read_u32(&root, prev) < logical {
            break;
        }
        at -= 1;
    }
    let first = EXTENT_HEADER_SIZE + at * EXTENT_ENTRY_SIZE;
    let last = EXTENT_HEADER_SIZE + entries as usize * EXTENT_ENTRY_SIZE;
    root.copy_within(first..last, first + EXTENT_ENTRY_SIZE);

    let off = first;
    root[off..off + 4].copy_from_slice(&logical.to_le_bytes());
    root[off + 4..off + 6].copy_from_slice(&1u16.to_le_bytes());
    root[off + 6..off + 8].copy_from_slice(&0u16.to_le_bytes()); // start_hi
    root[off + 8..off + 12].copy_from_slice(&phys.to_le_bytes());
    root[2..4].copy_from_slice(&(entries + 1).to_le_bytes());
    store_root(inode, &root);
    Ok(())
}

/// Whether [`extent_insert`] can still take a block for this inode.
///
/// Appending to the last extent always can; a new entry needs a free slot in
/// the root. A caller that has run out should stop growing the file rather
/// than allocate a block it cannot record.
pub fn can_add_extent(inode: &Ext2Inode, logical: u32, phys: u32) -> bool {
    let root = root_bytes(inode);
    let Ok(header) = parse_header(&root) else {
        return false;
    };
    if header.depth != 0 {
        return false;
    }
    let capacity = ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16;
    if header.entries < capacity {
        return true;
    }
    if header.entries == 0 {
        return false;
    }
    let off = EXTENT_HEADER_SIZE + (header.entries as usize - 1) * EXTENT_ENTRY_SIZE;
    let ee_block = read_u32(&root, off);
    let raw_len = read_u16(&root, off + 4);
    if raw_len >= INIT_MAX_LEN - 1 {
        return false;
    }
    let start = ((read_u16(&root, off + 6) as u64) << 32) | read_u32(&root, off + 8) as u64;
    logical == ee_block + raw_len as u32 && phys as u64 == start + raw_len as u64
}

/// A leaf entry's run: its first logical block, how many blocks, where they
/// start, and whether it is uninitialised.
fn leaf_run(e: &[u8]) -> Result<(u32, u32, u32, bool), u64> {
    let raw = read_u16(e, 4);
    let uninit = raw > INIT_MAX_LEN;
    let len = if uninit { raw - INIT_MAX_LEN } else { raw } as u32;
    let start = ((read_u16(e, 6) as u64) << 32) | read_u32(e, 8) as u64;
    let start = u32::try_from(start).map_err(|_| ERR_IO)?;
    Ok((read_u32(e, 0), len, start, uninit))
}

/// Free every block the extent tree maps and every block the tree itself
/// occupies, and leave an empty root. Returns how many blocks went.
pub fn free_tree(ext2: &mut Ext2State, inode: &mut Ext2Inode) -> Result<u32, u64> {
    let root = root_bytes(inode);
    let header = parse_header(&root)?;
    if header.entries as usize > (60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE {
        return Err(ERR_IO);
    }
    let mut freed = 0;
    for i in 0..header.entries as usize {
        let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        let mut e = [0u8; EXTENT_ENTRY_SIZE];
        e.copy_from_slice(&root[off..off + EXTENT_ENTRY_SIZE]);
        freed += free_entry(ext2, &e, header.depth, 0)?;
    }
    init_extent_root(inode);
    Ok(freed)
}

/// Free what one entry at `depth` maps, and for an index, its node.
fn free_entry(ext2: &mut Ext2State, e: &[u8; EXTENT_ENTRY_SIZE], depth: u16, guard: u16) -> Result<u32, u64> {
    if guard > MAX_DEPTH {
        return Err(ERR_IO);
    }
    if depth == 0 {
        let (_, len, start, _) = leaf_run(e)?;
        for b in 0..len {
            ext2_alloc::free_block(ext2, start + b)?;
        }
        return Ok(len);
    }
    let child = ((read_u16(e, 8) as u64) << 32) | read_u32(e, 4) as u64;
    let child = u32::try_from(child).map_err(|_| ERR_IO)?;
    let mut hdr = [0u8; EXTENT_HEADER_SIZE];
    read_block_bytes(ext2, child, 0, &mut hdr)?;
    let h = parse_header(&hdr)?;
    let capacity = (ext2.block_size as usize - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE;
    if h.depth + 1 != depth || h.entries as usize > capacity {
        return Err(ERR_IO);
    }
    let mut freed = 0;
    for i in 0..h.entries as usize {
        let mut ce = [0u8; EXTENT_ENTRY_SIZE];
        read_block_bytes(ext2, child, EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE, &mut ce)?;
        freed += free_entry(ext2, &ce, h.depth, guard + 1)?;
    }
    ext2_alloc::free_block(ext2, child)?;
    Ok(freed + 1)
}

/// Free the blocks from logical block `first` on, for a tree that is all root.
///
/// A deeper tree is refused: shortening one means rewriting leaf blocks, which
/// nothing here writes yet. Freeing all of one is [`free_tree`].
pub fn truncate_root(ext2: &mut Ext2State, inode: &mut Ext2Inode, first: u32) -> Result<u32, u64> {
    let root = root_bytes(inode);
    let header = parse_header(&root)?;
    if header.depth != 0 {
        return Err(ERR_NOT_SUPPORTED);
    }
    if header.entries as usize > (60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE {
        return Err(ERR_IO);
    }
    let mut out = root;
    let mut kept = 0usize;
    let mut freed = 0;
    for i in 0..header.entries as usize {
        let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        let mut e = [0u8; EXTENT_ENTRY_SIZE];
        e.copy_from_slice(&root[off..off + EXTENT_ENTRY_SIZE]);
        let (block, len, start, uninit) = leaf_run(&e)?;
        let keep = if block >= first { 0 } else { (first - block).min(len) };
        for b in keep..len {
            ext2_alloc::free_block(ext2, start + b)?;
        }
        freed += len - keep;
        if keep == 0 {
            continue;
        }
        let raw = if uninit { keep as u16 + INIT_MAX_LEN } else { keep as u16 };
        e[4..6].copy_from_slice(&raw.to_le_bytes());
        let to = EXTENT_HEADER_SIZE + kept * EXTENT_ENTRY_SIZE;
        out[to..to + EXTENT_ENTRY_SIZE].copy_from_slice(&e);
        kept += 1;
    }
    for i in kept..(60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE {
        let to = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        out[to..to + EXTENT_ENTRY_SIZE].fill(0);
    }
    out[2..4].copy_from_slice(&(kept as u16).to_le_bytes());
    store_root(inode, &out);
    Ok(freed)
}

fn store_root(inode: &mut Ext2Inode, root: &[u8; 60]) {
    for j in 0..15 {
        inode.i_block[j] = read_u32(root, j * 4);
    }
}

// ---------------------------------------------------------------------------
// A tree past the inode
// ---------------------------------------------------------------------------
//
// Four extents fit in the inode. A file in more pieces than that — written
// a block here and a block there, on a disk that has been used — needs the
// tree: the root becomes an index of blocks of extents, each holding as many
// as the block has room for (340 in four kilobytes), and when the index in
// the root is full too, the root's index goes into a block of its own and
// the tree is a level deeper. Each block of the tree carries a checksum in
// its last four bytes, seeded with its inode's, where the filesystem asks
// for them. Until this, a file that needed a fifth piece could not be
// written past it.

/// A block of the tree, as it is being changed: the path down from the
/// root holds one a level.
static mut NODES: [[u8; 4096]; MAX_DEPTH as usize + 1] = [[0; 4096]; MAX_DEPTH as usize + 1];

fn node(level: usize) -> &'static mut [u8; 4096] {
    unsafe { &mut (*core::ptr::addr_of_mut!(NODES))[level] }
}

/// How many entries a block of the tree holds: what is left of it after the
/// header, the checksum's four bytes at its end not counted.
fn block_capacity(ext2: &Ext2State) -> u16 {
    ((ext2.block_size as usize - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16
}

fn put_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn header_bytes(b: &mut [u8], entries: u16, max: u16, depth: u16) {
    put_u16(b, 0, EXTENT_MAGIC);
    put_u16(b, 2, entries);
    put_u16(b, 4, max);
    put_u16(b, 6, depth);
    put_u32(b, 8, 0);
}

/// Read block `block` of the tree into `out`.
fn read_node(ext2: &Ext2State, block: u32, out: &mut [u8; 4096]) -> Result<(), u64> {
    read_block_bytes(ext2, block, 0, &mut out[..ext2.block_size as usize])
}

/// Write block `block` of the tree from `buf`, its checksum set where the
/// filesystem keeps them.
fn write_node(ext2: &Ext2State, ino: u32, inode: &Ext2Inode, block: u32, buf: &mut [u8; 4096]) -> Result<(), u64> {
    let bs = ext2.block_size as usize;
    if crate::csum::enabled(ext2) {
        let tail = EXTENT_HEADER_SIZE + read_u16(buf, 4) as usize * EXTENT_ENTRY_SIZE;
        if tail + 4 <= bs {
            let seed = crate::csum::inode_seed(ext2, ino, inode.i_generation);
            let sum = crate::csum::crc32c(seed, &buf[..tail]);
            put_u32(buf, tail, sum);
        }
    }
    for s in 0..ext2.sectors_per_block {
        let disk = unsafe { core::slice::from_raw_parts_mut(crate::DISK_IO_BUF as *mut u8, 512) };
        disk.copy_from_slice(&buf[(s * 512) as usize..(s * 512) as usize + 512]);
        ext2.write_sector_abs(ext2.block_to_lba(block) + s).map_err(|_| ERR_IO)?;
    }
    Ok(())
}

/// The first logical block an entry covers: an extent's or an index's.
fn first_of(e: &[u8]) -> u32 {
    read_u32(e, 0)
}

fn index_entry(first: u32, child: u32) -> [u8; EXTENT_ENTRY_SIZE] {
    let mut e = [0u8; EXTENT_ENTRY_SIZE];
    put_u32(&mut e, 0, first);
    put_u32(&mut e, 4, child);
    e
}

fn leaf_entry(logical: u32, phys: u32) -> [u8; EXTENT_ENTRY_SIZE] {
    let mut e = [0u8; EXTENT_ENTRY_SIZE];
    put_u32(&mut e, 0, logical);
    put_u16(&mut e, 4, 1);
    put_u32(&mut e, 8, phys);
    e
}

/// Put `e` in `buf`'s entries in logical order, if there is room: whether
/// there was. A block that continues a leaf's extent, logically and on the
/// disk, makes it longer instead.
fn place(buf: &mut [u8], cap: u16, leaf: bool, e: &[u8; EXTENT_ENTRY_SIZE]) -> bool {
    let entries = read_u16(buf, 2);
    let logical = first_of(e);
    if leaf {
        // The extent before where it goes, made one longer.
        let mut before = None;
        for i in 0..entries as usize {
            if first_of(&buf[EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE..]) <= logical {
                before = Some(i);
            }
        }
        if let Some(i) = before {
            let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
            let raw = read_u16(buf, off + 4);
            let start = ((read_u16(buf, off + 6) as u64) << 32) | read_u32(buf, off + 8) as u64;
            let phys = read_u32(e, 8) as u64;
            if raw < INIT_MAX_LEN - 1 && logical == read_u32(buf, off) + raw as u32 && phys == start + raw as u64 {
                put_u16(buf, off + 4, raw + 1);
                return true;
            }
        }
    }
    if entries >= cap {
        return false;
    }
    let mut at = entries as usize;
    while at > 0 && first_of(&buf[EXTENT_HEADER_SIZE + (at - 1) * EXTENT_ENTRY_SIZE..]) > logical {
        at -= 1;
    }
    let from = EXTENT_HEADER_SIZE + at * EXTENT_ENTRY_SIZE;
    let end = EXTENT_HEADER_SIZE + entries as usize * EXTENT_ENTRY_SIZE;
    buf.copy_within(from..end, from + EXTENT_ENTRY_SIZE);
    buf[from..from + EXTENT_ENTRY_SIZE].copy_from_slice(e);
    put_u16(buf, 2, entries + 1);
    true
}

/// A new block for the tree, counted in the inode's blocks.
fn new_block(ext2: &mut Ext2State, inode: &mut Ext2Inode) -> Result<u32, u64> {
    let b = ext2_alloc::alloc_block(ext2)?;
    inode.i_blocks += ext2.block_size / 512;
    Ok(b)
}

/// The root's entries, moved into a block of their own; the root an index
/// of that one block, a level deeper. What makes room in the root.
fn deepen(ext2: &mut Ext2State, ino: u32, inode: &mut Ext2Inode) -> Result<(), u64> {
    let mut root = root_bytes(inode);
    let h = parse_header(&root)?;
    if h.depth >= MAX_DEPTH - 1 {
        return Err(ERR_NOT_FOUND);
    }
    let child = new_block(ext2, inode)?;
    let buf = node(MAX_DEPTH as usize);
    buf.fill(0);
    header_bytes(buf, h.entries, block_capacity(ext2), h.depth);
    let n = h.entries as usize * EXTENT_ENTRY_SIZE;
    buf[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + n].copy_from_slice(&root[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + n]);
    let first = if h.entries > 0 { first_of(&root[EXTENT_HEADER_SIZE..]) } else { 0 };
    write_node(ext2, ino, inode, child, buf)?;
    root[EXTENT_HEADER_SIZE..].fill(0);
    header_bytes(&mut root, 1, ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16, h.depth + 1);
    root[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + EXTENT_ENTRY_SIZE].copy_from_slice(&index_entry(first, child));
    store_root(inode, &root);
    Ok(())
}

/// Record that logical block `logical` of inode `ino` is at `phys`, in a tree
/// as deep as it has to be. What `extent_insert` does for a tree that is all
/// root, for any tree.
pub fn tree_insert(ext2: &mut Ext2State, ino: u32, inode: &mut Ext2Inode, logical: u32, phys: u32) -> Result<(), u64> {
    // The common case, and the only one a small file meets: the root.
    if parse_header(&root_bytes(inode))?.depth == 0 {
        match extent_insert(inode, logical, phys) {
            Err(ERR_NOT_FOUND) => deepen(ext2, ino, inode)?,
            done => return done,
        }
    }
    let e = leaf_entry(logical, phys);
    // Down from the root, at each level the last entry that does not start
    // past the block — or the first, for a block before them all.
    let root = root_bytes(inode);
    let depth = parse_header(&root)?.depth as usize;
    let mut blocks = [0u32; MAX_DEPTH as usize + 1];
    let mut at = [0usize; MAX_DEPTH as usize + 1];
    node(0)[..60].copy_from_slice(&root);
    for level in 0..depth {
        let buf = node(level);
        let entries = read_u16(buf, 2) as usize;
        if entries == 0 {
            return Err(ERR_IO);
        }
        let mut chosen = 0;
        for i in 0..entries {
            if first_of(&buf[EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE..]) <= logical {
                chosen = i;
            }
        }
        at[level] = chosen;
        let off = EXTENT_HEADER_SIZE + chosen * EXTENT_ENTRY_SIZE;
        let child = read_u32(buf, off + 4);
        if child == 0 {
            return Err(ERR_IO);
        }
        blocks[level + 1] = child;
        read_node(ext2, child, node(level + 1))?;
        if parse_header(&node(level + 1)[..])?.depth as usize != depth - level - 1 {
            return Err(ERR_IO);
        }
    }

    // Into the leaf, if it has room.
    let cap = block_capacity(ext2);
    let leaf = node(depth);
    if place(leaf, cap, true, &e) {
        write_node(ext2, ino, inode, blocks[depth], leaf)?;
        return lower_bounds(ext2, ino, inode, &blocks, &at, depth, logical);
    }

    // A full leaf: split, and the new half's index into its parent, which
    // may split in turn. A block past every extent the leaf has goes into a
    // leaf of its own, which is what a file written from start to end
    // wants; anything else, half the leaf moves.
    let mut carry = e;
    let mut is_leaf = true;
    let mut level = depth;
    loop {
        let buf = node(level);
        let entries = read_u16(buf, 2) as usize;
        let sibling = new_block(ext2, inode)?;
        let side = node(MAX_DEPTH as usize);
        side.fill(0);
        let node_depth = read_u16(buf, 6);
        header_bytes(side, 0, cap, node_depth);
        let last = first_of(&buf[EXTENT_HEADER_SIZE + (entries - 1) * EXTENT_ENTRY_SIZE..]);
        if first_of(&carry) > last {
            place(side, cap, is_leaf, &carry);
        } else {
            let half = entries / 2;
            let from = EXTENT_HEADER_SIZE + half * EXTENT_ENTRY_SIZE;
            let to = EXTENT_HEADER_SIZE + entries * EXTENT_ENTRY_SIZE;
            side[EXTENT_HEADER_SIZE..EXTENT_HEADER_SIZE + (to - from)].copy_from_slice(&buf[from..to]);
            put_u16(side, 2, (entries - half) as u16);
            buf[from..to].fill(0);
            put_u16(buf, 2, half as u16);
            let goes_right = first_of(&carry) >= first_of(&side[EXTENT_HEADER_SIZE..]);
            if goes_right {
                place(side, cap, is_leaf, &carry);
            } else {
                place(buf, cap, is_leaf, &carry);
            }
        }
        write_node(ext2, ino, inode, blocks[level], buf)?;
        let side_first = first_of(&side[EXTENT_HEADER_SIZE..]);
        write_node(ext2, ino, inode, sibling, side)?;
        carry = index_entry(side_first, sibling);
        is_leaf = false;
        level -= 1;
        if level == 0 {
            // Into the root, or the root a level deeper first.
            let mut root = root_bytes(inode);
            if !place(&mut root, ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16, false, &carry) {
                deepen(ext2, ino, inode)?;
                // The root's one entry is now the block its entries went to,
                // which has room for this.
                let moved = read_u32(&root_bytes(inode), EXTENT_HEADER_SIZE + 4);
                let buf = node(1);
                read_node(ext2, moved, buf)?;
                place(buf, cap, false, &carry);
                write_node(ext2, ino, inode, moved, buf)?;
                return Ok(());
            }
            store_root(inode, &root);
            return Ok(());
        }
        let parent = node(level);
        if place(parent, cap, false, &carry) {
            write_node(ext2, ino, inode, blocks[level], parent)?;
            return Ok(());
        }
    }
}

/// A block put before every other in its leaf: the index entries above it
/// that start after it start at it.
fn lower_bounds(
    ext2: &Ext2State,
    ino: u32,
    inode: &mut Ext2Inode,
    blocks: &[u32],
    at: &[usize],
    depth: usize,
    logical: u32,
) -> Result<(), u64> {
    for level in (0..depth).rev() {
        let buf = node(level);
        let off = EXTENT_HEADER_SIZE + at[level] * EXTENT_ENTRY_SIZE;
        if read_u32(buf, off) <= logical {
            break;
        }
        put_u32(buf, off, logical);
        if level == 0 {
            let mut root = [0u8; 60];
            root.copy_from_slice(&buf[..60]);
            store_root(inode, &root);
        } else {
            write_node(ext2, ino, inode, blocks[level], buf)?;
        }
    }
    Ok(())
}

/// Free the blocks of inode `ino` from logical block `first` on, in a tree
/// of any depth: each leaf cut short, each subtree past the cut freed
/// whole, each block left with nothing in it freed, and a root left with
/// nothing is a leaf again. How many blocks went, the tree's included.
pub fn truncate_tree(ext2: &mut Ext2State, ino: u32, inode: &mut Ext2Inode, first: u32) -> Result<u32, u64> {
    if parse_header(&root_bytes(inode))?.depth == 0 {
        return truncate_root(ext2, inode, first);
    }
    let mut root = root_bytes(inode);
    let (left, freed) = trim(ext2, ino, inode, &mut root[..], true, first, 0)?;
    if left == 0 {
        init_extent_root(inode);
    } else {
        store_root(inode, &root);
    }
    Ok(freed)
}

/// Cut the node in `buf` (the root's sixty bytes, or a block's) at `first`:
/// how many entries it has left, and how many blocks went.
fn trim(ext2: &mut Ext2State, ino: u32, inode: &mut Ext2Inode, buf: &mut [u8], in_root: bool, first: u32, guard: u16) -> Result<(u16, u32), u64> {
    if guard > MAX_DEPTH {
        return Err(ERR_IO);
    }
    let h = parse_header(buf)?;
    let cap = if in_root { ((60 - EXTENT_HEADER_SIZE) / EXTENT_ENTRY_SIZE) as u16 } else { block_capacity(ext2) };
    if h.entries > cap {
        return Err(ERR_IO);
    }
    let mut kept = 0usize;
    let mut freed = 0u32;
    // Of an index, only the last entry that starts before the cut can hold
    // anything past it: those before it are kept as they are, unread.
    let cut_in = (0..h.entries as usize)
        .filter(|&i| first_of(&buf[EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE..]) < first)
        .last();
    for i in 0..h.entries as usize {
        let off = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        let mut e = [0u8; EXTENT_ENTRY_SIZE];
        e.copy_from_slice(&buf[off..off + EXTENT_ENTRY_SIZE]);
        let starts = first_of(&e);
        if h.depth != 0 && Some(i) != cut_in && starts < first {
            // Wholly before the cut.
        } else if h.depth == 0 {
            let (block, len, start, uninit) = leaf_run(&e)?;
            let keep = if block >= first { 0 } else { (first - block).min(len) };
            for b in keep..len {
                ext2_alloc::free_block(ext2, start + b)?;
            }
            freed += len - keep;
            if keep == 0 {
                continue;
            }
            let raw = if uninit { keep as u16 + INIT_MAX_LEN } else { keep as u16 };
            put_u16(&mut e, 4, raw);
        } else if starts >= first {
            freed += free_entry(ext2, &e, h.depth, guard)?;
            continue;
        } else {
            // The entry the cut falls in, or one wholly before it: its
            // subtree is cut, and goes if nothing is left of it.
            let child = read_u32(&e, 4);
            let mut block = [0u8; 4096];
            read_node(ext2, child, &mut block)?;
            let (left, gone) = trim(ext2, ino, inode, &mut block[..ext2.block_size as usize], false, first, guard + 1)?;
            freed += gone;
            if left == 0 {
                ext2_alloc::free_block(ext2, child)?;
                freed += 1;
                continue;
            }
            if gone > 0 {
                write_node(ext2, ino, inode, child, &mut block)?;
            }
        }
        let to = EXTENT_HEADER_SIZE + kept * EXTENT_ENTRY_SIZE;
        buf[to..to + EXTENT_ENTRY_SIZE].copy_from_slice(&e);
        kept += 1;
    }
    for i in kept..h.entries as usize {
        let to = EXTENT_HEADER_SIZE + i * EXTENT_ENTRY_SIZE;
        buf[to..to + EXTENT_ENTRY_SIZE].fill(0);
    }
    put_u16(buf, 2, kept as u16);
    Ok((kept as u16, freed))
}
