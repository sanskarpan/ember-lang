use crate::handle::{Gc, GcBox, ObjHeader};
use crate::trace::{Trace, Tracer};
use rustc_hash::FxHashMap;
use std::alloc::{dealloc, Layout};
use std::ptr::NonNull;

/// Applied to `next_gc` after every collection.
const GROWTH_FACTOR: usize = 2;
/// The heap doesn't attempt its first collection until at least this many
/// bytes are live — otherwise a handful of early allocations would
/// trigger a pointless collection of an almost-empty heap. Also the floor
/// `next_gc` never drops below after a collection frees everything (see
/// `collect`'s last line, added in a later task): without a floor,
/// `next_gc` could settle at 0 and `should_collect` would return true
/// forever after, defeating the point of a threshold.
const INITIAL_NEXT_GC: usize = 1024;

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub collections: usize,
    pub bytes_freed: usize,
    pub live_objects: usize,
}

pub struct GcHeap {
    objects: Option<NonNull<ObjHeader>>,
    bytes_allocated: usize,
    next_gc: usize,
    strings: FxHashMap<String, NonNull<GcBox<String>>>,
    stats: GcStats,
    stress: bool,
}

impl Default for GcHeap {
    fn default() -> Self {
        GcHeap {
            objects: None,
            bytes_allocated: 0,
            next_gc: INITIAL_NEXT_GC,
            strings: FxHashMap::default(),
            stats: GcStats::default(),
            stress: cfg!(feature = "gc-stress"),
        }
    }
}

unsafe fn trace_shim<T: Trace>(header: NonNull<ObjHeader>, tracer: &mut Tracer) {
    let gcbox = header.cast::<GcBox<T>>();
    (*gcbox.as_ptr()).data.trace(tracer);
}

unsafe fn drop_shim<T>(header: NonNull<ObjHeader>, _heap: &mut GcHeap) {
    let gcbox_ptr = header.cast::<GcBox<T>>();
    std::ptr::drop_in_place(gcbox_ptr.as_ptr());
    dealloc(gcbox_ptr.as_ptr() as *mut u8, Layout::new::<GcBox<T>>());
}

unsafe fn drop_interned_string_shim(header: NonNull<ObjHeader>, heap: &mut GcHeap) {
    let gcbox_ptr = header.cast::<GcBox<String>>();
    let content = (*gcbox_ptr.as_ptr()).data.clone();
    heap.strings.remove(&content);
    std::ptr::drop_in_place(gcbox_ptr.as_ptr());
    dealloc(
        gcbox_ptr.as_ptr() as *mut u8,
        Layout::new::<GcBox<String>>(),
    );
}

impl GcHeap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bytes_allocated(&self) -> usize {
        self.bytes_allocated
    }

    pub fn stats(&self) -> GcStats {
        self.stats
    }

    /// True once enough has been allocated (or, under the `gc-stress`
    /// feature, always) that the next instruction boundary should run a
    /// collection. `ember-vm` checks this once per `Vm::step()` call, not
    /// inside `allocate`/`intern_str` — checking at instruction boundaries
    /// only (not per-allocation) is what makes it safe for a single
    /// instruction handler to do more than one allocation in a row without
    /// any of them needing to be a fully-rooted temporary in between.
    pub fn should_collect(&self) -> bool {
        if self.stress {
            return true;
        }
        self.bytes_allocated > self.next_gc
    }

    /// Overrides the compile-time `gc-stress` feature at runtime: when `on`
    /// is true, `should_collect` always returns true regardless of how the
    /// crate was compiled; when false, it falls back to normal
    /// threshold-based collection.
    pub fn set_stress(&mut self, on: bool) {
        self.stress = on;
    }

    pub fn allocate<T: Trace>(&mut self, data: T) -> Gc<T> {
        let layout = Layout::new::<GcBox<T>>();
        let header = ObjHeader {
            marked: false,
            next: self.objects,
            size: layout.size(),
            trace_fn: trace_shim::<T>,
            drop_fn: drop_shim::<T>,
        };
        let boxed = Box::new(GcBox { header, data });
        let raw = Box::into_raw(boxed);
        let ptr = unsafe { NonNull::new_unchecked(raw) };
        self.objects = Some(ptr.cast());
        self.bytes_allocated += layout.size();
        self.stats.live_objects += 1;
        #[cfg(feature = "gc-log")]
        eprintln!(
            "[gc] allocate {} bytes at {:p}",
            layout.size(),
            ptr.as_ptr()
        );
        Gc::from_box_ptr(ptr)
    }

    /// Interns `s`: returns the existing handle if this exact content is
    /// already live on the heap, otherwise allocates a fresh one. The
    /// table entry is a bare, non-owning pointer (not a `Gc<String>`) and
    /// is never scanned as a root by `collect` — an interned string with
    /// no other references is collected exactly like anything else, and
    /// this allocation path uses a dedicated drop function
    /// (`drop_interned_string_shim`, not the generic `drop_shim::<String>`
    /// that a bare `allocate(String)` would use) so the table entry is
    /// pruned in the very same sweep pass that frees the string, rather
    /// than left dangling.
    pub fn intern_str(&mut self, s: &str) -> Gc<String> {
        if let Some(&ptr) = self.strings.get(s) {
            return Gc::from_box_ptr(ptr);
        }
        let owned = s.to_string();
        let layout = Layout::new::<GcBox<String>>();
        let header = ObjHeader {
            marked: false,
            next: self.objects,
            size: layout.size(),
            trace_fn: trace_shim::<String>,
            drop_fn: drop_interned_string_shim,
        };
        let boxed = Box::new(GcBox {
            header,
            data: owned.clone(),
        });
        let raw = Box::into_raw(boxed);
        let ptr = unsafe { NonNull::new_unchecked(raw) };
        self.objects = Some(ptr.cast());
        self.bytes_allocated += layout.size();
        self.stats.live_objects += 1;
        self.strings.insert(owned, ptr);
        #[cfg(feature = "gc-log")]
        eprintln!("[gc] intern {s:?} at {:p}", ptr.as_ptr());
        Gc::from_box_ptr(ptr)
    }

    /// Runs one full mark-and-sweep collection. `mark_roots` is called
    /// once with a `Tracer` the caller uses to mark every root it knows
    /// about — this heap has no notion of "a VM" at all, which is exactly
    /// why this is a callback instead of a hardcoded scan: ember-gc must
    /// not depend on ember-vm.
    pub fn collect(&mut self, mark_roots: impl FnOnce(&mut Tracer)) {
        let mut gray_stack: Vec<NonNull<ObjHeader>> = Vec::new();
        {
            let mut tracer = Tracer {
                gray_stack: &mut gray_stack,
            };
            mark_roots(&mut tracer);
        }
        while let Some(header_ptr) = gray_stack.pop() {
            let mut tracer = Tracer {
                gray_stack: &mut gray_stack,
            };
            unsafe {
                let trace_fn = (*header_ptr.as_ptr()).trace_fn;
                trace_fn(header_ptr, &mut tracer);
            }
            #[cfg(feature = "gc-log")]
            eprintln!("[gc] blacken {:p}", header_ptr.as_ptr());
        }

        let mut survivors: Option<NonNull<ObjHeader>> = None;
        let mut cursor = self.objects;
        let mut freed_bytes = 0usize;
        let mut freed_count = 0usize;
        while let Some(node) = cursor {
            let (next, marked, size) = unsafe {
                let h = node.as_ptr();
                ((*h).next, (*h).marked, (*h).size)
            };
            if marked {
                unsafe {
                    (*node.as_ptr()).marked = false;
                    (*node.as_ptr()).next = survivors;
                }
                survivors = Some(node);
            } else {
                unsafe {
                    let drop_fn = (*node.as_ptr()).drop_fn;
                    drop_fn(node, self);
                }
                freed_bytes += size;
                freed_count += 1;
                #[cfg(feature = "gc-log")]
                eprintln!("[gc] free {size} bytes at {:p}", node.as_ptr());
            }
            cursor = next;
        }
        self.objects = survivors;
        self.bytes_allocated -= freed_bytes;
        self.stats.bytes_freed += freed_bytes;
        self.stats.live_objects -= freed_count;
        self.stats.collections += 1;
        self.next_gc = self.bytes_allocated * GROWTH_FACTOR + INITIAL_NEXT_GC;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Trace;
    use std::cell::RefCell;

    struct Leaf(i32);
    impl Trace for Leaf {
        fn trace(&self, _tracer: &mut Tracer) {}
    }

    struct Node(RefCell<Option<Gc<Node>>>);
    impl Trace for Node {
        fn trace(&self, tracer: &mut Tracer) {
            if let Some(next) = *self.0.borrow() {
                tracer.mark(next);
            }
        }
    }

    #[test]
    fn allocate_returns_a_handle_that_derefs_to_the_data() {
        let mut heap = GcHeap::new();
        let handle = heap.allocate(Leaf(42));
        assert_eq!(handle.0, 42);
    }

    #[test]
    fn allocating_tracks_bytes_allocated() {
        let mut heap = GcHeap::new();
        assert_eq!(heap.bytes_allocated(), 0);
        heap.allocate(Leaf(1));
        assert!(heap.bytes_allocated() > 0);
        let after_one = heap.bytes_allocated();
        heap.allocate(Leaf(2));
        assert_eq!(heap.bytes_allocated(), after_one * 2);
    }

    #[test]
    fn allocating_increments_live_object_count() {
        let mut heap = GcHeap::new();
        heap.allocate(Leaf(1));
        heap.allocate(Leaf(2));
        assert_eq!(heap.stats().live_objects, 2);
    }

    #[test]
    fn unreachable_object_is_collected() {
        let mut heap = GcHeap::new();
        heap.allocate(Leaf(1));
        assert_eq!(heap.stats().live_objects, 1);
        heap.collect(|_tracer| { /* nothing rooted */ });
        assert_eq!(heap.stats().live_objects, 0);
        assert_eq!(heap.bytes_allocated(), 0);
    }

    #[test]
    fn reachable_object_survives_a_collection() {
        let mut heap = GcHeap::new();
        let handle = heap.allocate(Leaf(7));
        heap.collect(|tracer| tracer.mark(handle));
        assert_eq!(heap.stats().live_objects, 1);
        assert_eq!(handle.0, 7, "the surviving object's data must be untouched");
    }

    #[test]
    fn only_the_rooted_object_survives_when_others_are_not_rooted() {
        let mut heap = GcHeap::new();
        let kept = heap.allocate(Leaf(1));
        heap.allocate(Leaf(2)); // never rooted
        heap.collect(|tracer| tracer.mark(kept));
        assert_eq!(heap.stats().live_objects, 1);
        assert_eq!(kept.0, 1);
    }

    #[test]
    fn a_cycle_is_collected_once_nothing_roots_either_side() {
        let mut heap = GcHeap::new();
        let a = heap.allocate(Node(RefCell::new(None)));
        let b = heap.allocate(Node(RefCell::new(None)));
        *a.0.borrow_mut() = Some(b);
        *b.0.borrow_mut() = Some(a); // a -> b -> a, a real cycle
        assert_eq!(heap.stats().live_objects, 2);
        heap.collect(|_tracer| { /* nothing rooted — Rc could never free this */ });
        assert_eq!(
            heap.stats().live_objects,
            0,
            "an unrooted cycle must be fully collected, not leaked"
        );
    }

    #[test]
    fn a_reachable_object_survives_100_collections() {
        let mut heap = GcHeap::new();
        let kept = heap.allocate(Leaf(99));
        for _ in 0..100 {
            heap.collect(|tracer| tracer.mark(kept));
        }
        assert_eq!(heap.stats().live_objects, 1);
        assert_eq!(kept.0, 99);
    }

    #[test]
    #[cfg(not(feature = "gc-stress"))]
    fn should_collect_is_true_once_past_the_initial_threshold() {
        let mut heap = GcHeap::new();
        assert!(
            !heap.should_collect(),
            "an empty heap has nothing to collect yet"
        );
        for i in 0..2000 {
            heap.allocate(Leaf(i));
        }
        assert!(heap.should_collect());
    }

    #[test]
    fn interning_the_same_content_twice_returns_the_same_object() {
        let mut heap = GcHeap::new();
        let a = heap.intern_str("hello");
        let b = heap.intern_str("hello");
        assert_eq!(
            heap.stats().live_objects,
            1,
            "must not allocate twice for equal content"
        );
        assert_eq!(*a, "hello".to_string());
        assert_eq!(*b, "hello".to_string());
    }

    #[test]
    fn interning_different_content_allocates_separately() {
        let mut heap = GcHeap::new();
        heap.intern_str("a");
        heap.intern_str("b");
        assert_eq!(heap.stats().live_objects, 2);
    }

    #[test]
    fn an_interned_string_with_no_roots_is_collected_and_reinterning_allocates_fresh() {
        let mut heap = GcHeap::new();
        heap.intern_str("temp");
        assert_eq!(heap.stats().live_objects, 1);
        heap.collect(|_tracer| {}); // nothing rooted — the intern table itself is not a root
        assert_eq!(
            heap.stats().live_objects,
            0,
            "an interned string with no other references must still be collectable"
        );
        heap.intern_str("temp"); // must allocate fresh, not find a dangling entry
        assert_eq!(heap.stats().live_objects, 1);
    }

    #[test]
    fn a_rooted_interned_string_survives_collection() {
        let mut heap = GcHeap::new();
        let s = heap.intern_str("kept");
        heap.collect(|tracer| tracer.mark(s));
        assert_eq!(heap.stats().live_objects, 1);
        assert_eq!(*s, "kept".to_string());
    }

    #[test]
    #[cfg(feature = "gc-stress")]
    fn under_gc_stress_should_collect_is_always_true_even_on_an_empty_heap() {
        let heap = GcHeap::new();
        assert!(heap.should_collect());
    }

    #[test]
    fn set_stress_forces_collection_regardless_of_the_cargo_feature() {
        let mut heap = GcHeap::new();
        heap.set_stress(true);
        assert!(heap.should_collect());
    }

    #[test]
    fn set_stress_false_restores_normal_threshold_based_collection() {
        let mut heap = GcHeap::new();
        heap.set_stress(false);
        assert!(
            !heap.should_collect(),
            "a fresh empty heap should not want to collect yet"
        );
    }
}
