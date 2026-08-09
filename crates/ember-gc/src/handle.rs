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
