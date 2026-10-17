//! Running independent tasks on several threads.
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Threads to use, counting the caller's: `requested` (0 for every
/// processor), one when there is little work.
pub(crate) fn thread_count(requested: usize, tasks: usize) -> usize {
    let wanted = if requested == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        requested
    };
    wanted.clamp(1, 64).min(tasks.max(1))
}

/// `f(i)` for `i` in `0..count` on up to `threads` threads, results in
/// order; the first error (by index) wins.
pub(crate) fn map<T: Send, E: Send>(
    threads: usize,
    count: usize,
    f: impl Fn(usize) -> Result<T, E> + Sync,
) -> Result<Vec<T>, E> {
    if threads <= 1 || count <= 1 {
        return (0..count).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Result<T, E>>>> = (0..count).map(|_| Mutex::new(None)).collect();
    let work = || {
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= count {
                return;
            }
            let value = f(i);
            *results[i].lock().expect("result") = Some(value);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads.min(count) {
            scope.spawn(work);
        }
        work();
    });
    results
        .into_iter()
        .map(|r| r.into_inner().expect("result").expect("every index"))
        .collect()
}
