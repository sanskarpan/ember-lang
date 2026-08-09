use crate::handle::{Gc, ObjHeader};
use std::ptr::NonNull;

/// Implemented by every type that lives on the GC heap. `trace` must call
/// `tracer.mark(...)` for every `Gc<U>` handle this value directly holds —
/// anything it doesn't report here is invisible to the collector and will
/// be freed while still reachable. This is the classic GC bug: getting a
/// `trace` impl wrong doesn't error at the point of the mistake, it
/// corrupts memory somewhere unrelated, later.
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
