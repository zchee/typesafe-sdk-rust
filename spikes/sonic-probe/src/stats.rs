//! Heap deltas taken around one measured section.

/// The change in dhat's heap counters across a closure.
///
/// `total_*` are monotonic counters over the whole run, so their difference is
/// what the section itself allocated, including memory it freed again.
/// `curr_*` is the live heap, so its difference is what the section retained.
#[derive(Debug, Clone, Copy)]
pub struct Measured {
    pub blocks: u64,
    pub bytes: u64,
    pub curr_blocks: i64,
    pub curr_bytes: i64,
    pub max_bytes_before: usize,
    pub max_bytes_after: usize,
}

impl Measured {
    /// Runs `body` between two reads of the heap counters.
    pub fn around<F, T>(body: F) -> Self
    where
        F: FnOnce() -> T,
    {
        let before = dhat::HeapStats::get();
        let value = body();
        let after = dhat::HeapStats::get();
        // Dropping after the second read keeps the freed bytes out of the
        // `curr_*` delta, so "retained" means retained past the section.
        drop(value);

        Self {
            blocks: after.total_blocks - before.total_blocks,
            bytes: after.total_bytes - before.total_bytes,
            curr_blocks: as_i64(after.curr_blocks) - as_i64(before.curr_blocks),
            curr_bytes: as_i64(after.curr_bytes) - as_i64(before.curr_bytes),
            max_bytes_before: before.max_bytes,
            max_bytes_after: after.max_bytes,
        }
    }

    pub fn print(&self, label: &str) {
        println!(
            "{label}: blocks={} bytes={} curr_blocks={:+} curr_bytes={:+} max_bytes={}->{}",
            self.blocks,
            self.bytes,
            self.curr_blocks,
            self.curr_bytes,
            self.max_bytes_before,
            self.max_bytes_after,
        );
    }
}

/// `usize` counters are compared as signed, because a section that frees more
/// than it allocates moves `curr_*` down.
fn as_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
