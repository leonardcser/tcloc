use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static REALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static BYTES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static BYTES_DEALLOCATED: AtomicU64 = AtomicU64::new(0);
static CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static T_ALLOCS: Cell<u64> = const { Cell::new(0) };
    static T_BYTES: Cell<u64> = const { Cell::new(0) };
}

pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            BYTES_ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            let cur = CURRENT_BYTES.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            update_peak(cur);
            // Per-thread tally for perf::Guard attribution. `try_with`
            // is required because the allocator can run during TLS
            // teardown (drop order), which would otherwise panic.
            let _ = T_ALLOCS.try_with(|c| c.set(c.get() + 1));
            let _ = T_BYTES.try_with(|c| c.set(c.get() + layout.size() as u64));
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        BYTES_DEALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        CURRENT_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            REALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            let old = layout.size();
            if new_size >= old {
                BYTES_ALLOCATED.fetch_add((new_size - old) as u64, Ordering::Relaxed);
                let cur =
                    CURRENT_BYTES.fetch_add(new_size - old, Ordering::Relaxed) + (new_size - old);
                update_peak(cur);
                let _ = T_BYTES.try_with(|c| c.set(c.get() + (new_size - old) as u64));
            } else {
                BYTES_DEALLOCATED.fetch_add((old - new_size) as u64, Ordering::Relaxed);
                CURRENT_BYTES.fetch_sub(old - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

/// Calling-thread allocation totals. Used by `perf::Guard` to attribute
/// allocs to the thread doing the work, free of cross-thread noise.
pub fn thread_snapshot() -> (u64, u64) {
    let allocs = T_ALLOCS.try_with(|c| c.get()).unwrap_or(0);
    let bytes = T_BYTES.try_with(|c| c.get()).unwrap_or(0);
    (allocs, bytes)
}

fn update_peak(cur: usize) {
    let mut peak = PEAK_BYTES.load(Ordering::Relaxed);
    while cur > peak {
        match PEAK_BYTES.compare_exchange_weak(peak, cur, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(p) => peak = p,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AllocStats {
    pub allocs: u64,
    pub deallocs: u64,
    pub reallocs: u64,
    pub bytes_allocated: u64,
    pub bytes_deallocated: u64,
    pub current_bytes: usize,
    pub peak_bytes: usize,
}

pub fn snapshot() -> AllocStats {
    AllocStats {
        allocs: ALLOC_COUNT.load(Ordering::Relaxed),
        deallocs: DEALLOC_COUNT.load(Ordering::Relaxed),
        reallocs: REALLOC_COUNT.load(Ordering::Relaxed),
        bytes_allocated: BYTES_ALLOCATED.load(Ordering::Relaxed),
        bytes_deallocated: BYTES_DEALLOCATED.load(Ordering::Relaxed),
        current_bytes: CURRENT_BYTES.load(Ordering::Relaxed),
        peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
    }
}

pub fn delta(start: AllocStats, end: AllocStats) -> AllocStats {
    AllocStats {
        allocs: end.allocs.saturating_sub(start.allocs),
        deallocs: end.deallocs.saturating_sub(start.deallocs),
        reallocs: end.reallocs.saturating_sub(start.reallocs),
        bytes_allocated: end.bytes_allocated.saturating_sub(start.bytes_allocated),
        bytes_deallocated: end
            .bytes_deallocated
            .saturating_sub(start.bytes_deallocated),
        current_bytes: end.current_bytes,
        peak_bytes: end.peak_bytes,
    }
}
