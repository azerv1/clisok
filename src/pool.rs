//! A fixed set of worker threads that take items strictly in order.
//!
//! Starting the slowest items first keeps one straggler from stretching the
//! whole run.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Run `f` on every item with `jobs` threads. Items start in order (the first
/// item first) and results come back in the same order as the input.
pub fn run_in_order<T: Send, R: Send>(items: Vec<T>, jobs: usize, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    let inputs: Vec<Mutex<Option<T>>> = items.into_iter().map(|t| Mutex::new(Some(t))).collect();
    let outputs: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..jobs.clamp(1, n.max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = inputs.get(i) else { break };
                    let item = slot.lock().unwrap().take().expect("each item is taken once");
                    let result = f(item);
                    *outputs[i].lock().unwrap() = Some(result);
                }
            });
        }
    });
    outputs.into_iter().map(|m| m.into_inner().unwrap().expect("every item ran")).collect()
}

/// Default worker count: `per_core` workers per logical core, capped at `max`.
pub fn default_jobs(per_core: usize, max: usize) -> usize {
    let cores = thread::available_parallelism().map_or(8, |n| n.get());
    (cores * per_core).clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn keeps_input_order() {
        let out = run_in_order((0..100).collect(), 8, |x: u32| x * 2);
        assert_eq!(out, (0..100).map(|x| x * 2).collect::<Vec<_>>());
    }

    #[test]
    fn starts_items_in_order() {
        let started = Mutex::new(Vec::new());
        run_in_order((0..20).collect(), 1, |x: u32| started.lock().unwrap().push(x));
        assert_eq!(started.into_inner().unwrap(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn runs_items_concurrently() {
        let t = Instant::now();
        run_in_order(vec![(); 8], 8, |_| thread::sleep(Duration::from_millis(200)));
        assert!(t.elapsed() < Duration::from_millis(800), "{:?}", t.elapsed());
    }

    #[test]
    fn handles_empty_input() {
        let out: Vec<u8> = run_in_order(Vec::<u8>::new(), 4, |x| x);
        assert!(out.is_empty());
    }
}
