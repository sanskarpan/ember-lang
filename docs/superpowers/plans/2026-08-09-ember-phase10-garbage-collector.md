# Phase 10: Garbage Collector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a real mark-and-sweep, tri-color tracing garbage collector in the (currently stub) `ember-gc` crate, and migrate `ember-vm`'s `Value` from `Rc`/`RefCell` onto it — closing the "swap for `Gc<T>` in Phase 10" deferral `ember-vm`'s Phase 9 design doc made explicit, and giving the language real reference-cycle collection for the first time.

**Architecture:** `ember-gc` is a small, `ember-vm`-agnostic crate: a `Gc<T>` handle (`Copy`, raw-pointer-based, `Deref`-only), a `Trace` trait each heap type implements, and a `GcHeap` that allocates via `allocate<T: Trace>`, collects via a caller-supplied root-marking closure (so it never needs to know what a "VM" is), and interns strings in a non-owning table pruned during sweep. `ember-vm` depends on `ember-gc`, migrates every `Rc<X>`/`Rc<RefCell<X>>` in `Value` to `Gc<X>` (via small orphan-rule-safe local newtypes for the `RefCell`-wrapping cases), and calls a collection check once at the top of every `step()`.

**Tech Stack:** Rust 2021, `rustc-hash` (FxHashMap), raw pointers / `unsafe` (this phase's whole point — a real tracing GC cannot be built in 100% safe Rust, and SPEC.md's own rationale for choosing Rust over a host-GC'd language is "you must write it yourself").

---

## Before you start (context every task needs)

- `ember-gc`'s crate skeleton already exists at `crates/ember-gc/` (`Cargo.toml` with no deps, `src/lib.rs` containing only a doc comment) — Task 1 fills it in, doesn't create it from scratch.
- `ember-vm`'s current `Value`/`Vm`/`vm.rs`/`natives.rs` (Phase 9, `Rc`/`RefCell`-based) is fully built, tested (54 tests), and conformance-passing. This plan's `ember-vm` tasks are a migration, not a rewrite — the *shape* of every opcode handler stays the same; only the handle type inside `Value`'s variants changes.
- **Collection-trigger policy**: a collection is checked for **once, at the very top of `Vm::step()`**, before that instruction pops/allocates anything — never inside `allocate`/`intern_str` themselves. This is load-bearing for correctness under `gc-stress`, not just a style choice: `OP_CLOSURE`'s upvalue-capture loop and `OP_MAKE_ADT`'s two back-to-back `intern_str` calls can each do more than one heap allocation within a single instruction, and a mid-instruction collection would see an already-allocated-but-not-yet-attached object only in a bare Rust local — invisible to `mark_roots`, and freed while still needed. Checking only at instruction boundaries means every op handler's whole body runs between two collection checkpoints with the stack in a fully-rooted, fully-accounted state, so no handler needs any hand-rolled temporary-root protection.
- **What stays `Rc`, not `Gc`**: `ClosureObj.proto: Rc<FunctionProto>` (immutable compile-time program data, never cyclic, outlives nothing it doesn't already own) and `Value::Native(Rc<NativeFn>)` (constructed once in `Vm::new`, `'static` for the VM's life, holds no `Value`s). Both are explained in the design doc; don't move them onto the GC heap.

---

## Part 1 — `ember-gc` crate

### Task 1: Scaffold the crate

**Files:**
- Modify: `crates/ember-gc/Cargo.toml`
- Modify: `crates/ember-gc/src/lib.rs`
- Create: `crates/ember-gc/src/trace.rs`
- Create: `crates/ember-gc/src/handle.rs`
- Create: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Fill in `Cargo.toml`**

```toml
[package]
name = "ember-gc"
version.workspace = true
edition.workspace = true

[dependencies]
rustc-hash = "2"

[features]
gc-stress = []
gc-log = []
```

- [ ] **Step 2: Replace `src/lib.rs`**

```rust
pub mod handle;
pub mod heap;
pub mod trace;

pub use handle::Gc;
pub use heap::{GcHeap, GcStats};
pub use trace::{Trace, Tracer};
```

- [ ] **Step 3: Create empty `src/trace.rs`, `src/handle.rs`, `src/heap.rs`**

Each just `// filled in by a later task` for now.

- [ ] **Step 4: Build**

Run: `cargo build -p ember-gc`
Expected: fails (empty modules don't provide `Gc`/`GcHeap`/etc. the `pub use`s reference) — that's expected here; Task 2 makes it compile. If your toolchain errors on this intermediate state in a way that blocks committing, that's fine — commit anyway, this is scaffolding.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/Cargo.toml crates/ember-gc/src/lib.rs crates/ember-gc/src/trace.rs crates/ember-gc/src/handle.rs crates/ember-gc/src/heap.rs
git commit -m "Scaffold ember-gc crate module layout"
```

---

### Task 2: `ObjHeader`, `GcBox<T>`, `Gc<T>`, `Trace`, `Tracer`

**Files:**
- Modify: `crates/ember-gc/src/trace.rs`
- Modify: `crates/ember-gc/src/handle.rs`

- [ ] **Step 1: Write `src/handle.rs`**

```rust
use crate::heap::GcHeap;
use crate::trace::Tracer;
use std::ptr::NonNull;

/// The bookkeeping every heap allocation carries, regardless of its
/// concrete type — this is what lets the heap's intrusive list and sweep
/// pass walk a mix of `String`s, `ClosureObj`s, etc. through one shared
/// pointer type (`NonNull<ObjHeader>`) without knowing what they are.
/// `trace_fn`/`drop_fn` are captured, monomorphized, at the call site that
/// allocated this object (`GcHeap::allocate::<T>`) — this crate's stand-in
/// for a vtable, without needing `dyn Trait` or ember-gc knowing about any
/// concrete type ember-vm defines.
pub struct ObjHeader {
    pub(crate) marked: bool,
    pub(crate) next: Option<NonNull<ObjHeader>>,
    pub(crate) size: usize,
    pub(crate) trace_fn: unsafe fn(NonNull<ObjHeader>, &mut Tracer),
    pub(crate) drop_fn: unsafe fn(NonNull<ObjHeader>, &mut GcHeap),
}

/// The header is embedded directly before the data in one allocation
/// (`#[repr(C)]` pins the header first), so a type-erased
/// `NonNull<ObjHeader>` can be cast straight to `NonNull<GcBox<T>>` for any
/// `T` — no separate lookup table needed to go from "an allocation the
/// sweep pass is looking at" to "the concrete typed data it holds".
#[repr(C)]
pub(crate) struct GcBox<T> {
    pub(crate) header: ObjHeader,
    pub(crate) data: T,
}

/// A handle to a `T` living on the GC heap. `Copy` and freely aliasable —
/// many `Gc<T>` can point at the same object, which is the whole point
/// (two closures sharing one captured upvalue, a record's field map
/// sharing an interned key) and exactly what `Rc` could never safely do
/// for *cyclic* structures, since nothing here counts references. Freeing
/// happens only during `GcHeap::collect`'s sweep, driven by the mark bit —
/// never by a handle going out of scope, so there's no `Drop` impl here.
///
/// Only `Deref`, deliberately no `DerefMut`: since handles alias freely,
/// handing out a safe `&mut T` here would let two live `Gc<T>` produce
/// overlapping `&mut T`, which is unsound. Every mutable heap object in
/// `ember-vm` is mutated through interior mutability (`RefCell`), exactly
/// as it already was through `Rc<RefCell<T>>` before this migration.
pub struct Gc<T> {
    ptr: NonNull<GcBox<T>>,
}

impl<T> Gc<T> {
    pub(crate) fn from_box_ptr(ptr: NonNull<GcBox<T>>) -> Self {
        Gc { ptr }
    }

    pub(crate) fn header_ptr(&self) -> NonNull<ObjHeader> {
        self.ptr.cast()
    }
}

impl<T> std::ops::Deref for Gc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &self.ptr.as_ref().data }
    }
}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Gc<T> {}

/// Content-based, forwarding through `Deref` to `T`'s own impls — matches
/// `Rc<T>`'s equality/hashing semantics exactly (NOT pointer identity), so
/// swapping `Rc<X>` for `Gc<X>` anywhere `X: PartialEq + Hash` was already
/// relied on (e.g. `FxHashMap<Gc<String>, Value>` record-field lookups)
/// needs no behavior change.
impl<T: PartialEq> PartialEq for Gc<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl<T: Eq> Eq for Gc<T> {}

impl<T: std::hash::Hash> std::hash::Hash for Gc<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

/// Also forwarded, matching `Rc<T>`'s own `Display` behavior exactly —
/// without this, every existing `format!("...{name}...")`-style
/// interpolation in `ember-vm` that used to work on `Rc<String>` would need
/// rewriting to an explicit deref just because the handle type changed,
/// for no actual behavioral reason.
impl<T: std::fmt::Display> std::fmt::Display for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}
```

- [ ] **Step 2: Write `src/trace.rs`**

```rust
use crate::handle::{Gc, ObjHeader};
use std::ptr::NonNull;

/// Implemented by every type that lives on the GC heap. `trace` must call
/// `tracer.mark(...)` for every `Gc<U>` handle this value directly holds —
/// anything it doesn't report here is invisible to the collector and will
/// be freed while still reachable. This is the classic GC bug (SPEC.md
/// calls it out directly): getting a `trace` impl wrong doesn't error at
/// the point of the mistake, it corrupts memory somewhere unrelated,
/// later.
pub trait Trace {
    fn trace(&self, tracer: &mut Tracer);
}

/// Drives the mark phase's gray worklist. Handed to a caller-supplied
/// root-marking closure by `GcHeap::collect`, and threaded through every
/// `Trace::trace` call while the worklist drains.
pub struct Tracer<'a> {
    pub(crate) gray_stack: &'a mut Vec<NonNull<ObjHeader>>,
}

impl<'a> Tracer<'a> {
    /// Marks `gc` reachable. A no-op if it was already marked — that
    /// check-before-push is what makes tracing a *cyclic* structure
    /// terminate instead of looping forever (A traces B, B traces A, A is
    /// already marked, stop).
    pub fn mark<T>(&mut self, gc: Gc<T>) {
        let header_ptr = gc.header_ptr();
        unsafe {
            if !(*header_ptr.as_ptr()).marked {
                (*header_ptr.as_ptr()).marked = true;
                self.gray_stack.push(header_ptr);
            }
        }
    }
}

/// A leaf: strings hold no `Gc<_>` of their own. Provided here (not left
/// for `ember-vm` to implement) because `impl ForeignTrait for
/// ForeignType` is only allowed from the crate that owns the trait —
/// `ember-vm` implementing `Trace` (this crate's trait) for `String` (a
/// std type) would violate Rust's orphan rule; `ember-gc` implementing its
/// own trait for a foreign type it chooses to support has no such
/// restriction.
impl Trace for String {
    fn trace(&self, _tracer: &mut Tracer) {}
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p ember-gc`
Expected: still fails — `heap.rs` is still empty and `lib.rs` re-exports `GcHeap`/`GcStats` from it. Expected at this point; Task 3 fixes it.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-gc/src/handle.rs crates/ember-gc/src/trace.rs
git commit -m "Add Gc<T>, ObjHeader, GcBox<T>, and the Trace trait"
```

---

### Task 3: `GcHeap` and `allocate<T: Trace>`

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Write the failing test first**

```rust
// bottom of src/heap.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Trace;

    struct Leaf(i32);
    impl Trace for Leaf {
        fn trace(&self, _tracer: &mut Tracer) {}
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
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-gc`
Expected: FAIL to compile — `GcHeap`/`Tracer` import path, `GcHeap::new`/`allocate`/`bytes_allocated`/`stats` don't exist yet.

- [ ] **Step 3: Write `GcHeap`/`GcStats`/`allocate` above the test module**

```rust
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
/// `collect`'s last line): without a floor, `next_gc` could settle at 0
/// and `should_collect` would return true forever after, defeating the
/// point of a threshold.
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
}

impl Default for GcHeap {
    fn default() -> Self {
        GcHeap {
            objects: None,
            bytes_allocated: 0,
            next_gc: INITIAL_NEXT_GC,
            strings: FxHashMap::default(),
            stats: GcStats::default(),
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
        eprintln!("[gc] allocate {} bytes at {:p}", layout.size(), ptr.as_ptr());
        Gc::from_box_ptr(ptr)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ember-gc`
Expected: PASS (3/3). `ember-gc` as a whole (`cargo build -p ember-gc`) now compiles clean too — `lib.rs`'s `pub use heap::{GcHeap, GcStats}` finally resolves.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Add GcHeap and allocate<T: Trace>"
```

---

### Task 4: `collect()` — mark, blacken, sweep

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// add inside the existing `mod tests` block in src/heap.rs
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-gc`
Expected: FAIL to compile — `GcHeap::collect` doesn't exist yet.

- [ ] **Step 3: Implement `collect`**

```rust
// add to impl GcHeap in src/heap.rs, after allocate
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ember-gc`
Expected: PASS (6/6).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Implement GcHeap::collect (mark, blacken, sweep)"
```

---

### Task 5: `should_collect`, cyclic-structure collection, survives-100-collections

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// add inside `mod tests`
use std::cell::RefCell;

struct Node(RefCell<Option<Gc<Node>>>);
impl Trace for Node {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(next) = *self.0.borrow() {
            tracer.mark(next);
        }
    }
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
fn should_collect_is_true_once_past_the_initial_threshold() {
    let mut heap = GcHeap::new();
    assert!(!heap.should_collect(), "an empty heap has nothing to collect yet");
    for i in 0..2000 {
        heap.allocate(Leaf(i));
    }
    assert!(heap.should_collect());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-gc`
Expected: FAIL to compile — `should_collect` doesn't exist. The two new value-only tests (cycle, 100-collections) should already pass once it compiles, since `collect` from Task 4 already handles cycles correctly (the mark-before-push check in `Tracer::mark` is what prevents infinite tracing of `a -> b -> a`) — this task's real new code is just `should_collect`.

- [ ] **Step 3: Add `should_collect`**

```rust
// add to impl GcHeap, after stats()
    /// True once enough has been allocated (or, under the `gc-stress`
    /// feature, always) that the next instruction boundary should run a
    /// collection. `ember-vm` checks this once per `Vm::step()` call, not
    /// inside `allocate`/`intern_str` — see this plan's "Before you start"
    /// note for why that boundary matters for soundness, not just style.
    pub fn should_collect(&self) -> bool {
        if cfg!(feature = "gc-stress") {
            return true;
        }
        self.bytes_allocated > self.next_gc
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ember-gc`
Expected: PASS (9/9).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Add should_collect; prove cyclic structures and long-lived roots work"
```

---

### Task 6: String interning

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// add inside `mod tests`
#[test]
fn interning_the_same_content_twice_returns_the_same_object() {
    let mut heap = GcHeap::new();
    let a = heap.intern_str("hello");
    let b = heap.intern_str("hello");
    assert_eq!(heap.stats().live_objects, 1, "must not allocate twice for equal content");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ember-gc`
Expected: FAIL to compile — `intern_str` doesn't exist.

- [ ] **Step 3: Implement `intern_str`**

```rust
// add near drop_shim, before impl GcHeap
unsafe fn drop_interned_string_shim(header: NonNull<ObjHeader>, heap: &mut GcHeap) {
    let gcbox_ptr = header.cast::<GcBox<String>>();
    let content = (*gcbox_ptr.as_ptr()).data.clone();
    heap.strings.remove(&content);
    std::ptr::drop_in_place(gcbox_ptr.as_ptr());
    dealloc(gcbox_ptr.as_ptr() as *mut u8, Layout::new::<GcBox<String>>());
}
```

```rust
// add to impl GcHeap, after allocate
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ember-gc`
Expected: PASS (13/13).

- [ ] **Step 5: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Add string interning with sweep-pruned weak table entries"
```

---

### Task 7: `gc-stress` / `gc-log` feature wiring verification

**Files:**
- Modify: `crates/ember-gc/src/heap.rs`

`gc-log`'s `eprintln!` calls were already added inline in Tasks 3/4/6 behind `#[cfg(feature = "gc-log")]`; `should_collect`'s `cfg!(feature = "gc-stress")` check was added in Task 5. This task just proves both actually work end-to-end via feature-gated tests, since none of the existing tests build with either feature on.

- [ ] **Step 1: Write the failing test**

```rust
// add inside `mod tests`
#[test]
#[cfg(feature = "gc-stress")]
fn under_gc_stress_should_collect_is_always_true_even_on_an_empty_heap() {
    let heap = GcHeap::new();
    assert!(heap.should_collect());
}
```

- [ ] **Step 2: Run test to verify it fails without the feature, passes with it**

Run: `cargo test -p ember-gc`
Expected: PASS, but the new test doesn't run (feature off by default) — confirm via: `cargo test -p ember-gc --features gc-stress`
Expected: PASS including the new test.

- [ ] **Step 3: Manually verify `gc-log` compiles and prints**

Run: `cargo test -p ember-gc --features gc-log -- --nocapture allocate_returns_a_handle`
Expected: test passes; stderr shows a `[gc] allocate ... bytes at 0x...` line.

No new production code needed this task — this step is verification that Tasks 3/4/6's feature-gated code paths actually compile and behave under each feature. If `cargo build -p ember-gc --features gc-stress,gc-log` (both together) doesn't compile cleanly, fix whatever breaks before moving on.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-gc/src/heap.rs
git commit -m "Add a feature-gated test proving gc-stress forces collection"
```

---

## Part 2 — `ember-vm` migration

### Task 8: `value.rs` — migrate `Value` from `Rc` to `Gc`

**Files:**
- Modify: `crates/ember-vm/Cargo.toml`
- Modify: `crates/ember-vm/src/value.rs`

- [ ] **Step 1: Add the `ember-gc` dependency**

```toml
# crates/ember-vm/Cargo.toml — add to [dependencies]
ember-gc = { path = "../ember-gc" }
```

- [ ] **Step 2: Replace `value.rs` in full**

```rust
use ember_bytecode::chunk::FunctionProto;
use ember_gc::{Gc, GcHeap, Trace, Tracer};
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Gc<String>),
    List(Gc<ListObj>),
    Closure(Gc<ClosureObj>),
    /// Stays `Rc`, not `Gc` — natives are constructed once in `Vm::new`,
    /// are effectively `'static` for the VM's lifetime, and hold no
    /// `Value`s of their own, so there is nothing for the GC heap to
    /// manage here.
    Native(Rc<NativeFn>),
    Adt(Gc<AdtValue>),
    Record {
        name: Gc<String>,
        fields: Gc<RecordFields>,
    },
}

/// Local newtype around the list's actual storage. Needed because Rust's
/// orphan rule forbids `ember-vm` implementing `ember-gc`'s `Trace` trait
/// directly for `RefCell<Vec<Value>>` — neither `Trace` nor `RefCell` is
/// defined in this crate. Wrapping in a type this crate *does* define
/// sidesteps the question entirely.
pub struct ListObj(pub RefCell<Vec<Value>>);

impl Trace for ListObj {
    fn trace(&self, tracer: &mut Tracer) {
        for v in self.0.borrow().iter() {
            trace_value(v, tracer);
        }
    }
}

/// Same reasoning as `ListObj` — a local wrapper so `Trace` can be
/// implemented for it.
pub struct RecordFields(pub RefCell<FxHashMap<Gc<String>, Value>>);

impl Trace for RecordFields {
    fn trace(&self, tracer: &mut Tracer) {
        for (k, v) in self.0.borrow().iter() {
            tracer.mark(*k);
            trace_value(v, tracer);
        }
    }
}

pub struct ClosureObj {
    /// Stays `Rc`, not `Gc` — see this file's own header reasoning:
    /// `FunctionProto` is immutable, compile-time-owned program data, part
    /// of the `Chunk` the whole `Vm` run borrows, never cyclic.
    pub proto: Rc<FunctionProto>,
    pub upvalues: Vec<Gc<UpvalueCell>>,
}

impl Trace for ClosureObj {
    fn trace(&self, tracer: &mut Tracer) {
        for uv in &self.upvalues {
            tracer.mark(*uv);
        }
    }
}

impl std::fmt::Debug for ClosureObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<closure arity={} upvalues={}>",
            self.proto.arity,
            self.upvalues.len()
        )
    }
}

/// Local wrapper around `RefCell<Upvalue>`, same orphan-rule reasoning as
/// `ListObj`/`RecordFields`.
pub struct UpvalueCell(pub RefCell<Upvalue>);

impl Trace for UpvalueCell {
    fn trace(&self, tracer: &mut Tracer) {
        if let Upvalue::Closed(v) = &*self.0.borrow() {
            trace_value(v, tracer);
        }
    }
}

#[derive(Debug, Clone)]
pub enum Upvalue {
    /// Still live on the VM stack, at this index.
    Open(usize),
    /// Hoisted to the heap — the stack slot it used to occupy is gone.
    Closed(Value),
}

#[derive(Debug)]
pub struct AdtValue {
    pub type_name: Gc<String>,
    pub variant: Gc<String>,
    pub fields: Vec<Value>,
}

impl Trace for AdtValue {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.mark(self.type_name);
        tracer.mark(self.variant);
        for v in &self.fields {
            trace_value(v, tracer);
        }
    }
}

pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    /// Gained a `&mut GcHeap` parameter this phase — natives that
    /// construct new heap values (`str`) need somewhere to allocate into.
    pub func: fn(&[Value], u32, &mut GcHeap) -> Result<Value, crate::error::RuntimeError>,
}

impl std::fmt::Debug for NativeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<native {}>", self.name)
    }
}

/// Marks every `Gc<_>` handle a `Value` directly holds. Deliberately not
/// `impl Trace for Value` — `Value` itself is never heap-allocated (it
/// lives inline on the VM stack and inside other GC objects' fields);
/// only the things *inside* certain variants are.
pub fn trace_value(v: &Value, tracer: &mut Tracer) {
    match v {
        Value::Str(g) => tracer.mark(*g),
        Value::List(g) => tracer.mark(*g),
        Value::Closure(g) => tracer.mark(*g),
        Value::Adt(g) => tracer.mark(*g),
        Value::Record { name, fields } => {
            tracer.mark(*name);
            tracer.mark(*fields);
        }
        Value::Native(_) | Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {}
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => *a.0.borrow() == *b.0.borrow(),
            (
                Value::Record {
                    name: n1,
                    fields: f1,
                },
                Value::Record {
                    name: n2,
                    fields: f2,
                },
            ) => n1 == n2 && *f1.0.borrow() == *f2.0.borrow(),
            _ => false,
        }
    }
}

/// Structural equality — `List`/`Record` compare by contents, not by
/// handle identity. Mirrors `ember-tree::values_equal` exactly (both
/// backends must agree on what `==` means for the conformance suite to be
/// meaningful).
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Nil, Value::Nil) => true,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => {
            let (x, y) = (x.0.borrow(), y.0.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

/// Deliberately takes no `&Interner` — see this file's own header note
/// from Phase 9, unchanged by this migration.
pub fn display_value(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.to_string(),
        Value::List(l) => {
            let items: Vec<String> = l.0.borrow().iter().map(display_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Closure(_) => "<function>".to_string(),
        Value::Native(n) => format!("<native {}>", n.name),
        Value::Adt(a) => {
            if a.fields.is_empty() {
                a.variant.to_string()
            } else {
                let parts: Vec<String> = a.fields.iter().map(display_value).collect();
                format!("{}({})", a.variant, parts.join(", "))
            }
        }
        Value::Record { name, fields } => {
            let f = fields.0.borrow();
            let parts: Vec<String> = f
                .iter()
                .map(|(k, v)| format!("{k}: {}", display_value(v)))
                .collect();
            format!("{name} {{ {} }}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_equal_compares_structurally_not_by_identity() {
        let mut heap = GcHeap::new();
        let a = Value::List(heap.allocate(ListObj(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
        ]))));
        let b = Value::List(heap.allocate(ListObj(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
        ]))));
        assert!(
            values_equal(&a, &b),
            "two separately-built lists with equal contents must compare equal"
        );
    }

    #[test]
    fn values_equal_rejects_different_types() {
        assert!(!values_equal(&Value::Int(1), &Value::Bool(true)));
        assert!(!values_equal(&Value::Nil, &Value::Int(0)));
    }

    #[test]
    fn display_value_formats_every_variant() {
        let mut heap = GcHeap::new();
        assert_eq!(display_value(&Value::Nil), "nil");
        assert_eq!(display_value(&Value::Bool(true)), "true");
        assert_eq!(display_value(&Value::Int(42)), "42");
        assert_eq!(display_value(&Value::Str(heap.intern_str("hi"))), "hi");
        let list = Value::List(heap.allocate(ListObj(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
        ]))));
        assert_eq!(display_value(&list), "[1, 2]");
    }

    #[test]
    fn display_value_formats_a_record_with_its_fields() {
        let mut heap = GcHeap::new();
        let x_key = heap.intern_str("x");
        let mut fields = FxHashMap::default();
        fields.insert(x_key, Value::Int(1));
        let record = Value::Record {
            name: heap.intern_str("P"),
            fields: heap.allocate(RecordFields(RefCell::new(fields))),
        };
        let out = display_value(&record);
        assert!(out.starts_with("P {"), "{out}");
        assert!(out.contains("x: 1"), "{out}");
    }

    #[test]
    fn display_value_formats_a_nullary_and_a_payload_adt() {
        let mut heap = GcHeap::new();
        let nullary = Value::Adt(heap.allocate(AdtValue {
            type_name: heap.intern_str("Shape"),
            variant: heap.intern_str("Origin"),
            fields: vec![],
        }));
        assert_eq!(display_value(&nullary), "Origin");
        let payload = Value::Adt(heap.allocate(AdtValue {
            type_name: heap.intern_str("Shape"),
            variant: heap.intern_str("Circle"),
            fields: vec![Value::Float(1.5)],
        }));
        assert_eq!(display_value(&payload), "Circle(1.5)");
    }
}
```

Note: `heap.intern_str("Shape")` called twice in `display_value_formats_a_nullary_and_a_payload_adt` returns two *different* handles across the two separate `heap.allocate(AdtValue{...})` calls only because they're the same `heap`, so `intern_str("Shape")` the second time actually returns the *same* handle as the first (interning) — this is fine and expected, not a bug to fix.

- [ ] **Step 3: Run tests**

Run: `cargo test -p ember-vm --lib value::`
Expected: FAIL to compile at first (other modules — `vm.rs`, `natives.rs`, `error.rs` doesn't need this but `lib.rs`'s module graph pulls everything in) — this is expected; the whole crate won't compile until Tasks 9-16 finish the migration. For *this* task, confirm `value.rs`'s own tests are internally consistent by eyeballing them against Task 2/3/6's `ember-gc` API (already tested there) — real compilation confirmation happens once Task 16 finishes.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm/Cargo.toml crates/ember-vm/src/value.rs
git commit -m "Migrate ember-vm::Value from Rc/RefCell to Gc"
```

---

### Task 9: `Vm` gains a `GcHeap`; `mark_roots`/`maybe_collect`; `Vm::new` migrated

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Update `Vm`/`CallFrame`/`Vm::new`**

Replace lines 1-86 (imports through the end of `Vm::new`) with:

```rust
use crate::error::RuntimeError;
use crate::value::{trace_value, ClosureObj, UpvalueCell, Value};
use ember_bytecode::chunk::{Chunk, FunctionProto};
use ember_bytecode::op::Op;
use ember_gc::{Gc, GcHeap};
use rustc_hash::FxHashMap;
use std::rc::Rc;

const MAX_FRAMES: usize = 1000;
const NATIVE_GLOBAL_COUNT: usize = 8;

pub struct CallFrame {
    pub closure: Gc<ClosureObj>,
    pub ip: usize,
    pub slot_base: usize,
}

pub struct Vm {
    stack: Vec<Value>,
    frames: Vec<CallFrame>,
    /// Keyed by an interned `Gc<String>` name now, not `Rc<String>` — see
    /// `read_global_name`. Note the *keys* are marked as roots in
    /// `mark_roots` too, not just the values: a global's name string must
    /// stay alive exactly as long as the global itself does, and nothing
    /// else roots it once it's off the constant pool's own `Rc` and onto
    /// the GC heap via interning.
    globals: FxHashMap<Gc<String>, Value>,
    open_upvalues: Vec<Gc<UpvalueCell>>,
    gc: GcHeap,
}

#[derive(Debug)]
pub enum StepOutcome {
    Running,
    Done(Value),
}

impl Vm {
    pub fn new(script: FunctionProto) -> Self {
        let mut gc = GcHeap::new();
        let proto = Rc::new(script);
        let closure = gc.allocate(ClosureObj {
            proto,
            upvalues: Vec::new(),
        });
        let frame = CallFrame {
            closure,
            ip: 0,
            slot_base: 0,
        };
        let mut stack = Vec::with_capacity(NATIVE_GLOBAL_COUNT);
        let mut globals = FxHashMap::default();
        for &(name, arity, func) in crate::natives::NATIVES {
            let native = Value::Native(Rc::new(crate::value::NativeFn { name, arity, func }));
            stack.push(native.clone());
            let key = gc.intern_str(name);
            globals.insert(key, native);
        }
        Vm {
            stack,
            frames: vec![frame],
            globals,
            open_upvalues: Vec::new(),
            gc,
        }
    }

    #[cfg(test)]
    pub(crate) fn stack_len_for_test(&self) -> usize {
        self.stack.len()
    }

    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, v: Value) {
        self.push(v);
    }

    #[cfg(test)]
    pub(crate) fn gc_mut_for_test(&mut self) -> &mut GcHeap {
        &mut self.gc
    }
```

(The rest of the `impl Vm` block — `frame`, `frame_mut`, `chunk`, `push`, `pop`, `peek` — is unchanged; keep it as-is between `gc_mut_for_test` and `read_u8`.)

- [ ] **Step 2: Add `mark_roots`/`maybe_collect` and wire into `step`**

Add these two methods to `impl Vm`, right after `peek` and before `read_u8`:

```rust
    /// A collection is checked for once, here, at the very top of every
    /// `step()` call — never inside `allocate`/`intern_str` themselves.
    /// See this plan's "Before you start" note: checking only at
    /// instruction boundaries means every opcode handler's whole body
    /// runs with the stack in a fully-rooted state from start to finish,
    /// so no handler needs its own temporary-root protection even when it
    /// does more than one allocation (OP_CLOSURE's upvalue-capture loop,
    /// OP_MAKE_ADT's two interned-name lookups).
    fn maybe_collect(&mut self) {
        if !self.gc.should_collect() {
            return;
        }
        let stack = &self.stack;
        let frames = &self.frames;
        let open_upvalues = &self.open_upvalues;
        let globals = &self.globals;
        self.gc.collect(|tracer| {
            for v in stack {
                trace_value(v, tracer);
            }
            for f in frames {
                tracer.mark(f.closure);
            }
            for uv in open_upvalues {
                tracer.mark(*uv);
            }
            for (k, v) in globals.iter() {
                tracer.mark(*k);
                trace_value(v, tracer);
            }
        });
    }
```

Then change the start of `step`:

```rust
    pub fn step(&mut self) -> Result<StepOutcome, RuntimeError> {
        self.maybe_collect();
        let op = self.read_op();
        match op {
```

(everything else in `step`'s body is migrated in Tasks 10-15; leave the rest of the `match` as-is for now — it won't compile until those tasks land, which is fine, this task's own verification is limited to eyeballing correctness the way Task 8 was.)

- [ ] **Step 3: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Vm gains a GcHeap; add mark_roots/maybe_collect; migrate Vm::new"
```

---

### Task 10: Globals and constants — `read_global_name`/`str_constant`/`const_to_value`/`read_constant`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Replace the four constant/name-reading helpers**

Replace the existing `read_constant`, `read_global_name`, `str_constant`, `const_to_value` methods with:

```rust
    /// Reads a `u16` constant-pool index and converts the pooled
    /// `ember_bytecode::value::Value` into a runtime `Value`. `Str`
    /// interns into the GC heap rather than sharing the pooled `Rc`
    /// directly — the two `Value` types (compile-time constant pool vs.
    /// runtime heap) are no longer the same representation for strings,
    /// unlike Phase 9.
    fn read_constant(&mut self) -> Value {
        let idx = self.read_u16();
        let pooled = self.chunk().constants[idx as usize].clone();
        self.const_to_value(pooled)
    }

    /// Reads a `u16` constant-pool index and expects the pooled value to
    /// be a `Str`, interning it — every global/field/type/variant name
    /// operand in the whole `Op` set is read this way. Panics on a
    /// non-`Str` constant: that would mean `ember-compile` emitted a name
    /// operand pointing at the wrong kind of constant, a compiler bug this
    /// crate has no responsibility to recover from.
    fn read_global_name(&mut self) -> Gc<String> {
        let idx = self.read_u16();
        self.str_constant(idx)
    }

    /// Resolves a *known* constant-pool index to its pooled string,
    /// interned — unlike `read_global_name`, which reads the index off
    /// `ip` itself, this is for opcodes (`MakeAdt`, `TestVariant`) that
    /// already have the index in hand and just need it turned into a
    /// name.
    fn str_constant(&mut self, idx: u16) -> Gc<String> {
        let s = match &self.chunk().constants[idx as usize] {
            ember_bytecode::value::Value::Str(s) => Rc::clone(s),
            other => panic!("name constant must be a string, found {other:?}"),
        };
        self.gc.intern_str(&s)
    }

    fn const_to_value(&mut self, c: ember_bytecode::value::Value) -> Value {
        match c {
            ember_bytecode::value::Value::Nil => Value::Nil,
            ember_bytecode::value::Value::Bool(b) => Value::Bool(b),
            ember_bytecode::value::Value::Int(n) => Value::Int(n),
            ember_bytecode::value::Value::Float(f) => Value::Float(f),
            ember_bytecode::value::Value::Str(s) => Value::Str(self.gc.intern_str(&s)),
        }
    }
```

- [ ] **Step 2: `runtime_error`/`attach_trace` — no signature change needed**

These already only read `f.closure.proto.name`/`.chunk`, both unaffected by the `Rc`→`Gc` swap on `closure` itself (`Gc<ClosureObj>` derefs to `&ClosureObj` exactly like `Rc<ClosureObj>` did). Leave them as-is.

- [ ] **Step 3: `GetGlobal`/`SetGlobal`/`DefineGlobal` — no body change needed**

These already just call `self.read_global_name()` and use the result as an `FxHashMap` key — since `Gc<String>: Hash + Eq` (Task 2) forwards to `String`'s own impls exactly like `Rc<String>` did, `self.globals.get(&name)`/`.contains_key(&name)`/`.insert(name, v)` all keep compiling and behaving identically. Leave the `Op::GetGlobal`/`Op::SetGlobal`/`Op::DefineGlobal` match arms exactly as they are.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate constant-pool reading and global name lookup to interned Gc<String>"
```

---

### Task 11: Upvalues — `capture_upvalue`/`close_upvalues`/`OP_CLOSURE`/`OP_GET_UPVALUE`/`OP_SET_UPVALUE`/`OP_CLOSE_UPVALUE`/`OP_CLOSE_UPVALUES_FROM`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Migrate `Op::Closure`**

Replace:
```rust
            Op::Closure => {
                let idx = self.read_u16();
                let proto = Rc::clone(&self.chunk().functions[idx as usize]);
                let mut upvalues = Vec::with_capacity(proto.upvalues.len());
                for desc in &proto.upvalues {
                    let uv = if desc.is_local {
                        let slot = self.frame().slot_base + desc.index as usize;
                        self.capture_upvalue(slot)
                    } else {
                        Rc::clone(&self.frame().closure.upvalues[desc.index as usize])
                    };
                    upvalues.push(uv);
                }
                self.push(Value::Closure(Rc::new(ClosureObj { proto, upvalues })));
            }
```
with:
```rust
            Op::Closure => {
                let idx = self.read_u16();
                let proto = Rc::clone(&self.chunk().functions[idx as usize]);
                let mut upvalues = Vec::with_capacity(proto.upvalues.len());
                for desc in &proto.upvalues {
                    let uv = if desc.is_local {
                        let slot = self.frame().slot_base + desc.index as usize;
                        self.capture_upvalue(slot)
                    } else {
                        self.frame().closure.upvalues[desc.index as usize]
                    };
                    upvalues.push(uv);
                }
                let closure = self.gc.allocate(ClosureObj { proto, upvalues });
                self.push(Value::Closure(closure));
            }
```
(`Gc<T>: Copy`, so `self.frame().closure.upvalues[idx]` — a `Gc<UpvalueCell>` — no longer needs `Rc::clone`, a plain copy does the same job.)

- [ ] **Step 2: Migrate `Op::GetUpvalue`/`Op::SetUpvalue`/`Op::CloseUpvalue`**

Replace:
```rust
            Op::GetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = Rc::clone(&self.frame().closure.upvalues[idx]);
                let v = match &*uv.borrow() {
                    crate::value::Upvalue::Open(slot) => self.stack[*slot].clone(),
                    crate::value::Upvalue::Closed(v) => v.clone(),
                };
                self.push(v);
            }
            Op::SetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = Rc::clone(&self.frame().closure.upvalues[idx]);
                let v = self.peek(0).clone();
                let open_slot = match &*uv.borrow() {
                    crate::value::Upvalue::Open(slot) => Some(*slot),
                    crate::value::Upvalue::Closed(_) => None,
                };
                match open_slot {
                    Some(slot) => self.stack[slot] = v,
                    None => *uv.borrow_mut() = crate::value::Upvalue::Closed(v),
                }
            }
```
with:
```rust
            Op::GetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = self.frame().closure.upvalues[idx];
                let v = match &*uv.0.borrow() {
                    crate::value::Upvalue::Open(slot) => self.stack[*slot].clone(),
                    crate::value::Upvalue::Closed(v) => v.clone(),
                };
                self.push(v);
            }
            Op::SetUpvalue => {
                let idx = self.read_u8() as usize;
                let uv = self.frame().closure.upvalues[idx];
                let v = self.peek(0).clone();
                let open_slot = match &*uv.0.borrow() {
                    crate::value::Upvalue::Open(slot) => Some(*slot),
                    crate::value::Upvalue::Closed(_) => None,
                };
                match open_slot {
                    Some(slot) => self.stack[slot] = v,
                    None => *uv.0.borrow_mut() = crate::value::Upvalue::Closed(v),
                }
            }
```
(`uv` no longer needs `Rc::clone` — `Gc<UpvalueCell>` copies. `uv.borrow()` becomes `uv.0.borrow()` — `Gc<UpvalueCell>` derefs to `UpvalueCell`, a tuple struct wrapping the `RefCell`, so the field access is now explicit.)

`Op::CloseUpvalue`'s body (`let slot = self.stack.len() - 1; self.close_upvalues(slot); self.pop();`) and `Op::CloseUpvaluesFrom`'s body are unchanged — neither touches `Rc`/`Gc` directly, both just call `close_upvalues`, migrated next.

- [ ] **Step 3: Migrate `capture_upvalue`/`close_upvalues`**

Replace:
```rust
    fn close_upvalues(&mut self, from: usize) {
        let mut keep = Vec::with_capacity(self.open_upvalues.len());
        for uv in self.open_upvalues.drain(..) {
            let open_slot = match &*uv.borrow() {
                crate::value::Upvalue::Open(s) => Some(*s),
                crate::value::Upvalue::Closed(_) => None,
            };
            match open_slot {
                Some(s) if s >= from => {
                    let value = self.stack[s].clone();
                    *uv.borrow_mut() = crate::value::Upvalue::Closed(value);
                }
                Some(_) => keep.push(uv),
                None => {}
            }
        }
        self.open_upvalues = keep;
    }

    fn capture_upvalue(&mut self, slot: usize) -> Rc<RefCell<crate::value::Upvalue>> {
        for uv in &self.open_upvalues {
            if let crate::value::Upvalue::Open(s) = &*uv.borrow() {
                if *s == slot {
                    return Rc::clone(uv);
                }
            }
        }
        let uv = Rc::new(RefCell::new(crate::value::Upvalue::Open(slot)));
        self.open_upvalues.push(Rc::clone(&uv));
        uv
    }
```
with:
```rust
    fn close_upvalues(&mut self, from: usize) {
        let mut keep = Vec::with_capacity(self.open_upvalues.len());
        for uv in self.open_upvalues.drain(..) {
            let open_slot = match &*uv.0.borrow() {
                crate::value::Upvalue::Open(s) => Some(*s),
                crate::value::Upvalue::Closed(_) => None,
            };
            match open_slot {
                Some(s) if s >= from => {
                    let value = self.stack[s].clone();
                    *uv.0.borrow_mut() = crate::value::Upvalue::Closed(value);
                }
                Some(_) => keep.push(uv),
                None => {}
            }
        }
        self.open_upvalues = keep;
    }

    fn capture_upvalue(&mut self, slot: usize) -> Gc<crate::value::UpvalueCell> {
        for uv in &self.open_upvalues {
            if let crate::value::Upvalue::Open(s) = &*uv.0.borrow() {
                if *s == slot {
                    return *uv;
                }
            }
        }
        let uv = self
            .gc
            .allocate(crate::value::UpvalueCell(RefCell::new(
                crate::value::Upvalue::Open(slot),
            )));
        self.open_upvalues.push(uv);
        uv
    }
```

`close_upvalues`/`capture_upvalue` need `use std::cell::RefCell;` back in scope — add it to the `use` block at the top of the file (it was removed in Task 9's Step 1 rewrite since `Vm`/`CallFrame` themselves no longer mention it directly, but these two methods do).

- [ ] **Step 4: Run tests**

Run: `cargo test -p ember-vm --lib`
Expected: still fails to compile — `Op::MakeList`/`Op::MakeRecord`/`Op::MakeAdt`/etc. (Tasks 12-14) and `natives.rs` (Task 16) still reference the old `Rc`-based shapes. Confirm via `cargo build -p ember-vm 2>&1 | grep -c error` that the error count is going down relative to before this task (fewer "expected `Gc`, found `Rc`" errors in the upvalue-related lines specifically) rather than expecting a clean build yet.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate upvalue capture/close and OP_CLOSURE to Gc<UpvalueCell>"
```

---

### Task 12: Lists — `OP_MAKE_LIST`/`OP_GET_INDEX`/`OP_SET_INDEX`/`index_get`/`index_set`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Migrate `Op::MakeList`**

Replace:
```rust
            Op::MakeList => {
                let count = self.read_u16() as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.pop());
                }
                items.reverse(); // popped in reverse of push order
                self.push(Value::List(Rc::new(RefCell::new(items))));
            }
```
with:
```rust
            Op::MakeList => {
                let count = self.read_u16() as usize;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.pop());
                }
                items.reverse(); // popped in reverse of push order
                let list = self
                    .gc
                    .allocate(crate::value::ListObj(RefCell::new(items)));
                self.push(Value::List(list));
            }
```

- [ ] **Step 2: Migrate `index_get`/`index_set`**

Replace:
```rust
    fn index_get(&self, base: Value, index: Value) -> Result<Value, RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let l = l.borrow();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                Ok(l[*i as usize].clone())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }

    fn index_set(&self, base: Value, index: Value, value: Value) -> Result<(), RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let mut l = l.borrow_mut();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                l[*i as usize] = value;
                Ok(())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }
```
with:
```rust
    fn index_get(&self, base: Value, index: Value) -> Result<Value, RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let l = l.0.borrow();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                Ok(l[*i as usize].clone())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }

    fn index_set(&self, base: Value, index: Value, value: Value) -> Result<(), RuntimeError> {
        match (&base, &index) {
            (Value::List(l), Value::Int(i)) => {
                let mut l = l.0.borrow_mut();
                if *i < 0 || *i as usize >= l.len() {
                    return Err(self.runtime_error(format!(
                        "index {i} out of bounds for list of length {}",
                        l.len()
                    )));
                }
                l[*i as usize] = value;
                Ok(())
            }
            _ => Err(self.runtime_error(format!("cannot index {base:?} with {index:?}"))),
        }
    }
```
(`Op::GetIndex`/`Op::SetIndex`'s own match arms in `step` are unchanged — they just call these two helpers.)

- [ ] **Step 3: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate list construction and indexing to Gc<ListObj>"
```

---

### Task 13: Records — `OP_GET_FIELD`/`OP_SET_FIELD`/`OP_MAKE_RECORD`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Migrate `Op::GetField`/`Op::SetField`**

Replace:
```rust
            Op::GetField => {
                let name = self.read_global_name();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        let v = fields.borrow().get(&name).cloned();
                        match v {
                            Some(v) => self.push(v),
                            None => return Err(self.runtime_error(format!("no field `{name}`"))),
                        }
                    }
                    other => {
                        return Err(self
                            .runtime_error(format!("cannot access field `{name}` on {other:?}")))
                    }
                }
            }
            Op::SetField => {
                let name = self.read_global_name();
                let value = self.pop();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        fields.borrow_mut().insert(name, value.clone());
                        self.push(value);
                    }
                    other => {
                        return Err(
                            self.runtime_error(format!("cannot set field `{name}` on {other:?}"))
                        )
                    }
                }
            }
```
with:
```rust
            Op::GetField => {
                let name = self.read_global_name();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        let v = fields.0.borrow().get(&name).cloned();
                        match v {
                            Some(v) => self.push(v),
                            None => return Err(self.runtime_error(format!("no field `{name}`"))),
                        }
                    }
                    other => {
                        return Err(self
                            .runtime_error(format!("cannot access field `{name}` on {other:?}")))
                    }
                }
            }
            Op::SetField => {
                let name = self.read_global_name();
                let value = self.pop();
                let base = self.pop();
                match base {
                    Value::Record { fields, .. } => {
                        fields.0.borrow_mut().insert(name, value.clone());
                        self.push(value);
                    }
                    other => {
                        return Err(
                            self.runtime_error(format!("cannot set field `{name}` on {other:?}"))
                        )
                    }
                }
            }
```
(`{name}` keeps working unchanged — `Gc<String>` implements `Display` by forwarding through `Deref`, added in Task 2 alongside `Debug`, matching `Rc<String>`'s own behavior exactly. Only the `.0` on `fields` — accessing `RecordFields`' wrapped `RefCell` — is a real change from Phase 9.)

- [ ] **Step 2: Migrate `Op::MakeRecord`**

Replace:
```rust
            Op::MakeRecord => {
                let name_idx = self.read_u16();
                let type_name = self.str_constant(name_idx);
                let field_count = self.read_u16() as usize;
                let mut pairs = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let value = self.pop();
                    let name = match self.pop() {
                        Value::Str(s) => s,
                        other => panic!("record field name must be a string, found {other:?}"),
                    };
                    pairs.push((name, value));
                }
                pairs.reverse();
                let fields: FxHashMap<Rc<String>, Value> = pairs.into_iter().collect();
                self.push(Value::Record {
                    name: type_name,
                    fields: Rc::new(RefCell::new(fields)),
                });
            }
```
with:
```rust
            Op::MakeRecord => {
                let name_idx = self.read_u16();
                let type_name = self.str_constant(name_idx);
                let field_count = self.read_u16() as usize;
                let mut pairs = Vec::with_capacity(field_count);
                for _ in 0..field_count {
                    let value = self.pop();
                    let name = match self.pop() {
                        Value::Str(s) => s,
                        other => panic!("record field name must be a string, found {other:?}"),
                    };
                    pairs.push((name, value));
                }
                pairs.reverse();
                let fields: FxHashMap<Gc<String>, Value> = pairs.into_iter().collect();
                let fields = self.gc.allocate(crate::value::RecordFields(RefCell::new(fields)));
                self.push(Value::Record {
                    name: type_name,
                    fields,
                });
            }
```

- [ ] **Step 3: `Op::TestVariant`'s `Value::Record` arm**

`a.variant == name`/`*rname == name` (both `Gc<String>` now) still compile unchanged — `Gc<String>: PartialEq` forwards to `String`'s content comparison, same as `Rc<String>` did.

- [ ] **Step 4: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate record field access and construction to Gc<RecordFields>"
```

---

### Task 14: ADTs — `OP_MAKE_ADT`/`OP_TEST_VARIANT`/`OP_DESTRUCTURE`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Migrate `Op::MakeAdt`**

Replace:
```rust
            Op::MakeAdt => {
                let type_idx = self.read_u16();
                let type_name = self.str_constant(type_idx);
                let variant_idx = self.read_u16();
                let variant = self.str_constant(variant_idx);
                let arity = self.read_u16() as usize;
                let mut fields = Vec::with_capacity(arity);
                for _ in 0..arity {
                    fields.push(self.pop());
                }
                fields.reverse(); // positional order matters — see this task's own note
                self.push(Value::Adt(Rc::new(crate::value::AdtValue {
                    type_name,
                    variant,
                    fields,
                })));
            }
```
with:
```rust
            Op::MakeAdt => {
                let type_idx = self.read_u16();
                let type_name = self.str_constant(type_idx);
                let variant_idx = self.read_u16();
                let variant = self.str_constant(variant_idx);
                let arity = self.read_u16() as usize;
                let mut fields = Vec::with_capacity(arity);
                for _ in 0..arity {
                    fields.push(self.pop());
                }
                fields.reverse(); // positional order matters
                let adt = self.gc.allocate(crate::value::AdtValue {
                    type_name,
                    variant,
                    fields,
                });
                self.push(Value::Adt(adt));
            }
```

Note this is exactly the two-allocation-per-instruction case (`str_constant` for `type_name`, then again for `variant`, then the final `AdtValue` allocation — three `intern_str`/`allocate` calls total in the worst case) this plan's "Before you start" note names explicitly: safe here only because `maybe_collect` (Task 9) runs once at the *top* of `step`, before any of these three calls, not between them.

- [ ] **Step 2: `Op::TestVariant`/`Op::Destructure` — no change needed**

`a.variant == name` (Task 13's note applies identically here) and `a.fields[index].clone()` both keep compiling unchanged — `Value::Adt(Gc<AdtValue>)` derefs the same way `Rc<AdtValue>` did.

- [ ] **Step 3: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate ADT construction to Gc<AdtValue>"
```

---

### Task 15: String concatenation, `binary_arith`

**Files:**
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Migrate the `Str` arm of `binary_arith`**

Replace:
```rust
            (Op::Add, Value::Str(a), Value::Str(b)) => Ok(Value::Str(Rc::new(format!("{a}{b}")))),
```
with:
```rust
            (Op::Add, Value::Str(a), Value::Str(b)) => {
                Ok(Value::Str(self.gc.intern_str(&format!("{a}{b}"))))
            }
```
This changes `binary_arith`'s signature from `&self` to `&mut self` (it now needs `&mut self.gc`). Update its declaration:
```rust
    fn binary_arith(&mut self, op: Op, l: Value, r: Value) -> Result<Value, RuntimeError> {
```
and its one call site in `step`:
```rust
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod => {
                let b = self.pop();
                let a = self.pop();
                let v = self.binary_arith(op, a, b)?;
                self.push(v);
            }
```
is unchanged — `self.binary_arith(...)` already runs inside `step(&mut self)`, so it already has a `&mut self` to call it with; only the callee's own signature needed updating.

`compare` is untouched — it does no allocation.

- [ ] **Step 2: Full-crate build check**

Run: `cargo build -p ember-vm 2>&1 | head -80`
Expected: this is the first point since Task 8 where the whole `vm.rs` match arms in `step` should be internally consistent (`natives.rs` is still unmigrated — Task 16 — so expect errors there specifically, e.g. `NativeFn.func` signature mismatch, but `vm.rs` itself should show few or no errors of its own now). Fix anything unexpected in `vm.rs` before moving on; leave `natives.rs`-originated errors for Task 16.

- [ ] **Step 3: Commit**

```bash
git add crates/ember-vm/src/vm.rs
git commit -m "Migrate string concatenation to intern_str"
```

---

### Task 16: `natives.rs` — thread `&mut GcHeap` through, migrate `Op::Call`, migrate remaining tests

**Files:**
- Modify: `crates/ember-vm/src/natives.rs`
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Replace `natives.rs` in full**

```rust
use crate::error::RuntimeError;
use crate::value::{display_value, ListObj, Value};
use ember_gc::GcHeap;
use std::cell::RefCell;
use std::rc::Rc;

pub fn print(args: &[Value], _line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    println!("{}", display_value(&args[0]));
    Ok(Value::Nil)
}

pub fn len(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => Ok(Value::Int(l.0.borrow().len() as i64)),
        other => Err(RuntimeError::new(
            format!("len expects a list, found {other:?}"),
            line,
        )),
    }
}

pub fn push(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::List(l) => {
            l.0.borrow_mut().push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(RuntimeError::new(
            format!("push expects a list, found {other:?}"),
            line,
        )),
    }
}

pub fn clock(_args: &[Value], _line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(Value::Float(now.as_secs_f64()))
}

pub fn str_fn(args: &[Value], _line: u32, gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    Ok(Value::Str(gc.intern_str(&display_value(&args[0]))))
}

pub fn int_fn(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Int"), line)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Int"),
            line,
        )),
    }
}

pub fn float_fn(args: &[Value], line: u32, _gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("cannot convert \"{s}\" to Float"), line)),
        other => Err(RuntimeError::new(
            format!("cannot convert {other:?} to Float"),
            line,
        )),
    }
}

pub fn type_of(args: &[Value], _line: u32, gc: &mut GcHeap) -> Result<Value, RuntimeError> {
    let name = match &args[0] {
        Value::Int(_) => "Int".to_string(),
        Value::Float(_) => "Float".to_string(),
        Value::Bool(_) => "Bool".to_string(),
        Value::Nil => "Nil".to_string(),
        Value::Str(_) => "String".to_string(),
        Value::List(_) => "List".to_string(),
        Value::Closure(_) | Value::Native(_) => "Function".to_string(),
        Value::Adt(a) => (*a.type_name).clone(),
        Value::Record { name, .. } => (**name).clone(),
    };
    Ok(Value::Str(gc.intern_str(&name)))
}

type NativeImpl = fn(&[Value], u32, &mut GcHeap) -> Result<Value, RuntimeError>;

pub const NATIVES: &[(&str, usize, NativeImpl)] = &[
    ("print", 1, print),
    ("len", 1, len),
    ("push", 2, push),
    ("clock", 0, clock),
    ("str", 1, str_fn),
    ("int", 1, int_fn),
    ("float", 1, float_fn),
    ("type_of", 1, type_of),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn len_reports_list_length() {
        let mut gc = GcHeap::new();
        let list = Value::List(gc.allocate(ListObj(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))));
        let result = len(&[list], 1, &mut gc).unwrap();
        assert!(matches!(result, Value::Int(3)));
    }

    #[test]
    fn len_rejects_a_non_list() {
        let mut gc = GcHeap::new();
        assert!(len(&[Value::Int(1)], 1, &mut gc).is_err());
    }

    #[test]
    fn push_appends_in_place() {
        let mut gc = GcHeap::new();
        let list = Value::List(gc.allocate(ListObj(RefCell::new(vec![Value::Int(1)]))));
        push(&[list.clone(), Value::Int(2)], 1, &mut gc).unwrap();
        match &list {
            Value::List(l) => assert_eq!(l.0.borrow().len(), 2),
            _ => unreachable!(),
        }
    }

    #[test]
    fn str_formats_any_value() {
        let mut gc = GcHeap::new();
        let result = str_fn(&[Value::Int(42)], 1, &mut gc).unwrap();
        match result {
            Value::Str(s) => assert_eq!(*s, "42"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn int_parses_strings_and_truncates_floats() {
        let mut gc = GcHeap::new();
        assert!(matches!(
            int_fn(&[Value::Float(3.9)], 1, &mut gc).unwrap(),
            Value::Int(3)
        ));
        let s = Value::Str(gc.intern_str("42"));
        assert!(matches!(int_fn(&[s], 1, &mut gc).unwrap(), Value::Int(42)));
        let bad = Value::Str(gc.intern_str("nope"));
        assert!(int_fn(&[bad], 1, &mut gc).is_err());
    }

    #[test]
    fn float_parses_strings_and_widens_ints() {
        let mut gc = GcHeap::new();
        assert!(matches!(
            float_fn(&[Value::Int(3)], 1, &mut gc).unwrap(),
            Value::Float(f) if f == 3.0
        ));
    }

    #[test]
    fn type_of_names_every_kind_including_records_and_adts_by_their_own_name() {
        let mut gc = GcHeap::new();
        let int_name = type_of(&[Value::Int(1)], 1, &mut gc).unwrap();
        assert!(matches!(int_name, Value::Str(s) if *s == "Int"));

        let adt = Value::Adt(gc.allocate(crate::value::AdtValue {
            type_name: gc.intern_str("Shape"),
            variant: gc.intern_str("Circle"),
            fields: vec![],
        }));
        let adt_name = type_of(&[adt], 1, &mut gc).unwrap();
        assert!(matches!(adt_name, Value::Str(s) if *s == "Shape"));
    }

    #[test]
    fn clock_returns_a_float() {
        let mut gc = GcHeap::new();
        assert!(matches!(clock(&[], 1, &mut gc).unwrap(), Value::Float(_)));
    }

    #[test]
    fn natives_table_has_all_8_with_the_right_arities() {
        let expected: &[(&str, usize)] = &[
            ("print", 1),
            ("len", 1),
            ("push", 2),
            ("clock", 0),
            ("str", 1),
            ("int", 1),
            ("float", 1),
            ("type_of", 1),
        ];
        assert_eq!(NATIVES.len(), 8);
        for (name, arity) in expected {
            let found = NATIVES.iter().find(|(n, _, _)| n == name);
            assert!(found.is_some(), "missing native {name}");
            assert_eq!(found.unwrap().1, *arity, "wrong arity for {name}");
        }
    }
}
```

Note `Value::Adt(a) => (*a.type_name).clone()` in `type_of`: `a.type_name` is `Gc<String>`, `*a.type_name` derefs to `String`, `.clone()` gets an owned `String` — needed because `type_of` builds a `name: String` local across all its match arms uniformly. Same reasoning for the `Record` arm.

- [ ] **Step 2: Migrate `Op::Call`'s native-calling branch in `vm.rs`**

Replace:
```rust
                    Value::Native(n) => {
                        if argc != n.arity {
                            return Err(self.runtime_error(format!(
                                "expected {} argument(s), got {argc}",
                                n.arity
                            )));
                        }
                        let args_start = self.stack.len() - argc;
                        let args: Vec<Value> = self.stack[args_start..].to_vec();
                        self.stack.truncate(args_start - 1); // removes the native callee + its args
                        let line = self.chunk().line_at(self.frame().ip.saturating_sub(1));
                        let result = (n.func)(&args, line).map_err(|e| self.attach_trace(e))?;
                        self.push(result);
                    }
```
with:
```rust
                    Value::Native(n) => {
                        if argc != n.arity {
                            return Err(self.runtime_error(format!(
                                "expected {} argument(s), got {argc}",
                                n.arity
                            )));
                        }
                        let args_start = self.stack.len() - argc;
                        let args: Vec<Value> = self.stack[args_start..].to_vec();
                        self.stack.truncate(args_start - 1); // removes the native callee + its args
                        let line = self.chunk().line_at(self.frame().ip.saturating_sub(1));
                        let result =
                            (n.func)(&args, line, &mut self.gc).map_err(|e| self.attach_trace(e))?;
                        self.push(result);
                    }
```

- [ ] **Step 3: Migrate `vm.rs`'s test helper that constructs a native directly**

`calling_a_native_dispatches_immediately_with_no_extra_frame` builds `fn double(args: &[Value], _line: u32) -> Result<Value, RuntimeError>` — update its signature to match the new `NativeImpl`:
```rust
        fn double(args: &[Value], _line: u32, _gc: &mut ember_gc::GcHeap) -> Result<Value, RuntimeError> {
            match args[0] {
                Value::Int(n) => Ok(Value::Int(n * 2)),
                _ => unreachable!(),
            }
        }
```
No other change needed in that test.

- [ ] **Step 4: Migrate `vm.rs`'s remaining `Rc`-constructing tests**

`calling_a_closure_pushes_a_frame_and_the_result_replaces_the_whole_call` and `calling_with_the_wrong_arity_is_a_runtime_error` both currently do:
```rust
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner));
        let closure = Value::Closure(Rc::new(ClosureObj {
            proto: callee,
            upvalues: vec![],
        }));
```
Change the ordering so the `Vm` (and its `gc`) exists before the closure is built:
```rust
        let mut interner = Interner::new();
        let callee = Rc::new(callee_proto(&mut interner));
        let proto = script(|c| {
            let five = c.add_constant(ember_bytecode::value::Value::Int(5));
            c.write_op(Op::Constant, 1);
            c.write_u16(five, 1);
            c.write_op(Op::Call, 1);
            c.write_u8(1, 1);
            c.write_op(Op::Return, 1);
        });
        let mut vm = Vm::new(proto);
        let closure = Value::Closure(vm.gc_mut_for_test().allocate(ClosureObj {
            proto: callee,
            upvalues: vec![],
        }));
```
(i.e., move `let mut vm = Vm::new(proto);` up before building `closure`, and build `closure` via `vm.gc_mut_for_test().allocate(...)` instead of `Rc::new`; everything after — `let base = vm.stack_len_for_test(); vm.push_for_test(closure); ...` — stays the same, just remove the now-duplicate `let proto = script(...)` and `let mut vm = Vm::new(proto);` lines that follow in the original.) Apply the same reordering to `calling_with_the_wrong_arity_is_a_runtime_error`.

`field_read_and_write_on_a_struct` currently does:
```rust
        let mut fields = FxHashMap::default();
        fields.insert(Rc::new("x".to_string()), Value::Int(1));
        let record = Value::Record {
            name: Rc::new("P".to_string()),
            fields: Rc::new(RefCell::new(fields)),
        };
```
followed later by building `proto` and `let mut vm = Vm::new(proto);`. Reorder the same way:
```rust
        let proto = script(|c| {
            let x_name =
                c.add_constant(ember_bytecode::value::Value::Str(Rc::new("x".to_string())));
            let forty_two = int_const(c, 42);
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
            c.write_op(Op::Constant, 1);
            c.write_u16(forty_two, 1);
            c.write_op(Op::SetField, 1);
            c.write_u16(x_name, 1);
            c.write_op(Op::Pop, 1);
            c.write_op(Op::GetLocal, 1);
            c.write_u8(8, 1);
            c.write_op(Op::GetField, 1);
            c.write_u16(x_name, 1);
            c.write_op(Op::Return, 1);
        });

        let mut vm = Vm::new(proto);
        let gc = vm.gc_mut_for_test();
        let mut fields = FxHashMap::default();
        fields.insert(gc.intern_str("x"), Value::Int(1));
        let record = Value::Record {
            name: gc.intern_str("P"),
            fields: gc.allocate(crate::value::RecordFields(RefCell::new(fields))),
        };
        let base = vm.stack_len_for_test();
        assert_eq!(
            base, 8,
            "record must land where the GetLocal operand above expects it"
        );
        vm.push_for_test(record);
        let result = vm.run().expect("should not error");
        assert!(matches!(result, Value::Int(42)));
```
(the constant-pool string `Rc::new("x".to_string())` inside `script(...)`'s closure stays `Rc` — that's `ember_bytecode::value::Value::Str`, the compile-time constant type, unaffected by this migration.)

- [ ] **Step 5: Run the full `ember-vm` test suite**

Run: `cargo test -p ember-vm --lib`
Expected: PASS, all tests (the original 54 plus `value.rs`'s Task 8 additions — count should be ≥54). If anything fails, it's almost certainly one of the manual reorderings in Step 4 — re-check against `field_read_and_write_on_a_struct`'s original slot-8 assertion logic, which must be unchanged in meaning even though the surrounding code moved.

- [ ] **Step 6: Commit**

```bash
git add crates/ember-vm/src/natives.rs crates/ember-vm/src/vm.rs
git commit -m "Thread GcHeap through natives; migrate remaining Rc-constructing tests"
```

---

## Part 3 — Integration and verification

### Task 17: `gc-stress` conformance variant + bounded-heap test

**Files:**
- Modify: `crates/ember-cli/Cargo.toml`
- Modify: `crates/ember-cli/tests/conformance.rs`
- Modify: `crates/ember-vm/src/vm.rs`

- [ ] **Step 1: Add a `gc-stress`-enabling feature to `ember-cli`, forwarding to `ember-vm`/`ember-gc`**

```toml
# crates/ember-cli/Cargo.toml — add
[features]
gc-stress = ["ember-vm/gc-stress"]
```
```toml
# crates/ember-vm/Cargo.toml — add
[features]
gc-stress = ["ember-gc/gc-stress"]
```

- [ ] **Step 2: Run the existing conformance suite under the new feature**

Run: `cargo test -p ember-cli --features gc-stress --test conformance`
Expected: PASS — every fixture, through both backends, with a collection forced before literally every VM instruction. This is, per the design doc's own framing, "the real GC test": if any root was missed anywhere in Tasks 9-16, this is where it surfaces, typically as a wrong value, a panic on a freed pointer being dereferenced (in a debug build, more likely manifesting as wildly wrong data than a segfault, but treat any failure here as a root-tracking bug, not a flaky test). If it fails, use `--features gc-stress,ember-gc/gc-log` (via `cargo test -p ember-cli --features gc-stress -- --nocapture` with `ember-gc`'s `gc-log` also enabled through a similar feature-forward if needed) to see the allocate/blacken/free trace and find the missing root.

No new test code needed for this step — the *existing* conformance test, run under the new feature flag, is the check. If it doesn't already pass, that's a real bug to fix in `vm.rs`'s `mark_roots` closure (Task 9) before continuing.

- [ ] **Step 3: Add a bounded-heap long-running-loop test to `ember-vm`**

```rust
// add to the `mod tests` block in vm.rs
#[test]
fn heap_size_stays_bounded_in_a_long_running_allocation_loop() {
    let src = "
        let mut i = 0;
        let mut last = \"\";
        while i < 5000 {
            last = str(i);
            i = i + 1;
        }
        last;
    ";
    let (ast, mut interner, stmts, parse_diags) = ember_parser::parse(src);
    assert!(parse_diags.is_empty(), "parse diags: {parse_diags:?}");
    let (bindings, resolve_diags) = ember_resolve::resolve(&ast, &mut interner, &stmts);
    assert!(resolve_diags.is_empty(), "resolve diags: {resolve_diags:?}");
    let proto = ember_compile::compile(&ast, &mut interner, &bindings, &stmts);
    let mut vm = Vm::new(proto);
    let result = vm.run().expect("should not error");
    assert!(matches!(result, Value::Str(s) if *s == "4999"));
    assert!(
        vm.gc_mut_for_test().bytes_allocated() < 200_000,
        "5000 iterations each discarding the previous `last` must not retain all 5000 \
         strings — got {} bytes allocated, collection is not reclaiming discarded ones",
        vm.gc_mut_for_test().bytes_allocated()
    );
}
```
(`while` is real `ember` syntax — `ember-parser`'s `Stmt::While`/`while_stmt`, compiled via `ember-compile`'s `compile_while` — confirmed present before writing this test.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ember-vm --lib heap_size_stays_bounded`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ember-cli/Cargo.toml crates/ember-vm/Cargo.toml crates/ember-vm/src/vm.rs
git commit -m "Add gc-stress conformance feature wiring and a bounded-heap test"
```

---

### Task 18: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS, 0 failures — every `ember-gc` test from Part 1, every migrated `ember-vm` test from Part 2, the existing full workspace suite from every prior phase, all green.

- [ ] **Step 2: Run the workspace test suite again under `gc-stress`**

Run: `cargo test --workspace --features ember-cli/gc-stress`
Expected: PASS. (If Cargo complains the feature doesn't exist at the workspace root, run `cargo test -p ember-cli --features gc-stress` and `cargo test -p ember-vm --features gc-stress` separately instead — either way, both crates' suites must pass with stress mode on.)

- [ ] **Step 3: Clippy and fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. Pay particular attention to any `unsafe`-related lints in `ember-gc` (e.g. `clippy::missing_safety_doc` on the `unsafe fn` shims) — add a `/// # Safety` doc comment explaining the caller contract (header pointer must have been produced by `allocate::<T>`/`intern_str` for the matching `T`) rather than suppressing the lint.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 4: No commit this task** — verification only, nothing to stage.

---

### Task 19: `CHECKLIST.md` reconciliation

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Reconcile the Phase 10 section item-by-item**

Check off every 🔴/🟡 item that's genuinely done. Document these deliberate deviations explicitly (matching the style of Phase 9's own "Retroactive fixes" section):

- **No `ObjKind` enum** — `ember-gc` cannot know `ember-vm`'s concrete types (that would be a circular crate dependency), so kind-dispatch is done via a `Trace` trait + type-erased `trace_fn`/`drop_fn` function pointers captured per-allocation in `allocate<T: Trace>`, rather than a hardcoded enum. `ObjHeader` still carries everything the checklist names (`marked`, `next`, plus what a kind tag was for) — just not as a literal enum field.
- **`gc-stress` collects once per instruction, not once per allocation** — see this plan's "Before you start" note for the full soundness reasoning (verified against `OP_CLOSURE`'s and `OP_MAKE_ADT`'s multi-allocation-per-instruction cases specifically).
- **`mark_compiler_roots` is a documented no-op, not a ported function** — `ember-compile` is a fully separate, already-completed phase that never touches `ember-gc`; there is no interleaved compiler-heap-allocation scenario in this architecture the way clox has. See the design doc's own section on this.
- **GC pause-duration stats not tracked** (🟡, out of scope per the design doc's Non-goals — `GcStats` covers collections/bytes_freed/live_objects only).

- [ ] **Step 2: Final full-workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all clean (re-confirming Task 18 after the `CHECKLIST.md`-only edit, which shouldn't affect any of these, but confirm anyway per this project's established pattern).

- [ ] **Step 3: Commit**

```bash
git add CHECKLIST.md
git commit -m "Reconcile Phase 10 (Garbage Collector) against CHECKLIST.md"
```
