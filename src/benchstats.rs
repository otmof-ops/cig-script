// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! Allocation counts for `cig bench --stats` and `cig run --stats` (#179
//! step 4), behind the `bench-stats` cargo feature.
//!
//! With the feature, `src/main.rs` installs [`Counting`] around the allocator
//! the platform would use anyway (the system's, or mimalloc on musl), and
//! counts calls and bytes. Without it nothing is wrapped, nothing is counted
//! and [`snapshot`] is `None`: a release build pays nothing, and says so when
//! asked (`cargo build --features bench-stats` makes a build that counts).

#[cfg(feature = "bench-stats")]
use std::alloc::{GlobalAlloc, Layout};
#[cfg(feature = "bench-stats")]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The reason written into an `unavailable` line.
pub const UNAVAILABLE: &str = "built without the bench-stats feature";
/// The same, as a sentence for a person.
pub const UNAVAILABLE_LONG: &str =
    "this cig was built without the `bench-stats` feature, so allocations are not counted (cargo build --features bench-stats)";

/// What the counting allocator has seen since the process started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub allocs: u64,
    pub bytes: u64,
    pub frees: u64,
}

#[cfg(feature = "bench-stats")]
static ALLOCS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-stats")]
static BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "bench-stats")]
static FREES: AtomicU64 = AtomicU64::new(0);

/// The counts so far, or `None` in a build that does not count.
pub fn snapshot() -> Option<Snapshot> {
    #[cfg(feature = "bench-stats")]
    {
        Some(Snapshot {
            allocs: ALLOCS.load(Relaxed),
            bytes: BYTES.load(Relaxed),
            frees: FREES.load(Relaxed),
        })
    }
    #[cfg(not(feature = "bench-stats"))]
    {
        None
    }
}

/// An allocator that counts and then does what the one inside does.
#[cfg(feature = "bench-stats")]
pub struct Counting<A>(pub A);

// SAFETY: every call is forwarded unchanged to the inner allocator, which
// upholds the GlobalAlloc contract; the counters are plain atomics and never
// allocate.
#[cfg(feature = "bench-stats")]
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        self.0.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        self.0.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        FREES.fetch_add(1, Relaxed);
        self.0.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new as u64, Relaxed);
        self.0.realloc(p, l, new)
    }
}
