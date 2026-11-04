//! Test support: a global allocator that remembers the largest single allocation a thread asked for, so a
//! test can show that a count read from a file does not size an allocation before it is checked.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
    static REFUSE_FROM: Cell<usize> = const { Cell::new(usize::MAX) };
}

pub struct Probe;

fn note(size: usize) {
    let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
}

/// Whether this thread has been told to fail an allocation of `size` bytes (see [`refusing_from`])
fn refuses(size: usize) -> bool {
    REFUSE_FROM
        .try_with(|from| size >= from.get())
        .unwrap_or(false)
}

// SAFETY: every call is forwarded to the system allocator unchanged, except that one a test asked to fail returns null
// (which `GlobalAlloc` allows: it is how an allocator reports that it is out of memory)
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        if refuses(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        if refuses(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        if refuses(new_size) {
            return std::ptr::null_mut();
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Runs `f` and returns its result with the size in bytes of the largest allocation it requested on this thread
pub fn largest_allocation<R>(f: impl FnOnce() -> R) -> (R, usize) {
    LARGEST.with(|largest| largest.set(0));
    let out = f();
    (out, LARGEST.with(Cell::get))
}

/// Runs `f` with every single allocation of `bytes` or more on this thread failing (the allocator returns null, as
/// it does when the memory is not there), so a test can show that a fallible allocation returns an error
pub fn refusing_from<R>(bytes: usize, f: impl FnOnce() -> R) -> R {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            REFUSE_FROM.with(|from| from.set(usize::MAX));
        }
    }
    REFUSE_FROM.with(|from| from.set(bytes));
    let _restore = Restore;
    f()
}
