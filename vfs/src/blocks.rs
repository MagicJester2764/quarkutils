//! A table that grows without moving what is in it.
//!
//! The server's tables were arrays sized for a machine of sixty-four tasks:
//! 512 handles for every program there was, sixty-four working directories,
//! 192 mapped files. A desktop's programs hold thousands of files open
//! between them. A table here grows a block at a time, as it is wanted, to a
//! ceiling its owner gives; and a record, once made, never moves: a request
//! holds one handle while it opens another, and a table that copied itself
//! somewhere bigger would leave that reference pointing at what it left.
//!
//! Growing asks the heap and takes no for an answer: a table with no room
//! for another record says so, and the request is refused, where the heap's
//! own collections would stop the server.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Layout;

/// Records in a block.
pub const BLOCK: usize = 256;

pub struct Blocks<T> {
    blocks: Vec<Box<[T; BLOCK]>>,
    most: usize,
}

impl<T> Blocks<T> {
    /// No room yet, and never more than `most` records.
    pub const fn new(most: usize) -> Self {
        Blocks { blocks: Vec::new(), most }
    }

    /// How many records there is room for now.
    pub fn len(&self) -> usize {
        (self.blocks.len() * BLOCK).min(self.most)
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len() {
            return None;
        }
        self.blocks.get(i / BLOCK).map(|b| &b[i % BLOCK])
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        if i >= self.len() {
            return None;
        }
        self.blocks.get_mut(i / BLOCK).map(|b| &mut b[i % BLOCK])
    }

    /// Room made for record `i`, each record a new block brings made by
    /// `fill`. False past the ceiling, or with no memory for it.
    pub fn grow_to(&mut self, i: usize, fill: impl Fn() -> T) -> bool {
        if i >= self.most {
            return false;
        }
        while self.blocks.len() * BLOCK <= i {
            if self.blocks.try_reserve(1).is_err() {
                return false;
            }
            let Some(block) = block(&fill) else {
                return false;
            };
            self.blocks.push(block);
        }
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        let len = self.len();
        self.blocks.iter().flat_map(|b| b.iter()).take(len)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> + '_ {
        let len = self.len();
        self.blocks.iter_mut().flat_map(|b| b.iter_mut()).take(len)
    }
}

/// A block of records made by `fill` where it is to stay — a block of
/// handles is twenty kilobytes, which is not for a stack — or `None` with no
/// memory for it.
fn block<T>(fill: &impl Fn() -> T) -> Option<Box<[T; BLOCK]>> {
    let layout = Layout::new::<[T; BLOCK]>();
    unsafe {
        let at = alloc::alloc::alloc(layout) as *mut T;
        if at.is_null() {
            return None;
        }
        for i in 0..BLOCK {
            at.add(i).write(fill());
        }
        Some(Box::from_raw(at as *mut [T; BLOCK]))
    }
}
