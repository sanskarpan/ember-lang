use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct CountingAlloc {
    bytes: AtomicUsize,
    count: AtomicUsize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocStats {
    pub bytes: usize,
    pub count: usize,
}

impl Default for CountingAlloc {
    fn default() -> Self {
        Self::new()
    }
}

impl CountingAlloc {
    pub const fn new() -> Self {
        CountingAlloc {
            bytes: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
        }
    }

    pub fn reset(&self) {
        self.bytes.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> AllocStats {
        AllocStats {
            bytes: self.bytes.load(Ordering::Relaxed),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

// SAFETY: `alloc` and `dealloc` delegate directly to `System`, which is
// itself a valid `GlobalAlloc` implementation. The counting performed here
// is pure side-effect bookkeeping (atomic increments) with no aliasing or
// lifetime implications for the returned/passed pointers.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.bytes.fetch_add(layout.size(), Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_zeroes_both_counters() {
        let alloc = CountingAlloc::new();
        alloc.bytes.store(100, Ordering::Relaxed);
        alloc.count.store(5, Ordering::Relaxed);
        alloc.reset();
        assert_eq!(alloc.snapshot(), AllocStats { bytes: 0, count: 0 });
    }

    #[test]
    fn snapshot_reflects_recorded_activity() {
        let alloc = CountingAlloc::new();
        alloc.bytes.fetch_add(64, Ordering::Relaxed);
        alloc.count.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            alloc.snapshot(),
            AllocStats {
                bytes: 64,
                count: 1
            }
        );
    }
}
