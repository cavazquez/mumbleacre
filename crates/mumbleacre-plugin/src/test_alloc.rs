use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct TrackingAllocator;

fn record_allocation() {
    if TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every operation delegates unchanged to the system allocator;
// the thread-local counter is observational only.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: delegated with the caller-provided layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: delegated with the caller-provided layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: delegated with the caller-provided pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: delegated with the caller-provided allocation metadata.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

pub(crate) fn count_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let already_tracking = TRACKING.with(|tracking| tracking.replace(true));
    assert!(!already_tracking, "allocation tracking cannot be nested");
    ALLOCATIONS.with(|count| count.set(0));
    let output = operation();
    let allocations = ALLOCATIONS.with(Cell::get);
    TRACKING.with(|tracking| tracking.set(false));
    (output, allocations)
}
