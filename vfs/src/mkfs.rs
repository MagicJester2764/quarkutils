//! A filesystem made in memory, for a server whose volume is memory (`vfs
//! mem`, `disk::in_memory`): ext2, with nothing on it but its root.
//!
//! What is made is what `mke2fs -t ext2 -b 4096 -I 256` makes of the same
//! size, less what nothing here wants: no `lost+found` — nobody checks a
//! filesystem that goes when the machine does — and no blocks kept back for
//! root. Blocks are four kilobytes, so that each is a page and a freed one is
//! a page given back (`disk::discard`). The memory starts out as nothing but
//! a reservation, and nothing here touches what is nought anyway: an empty
//! inode table costs no memory until it has inodes in it.

use crate::ext4::{INCOMPAT_FILETYPE, RO_COMPAT_LARGE_FILE, RO_COMPAT_SPARSE_SUPER};

const BLOCK: usize = 4096;
const INODE_SIZE: usize = 256;
/// As many blocks as one block of bitmap has bits for.
const BLOCKS_PER_GROUP: usize = BLOCK * 8;
/// One inode for every sixteen kilobytes, as `mke2fs` gives a small
/// filesystem.
const BYTES_PER_INODE: usize = 16384;
/// What the file server mounts at most.
const MAX_GROUPS: usize = 128;
const ROOT_INO: usize = 2;
/// Inodes 1 to 10 are the filesystem's own.
const FIRST_INO: usize = 11;
const DESC_SIZE: usize = 32;
/// The extra room a 256-byte inode keeps past the first 128, as `mke2fs`
/// says it.
const EXTRA_ISIZE: u16 = 32;

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Whether group `g` keeps a copy of the superblock and the descriptors:
/// the first two, and the powers of three, five and seven.
fn has_super(g: usize) -> bool {
    let power_of = |mut n: usize, base: usize| {
        while n > 1 && n % base == 0 {
            n /= base;
        }
        n == 1
    };
    g <= 1 || power_of(g, 3) || power_of(g, 5) || power_of(g, 7)
}

/// Where group `g`'s bitmaps and inode table are, by block.
struct Layout {
    start: usize,
    blocks: usize,
    block_bitmap: usize,
    inode_bitmap: usize,
    inode_table: usize,
    /// The first block past the group's own.
    first_free: usize,
}

/// Make the filesystem in `disk`, which is all noughts. Its root is root's,
/// with `mode` (a sticky directory anybody may write in, for `/tmp`), and
/// `now` is when it was made.
pub fn format(disk: &mut [u8], mode: u16, now: u32, uuid: [u8; 16]) -> Result<(), &'static str> {
    let blocks = disk.len() / BLOCK;
    let groups = blocks.div_ceil(BLOCKS_PER_GROUP);
    if blocks < 64 {
        return Err("smaller than a filesystem can be");
    }
    if groups > MAX_GROUPS || blocks > u32::MAX as usize {
        return Err("larger than a filesystem here can be");
    }
    let per_block = BLOCK / INODE_SIZE;
    let wanted = (disk.len() / BYTES_PER_INODE).max(FIRST_INO + 1);
    let inodes_per_group = (wanted.div_ceil(groups).div_ceil(per_block) * per_block).min(BLOCK * 8);
    let table_blocks = inodes_per_group / per_block;

    let layout = |g: usize| {
        let start = g * BLOCKS_PER_GROUP;
        let blocks = BLOCKS_PER_GROUP.min(blocks - start);
        // A copy of the superblock, then one of the descriptors: a block
        // each, since a hundred and twenty-eight descriptors fill one.
        let block_bitmap = start + if has_super(g) { 2 } else { 0 };
        Layout {
            start,
            blocks,
            block_bitmap,
            inode_bitmap: block_bitmap + 1,
            inode_table: block_bitmap + 2,
            first_free: block_bitmap + 2 + table_blocks,
        }
    };
    // The last group has to hold its own; one too small to is left off.
    let last = layout(groups - 1);
    if last.first_free >= last.start + last.blocks {
        return Err("a size that leaves a group too small for itself");
    }

    // The root directory's block: the first group's first free one.
    let root_block = layout(0).first_free;
    let mut free_blocks = 0usize;
    let mut descriptors = [0u8; MAX_GROUPS * DESC_SIZE];
    for g in 0..groups {
        let l = layout(g);
        let mut used = l.first_free - l.start;
        if g == 0 {
            used += 1;
        }
        let used_inodes = if g == 0 { FIRST_INO - 1 } else { 0 };
        free_blocks += l.blocks - used;

        // The bitmaps: what is used, and the bits past the group's end set,
        // as mke2fs leaves them.
        let bitmap = &mut disk[l.block_bitmap * BLOCK..(l.block_bitmap + 1) * BLOCK];
        for bit in (0..used).chain(l.blocks..BLOCK * 8) {
            bitmap[bit / 8] |= 1 << (bit % 8);
        }
        let bitmap = &mut disk[l.inode_bitmap * BLOCK..(l.inode_bitmap + 1) * BLOCK];
        for bit in (0..used_inodes).chain(inodes_per_group..BLOCK * 8) {
            bitmap[bit / 8] |= 1 << (bit % 8);
        }

        let d = &mut descriptors[g * DESC_SIZE..(g + 1) * DESC_SIZE];
        put32(d, 0, l.block_bitmap as u32);
        put32(d, 4, l.inode_bitmap as u32);
        put32(d, 8, l.inode_table as u32);
        put16(d, 12, (l.blocks - used) as u16);
        put16(d, 14, (inodes_per_group - used_inodes) as u16);
        put16(d, 16, if g == 0 { 1 } else { 0 });
    }
    let free_inodes = groups * inodes_per_group - (FIRST_INO - 1);

    // The superblock, and its copies with the descriptors after each.
    let mut sb = [0u8; 1024];
    put32(&mut sb, 0, (groups * inodes_per_group) as u32);
    put32(&mut sb, 4, blocks as u32);
    put32(&mut sb, 12, free_blocks as u32);
    put32(&mut sb, 16, free_inodes as u32);
    put32(&mut sb, 20, 0); // the first data block, for blocks bigger than 1 KiB
    put32(&mut sb, 24, 2); // 1024 << 2
    put32(&mut sb, 28, 2);
    put32(&mut sb, 32, BLOCKS_PER_GROUP as u32);
    put32(&mut sb, 36, BLOCKS_PER_GROUP as u32);
    put32(&mut sb, 40, inodes_per_group as u32);
    put32(&mut sb, 48, now);
    put16(&mut sb, 54, 0xFFFF); // never checked for having been mounted often
    put16(&mut sb, 56, crate::ext2::EXT2_MAGIC);
    put16(&mut sb, 58, 1); // clean
    put16(&mut sb, 60, 1); // carry on after an error
    put32(&mut sb, 64, now);
    put32(&mut sb, 76, 1); // dynamic revision
    put32(&mut sb, 84, FIRST_INO as u32);
    put16(&mut sb, 88, INODE_SIZE as u16);
    put32(&mut sb, 96, INCOMPAT_FILETYPE);
    put32(&mut sb, 100, RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE);
    sb[104..120].copy_from_slice(&uuid);
    sb[120..125].copy_from_slice(b"tmpfs");
    put32(&mut sb, 264, now);
    put16(&mut sb, 348, EXTRA_ISIZE);
    put16(&mut sb, 350, EXTRA_ISIZE);
    for g in (0..groups).filter(|&g| has_super(g)) {
        let at = layout(g).start * BLOCK;
        put16(&mut sb, 90, g as u16);
        // The first superblock is a kilobyte into the volume; a copy is at
        // the start of its group.
        let sb_at = if g == 0 { 1024 } else { at };
        disk[sb_at..sb_at + 1024].copy_from_slice(&sb);
        let table = at + BLOCK;
        disk[table..table + groups * DESC_SIZE].copy_from_slice(&descriptors[..groups * DESC_SIZE]);
    }

    // The root: a directory with `.` and `..` in it, both itself.
    let l = layout(0);
    let inode = &mut disk[l.inode_table * BLOCK + (ROOT_INO - 1) * INODE_SIZE..][..INODE_SIZE];
    put16(inode, 0, 0o040000 | (mode & 0o7777));
    put32(inode, 4, BLOCK as u32);
    put32(inode, 8, now);
    put32(inode, 12, now);
    put32(inode, 16, now);
    put16(inode, 26, 2);
    put32(inode, 28, (BLOCK / 512) as u32);
    put32(inode, 40, root_block as u32);
    put16(inode, 128, EXTRA_ISIZE);
    let dir = &mut disk[root_block * BLOCK..(root_block + 1) * BLOCK];
    put32(dir, 0, ROOT_INO as u32);
    put16(dir, 4, 12);
    dir[6] = 1;
    dir[7] = 2; // a directory
    dir[8] = b'.';
    put32(dir, 12, ROOT_INO as u32);
    put16(dir, 16, (BLOCK - 12) as u16);
    dir[18] = 2;
    dir[19] = 2;
    dir[20..22].copy_from_slice(b"..");
    Ok(())
}
