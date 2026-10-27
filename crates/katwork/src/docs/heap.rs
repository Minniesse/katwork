//! The bytes a read holds on its pool thread. `ReadHeap`, the process's global allocator, counts
//! what each thread allocates and frees while a count is open (inside `deadline::scope`), so
//! `deadline::check` can refuse a read whose live bytes pass `DocLimits::max_read_bytes`. A
//! binary that reads documents names it as its `#[global_allocator]`; without it nothing is
//! counted and `installed` is false.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

/// One open count: the bytes allocated less those freed since it opened (negative when the
/// thread frees what it held before), and the most that ever was.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Count {
    live: i64,
    peak: i64,
}

thread_local! {
    static COUNT: Cell<Option<Count>> = const { Cell::new(None) };
}

/// `System`, counting each thread's live bytes while a count is open on it.
pub struct ReadHeap;

fn add(bytes: i64) {
    // `try_with`: the allocator may run while the thread's locals are torn down.
    let _ = COUNT.try_with(|count| {
        if let Some(mut open) = count.get() {
            open.live = open.live.saturating_add(bytes);
            open.peak = open.peak.max(open.live);
            count.set(Some(open));
        }
    });
}

fn size(bytes: usize) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

// SAFETY: every operation forwards its pointer and layout to `System` unchanged; the count is a
// const-initialized thread local with no destructor, so updating it never allocates.
unsafe impl GlobalAlloc for ReadHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            add(size(layout.size()));
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            add(size(layout.size()));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        add(-size(layout.size()));
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            add(size(new_size) - size(layout.size()));
        }
        moved
    }
}

/// Opens a count on this thread, returning the enclosing one for `close`.
fn open() -> Option<Count> {
    COUNT.replace(Some(Count::default()))
}

/// Closes this thread's count, folding it into the enclosing one; returns its peak.
fn close(outer: Option<Count>) -> u64 {
    let inner = COUNT.replace(None).unwrap_or_default();
    COUNT.set(outer.map(|mut outer| {
        outer.peak = outer.peak.max(outer.live.saturating_add(inner.peak));
        outer.live = outer.live.saturating_add(inner.live);
        outer
    }));
    u64::try_from(inner.peak).unwrap_or(0)
}

/// The bytes this thread's open count holds now, or 0 with none open.
pub(crate) fn live() -> u64 {
    COUNT
        .get()
        .map_or(0, |count| u64::try_from(count.live).unwrap_or(0))
}

/// Closes the count it holds when dropped, on a panic too.
pub(crate) struct Opened(Option<Option<Count>>);

impl Opened {
    pub(crate) fn new() -> Opened {
        Opened(Some(open()))
    }

    /// Closes the count now; its peak.
    pub(crate) fn close(mut self) -> u64 {
        self.0.take().map_or(0, close)
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        if let Some(outer) = self.0.take() {
            close(outer);
        }
    }
}

/// The most bytes `f` held at once on this thread, beside its result. The peak-heap harness
/// measures reads with it.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let opened = Opened::new();
    let result = f();
    (result, opened.close())
}

/// Whether `ReadHeap` is this process's global allocator.
pub fn installed() -> bool {
    let (_, peak) = measure(|| std::hint::black_box(vec![0u8; 4096]));
    peak >= 4096
}
