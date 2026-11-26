//! Memory the other process may write.
//!
//! Every access to the mapped file goes through [`Memory`]. No Rust reference
//! to the shared bytes is ever formed: words are read and written through
//! atomics made from raw pointers, and byte ranges are copied out or in with
//! raw-pointer copies (spec/transport.md, "Conventions").
//!
//! The rings and the status block take `&impl Memory`, so the Loom tests can
//! run them on modeled atomics and the Miri tests on a heap buffer.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Bytes shared with another process.
///
/// Offsets are from the start of the memory. A word access needs an offset
/// that is a multiple of the word's size. An access out of range panics: the
/// callers check every offset that came from the other side first.
pub trait Memory {
    /// Bytes in the memory.
    fn len(&self) -> usize;
    /// Whether the memory is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// An atomic 64-bit load.
    fn load(&self, off: usize, order: Ordering) -> u64;
    /// An atomic 64-bit store.
    fn store(&self, off: usize, value: u64, order: Ordering);
    /// An atomic 32-bit load.
    fn load32(&self, off: usize, order: Ordering) -> u32;
    /// An atomic 32-bit store.
    fn store32(&self, off: usize, value: u32, order: Ordering);
    /// A fence.
    fn fence(&self, order: Ordering);
    /// Copy `out.len()` bytes from `off` into `out`.
    fn read(&self, off: usize, out: &mut [u8]);
    /// Copy `data` to `off`.
    fn write(&self, off: usize, data: &[u8]);
    /// Set `n` bytes from `off` to zero.
    fn zero(&self, off: usize, n: usize);
}

/// Memory at a raw pointer: a mapped file, or a heap buffer in the tests.
pub struct Raw {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: every access is an atomic or a raw copy; nothing holds a reference.
unsafe impl Send for Raw {}
// SAFETY: as above.
unsafe impl Sync for Raw {}

impl Raw {
    /// Memory of `len` bytes at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` is aligned to 8 and valid for reads and writes of `len` bytes for
    /// as long as the `Raw` lives, and nothing in this process holds a Rust
    /// reference to those bytes.
    pub unsafe fn new(ptr: *mut u8, len: usize) -> Raw {
        assert!(
            (ptr as usize).is_multiple_of(8),
            "shared memory is aligned to 8"
        );
        Raw { ptr, len }
    }

    fn at(&self, off: usize, n: usize, align: usize) -> *mut u8 {
        assert!(
            off.checked_add(n).is_some_and(|end| end <= self.len) && off.is_multiple_of(align),
            "shared access out of range: {off}+{n} of {}",
            self.len
        );
        // SAFETY: in range, checked above.
        unsafe { self.ptr.add(off) }
    }
}

impl Memory for Raw {
    fn len(&self) -> usize {
        self.len
    }

    fn load(&self, off: usize, order: Ordering) -> u64 {
        let p = self.at(off, 8, 8).cast::<u64>();
        // SAFETY: aligned, in range, and accessed only atomically or by raw copy.
        unsafe { AtomicU64::from_ptr(p) }.load(order)
    }

    fn store(&self, off: usize, value: u64, order: Ordering) {
        let p = self.at(off, 8, 8).cast::<u64>();
        // SAFETY: as in `load`.
        unsafe { AtomicU64::from_ptr(p) }.store(value, order);
    }

    fn load32(&self, off: usize, order: Ordering) -> u32 {
        let p = self.at(off, 4, 4).cast::<u32>();
        // SAFETY: as in `load`.
        unsafe { AtomicU32::from_ptr(p) }.load(order)
    }

    fn store32(&self, off: usize, value: u32, order: Ordering) {
        let p = self.at(off, 4, 4).cast::<u32>();
        // SAFETY: as in `load`.
        unsafe { AtomicU32::from_ptr(p) }.store(value, order);
    }

    fn fence(&self, order: Ordering) {
        core::sync::atomic::fence(order);
    }

    fn read(&self, off: usize, out: &mut [u8]) {
        let p = self.at(off, out.len(), 1);
        // SAFETY: in range; `out` is private memory that cannot overlap.
        unsafe { core::ptr::copy_nonoverlapping(p, out.as_mut_ptr(), out.len()) };
    }

    fn write(&self, off: usize, data: &[u8]) {
        let p = self.at(off, data.len(), 1);
        // SAFETY: in range; `data` is private memory that cannot overlap.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
    }

    fn zero(&self, off: usize, n: usize) {
        let p = self.at(off, n, 1);
        // SAFETY: in range.
        unsafe { core::ptr::write_bytes(p, 0, n) };
    }
}

/// Zeroed memory on the heap, for tests, fuzzing and the inspector's copies.
pub struct Heap {
    // Owned through a raw pointer, so that no reference to the bytes exists
    // while other threads access them.
    buf: *mut u64,
    words: usize,
    raw: Raw,
}

// SAFETY: as for `Raw`.
unsafe impl Send for Heap {}
// SAFETY: as for `Raw`.
unsafe impl Sync for Heap {}

impl Heap {
    /// `len` zero bytes, rounded up to a multiple of 8.
    pub fn new(len: usize) -> Heap {
        let words = len.div_ceil(8);
        let buf = Box::into_raw(vec![0u64; words].into_boxed_slice()).cast::<u64>();
        // SAFETY: a fresh allocation of `words` u64s, aligned to 8, owned here.
        let raw = unsafe { Raw::new(buf.cast(), words * 8) };
        Heap { buf, words, raw }
    }

    /// A heap copy of `bytes`, rounded up to a multiple of 8.
    pub fn from_bytes(bytes: &[u8]) -> Heap {
        let heap = Heap::new(bytes.len());
        heap.write(0, bytes);
        heap
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // SAFETY: the allocation made in `new`, freed once.
        drop(unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(self.buf, self.words)) });
    }
}

impl Memory for Heap {
    fn len(&self) -> usize {
        self.raw.len()
    }
    fn load(&self, off: usize, order: Ordering) -> u64 {
        self.raw.load(off, order)
    }
    fn store(&self, off: usize, value: u64, order: Ordering) {
        self.raw.store(off, value, order)
    }
    fn load32(&self, off: usize, order: Ordering) -> u32 {
        self.raw.load32(off, order)
    }
    fn store32(&self, off: usize, value: u32, order: Ordering) {
        self.raw.store32(off, value, order)
    }
    fn fence(&self, order: Ordering) {
        self.raw.fence(order)
    }
    fn read(&self, off: usize, out: &mut [u8]) {
        self.raw.read(off, out)
    }
    fn write(&self, off: usize, data: &[u8]) {
        self.raw.write(off, data)
    }
    fn zero(&self, off: usize, n: usize) {
        self.raw.zero(off, n)
    }
}

impl<M: Memory + ?Sized> Memory for &M {
    fn len(&self) -> usize {
        (**self).len()
    }
    fn load(&self, off: usize, order: Ordering) -> u64 {
        (**self).load(off, order)
    }
    fn store(&self, off: usize, value: u64, order: Ordering) {
        (**self).store(off, value, order)
    }
    fn load32(&self, off: usize, order: Ordering) -> u32 {
        (**self).load32(off, order)
    }
    fn store32(&self, off: usize, value: u32, order: Ordering) {
        (**self).store32(off, value, order)
    }
    fn fence(&self, order: Ordering) {
        (**self).fence(order)
    }
    fn read(&self, off: usize, out: &mut [u8]) {
        (**self).read(off, out)
    }
    fn write(&self, off: usize, data: &[u8]) {
        (**self).write(off, data)
    }
    fn zero(&self, off: usize, n: usize) {
        (**self).zero(off, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_bytes_round_trip() {
        let m = Heap::new(64);
        m.store(8, 0x0102_0304_0506_0708, Ordering::Release);
        assert_eq!(m.load(8, Ordering::Acquire), 0x0102_0304_0506_0708);
        let mut b = [0u8; 8];
        m.read(8, &mut b);
        assert_eq!(b, 0x0102_0304_0506_0708u64.to_le_bytes());
        m.store32(20, 7, Ordering::Relaxed);
        assert_eq!(m.load32(20, Ordering::Relaxed), 7);
        m.write(3, b"abc");
        m.zero(4, 1);
        let mut c = [0u8; 3];
        m.read(3, &mut c);
        assert_eq!(&c, b"a\0c");
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn an_access_past_the_end_panics() {
        Heap::new(16).load(16, Ordering::Relaxed);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn a_misaligned_word_panics() {
        Heap::new(16).load(4, Ordering::Relaxed);
    }
}
