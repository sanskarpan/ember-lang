pub mod handle;
pub mod heap;
pub mod trace;

pub use handle::Gc;
pub use heap::{GcHeap, GcStats};
pub use trace::{Trace, Tracer};
