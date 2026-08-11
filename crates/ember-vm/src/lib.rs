pub mod alloc_counter;
pub mod error;
pub mod natives;
pub mod value;
pub mod vm;

pub use error::RuntimeError;
pub use value::Value;
pub use vm::{StepOutcome, Vm};

#[cfg(feature = "count-allocs")]
#[global_allocator]
static GLOBAL_ALLOC_COUNTER: alloc_counter::CountingAlloc = alloc_counter::CountingAlloc::new();

#[cfg(feature = "count-allocs")]
pub fn alloc_stats() -> alloc_counter::AllocStats {
    GLOBAL_ALLOC_COUNTER.snapshot()
}

#[cfg(feature = "count-allocs")]
pub fn reset_alloc_stats() {
    GLOBAL_ALLOC_COUNTER.reset()
}
