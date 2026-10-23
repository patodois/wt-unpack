//! Giving freed memory back to the operating system.
//!
//! Loading a level allocates and frees a great deal (decoded textures, vertex buffers). Freeing a block
//! does not hand its pages back: the system allocator keeps them for reuse, so a process that loaded a
//! 1 GB scene and dropped it still shows about a gigabyte of resident memory (on macOS the freed pages
//! stay dirty until the system is short of memory). Nothing leaks; the allocator is just holding on.
//!
//! [`release_freed_memory`] asks it to let go. The loaders call it when they finish, and
//! `WarThunder::clear_caches` calls it, so a consumer drops its scene, calls `clear_caches()`, and gets the
//! memory back.

/// Asks the system allocator to return its free pages to the operating system. Returns the bytes the
/// allocator says it released, when it says (macOS does; glibc reports nothing, so 0).
///
/// Supported on macOS (`malloc_zone_pressure_relief`) and on Linux with glibc (`malloc_trim`); elsewhere,
/// and when the program uses another global allocator, it does nothing.
pub fn release_freed_memory() -> usize {
    imp::release()
}

#[cfg(target_os = "macos")]
mod imp {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    pub fn release() -> usize {
        // Safety: a null zone means every zone, and a goal of 0 means as much as possible
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod imp {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }

    pub fn release() -> usize {
        // Safety: malloc_trim takes no pointers
        unsafe { malloc_trim(0) };
        0
    }
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
mod imp {
    pub fn release() -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn releasing_with_nothing_to_release_is_harmless() {
        let block = vec![1u8; 8 << 20];
        drop(block);
        super::release_freed_memory();
        super::release_freed_memory();
    }
}
