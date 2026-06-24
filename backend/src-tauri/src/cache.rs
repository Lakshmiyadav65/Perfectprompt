//! Stage B of the enhancement pipeline. A thin LRU keyed on the Stage A
//! fingerprint so that identical input + identical active app skips the
//! LLM and returns a previously-computed enhancement.
//!
//! Bounded at 256 entries — small in RAM, large enough that a few hours
//! of work tends to find hits. The mutex makes it cheap to share across
//! the pipeline's async stages without per-call cloning.

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Mutex;

/// Default capacity. Hoisted to a `const` so tests can reference it and
/// the construction in [AppState] doesn't drift from this file.
pub const DEFAULT_CAPACITY: usize = 256;

pub struct EnhancementCache {
    inner: Mutex<LruCache<String, String>>,
}

impl Default for EnhancementCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl EnhancementCache {
    /// Build a cache with the given capacity. `capacity == 0` is
    /// clamped to 1 so an accidental zero doesn't panic later — the
    /// `LruCache` constructor requires `NonZeroUsize`.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Look up an enhancement by fingerprint. Returns `None` on miss or
    /// if the lock is poisoned (which can only happen if another caller
    /// panicked inside `put`).
    pub fn get(&self, key: &str) -> Option<String> {
        let mut guard = self.inner.lock().ok()?;
        guard.get(key).cloned()
    }

    /// Insert a fingerprint → enhancement pair. Lock-poisoned errors
    /// are logged but never propagated; the pipeline must keep moving.
    pub fn put(&self, key: String, value: String) {
        match self.inner.lock() {
            Ok(mut guard) => {
                guard.put(key, value);
            }
            Err(_) => {
                eprintln!("[cache] put: lock poisoned, entry not cached");
            }
        }
    }

    /// Current entry count. Diagnostics and tests only.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn round_trip_returns_stored_value() {
        let c = EnhancementCache::new(8);
        c.put("fp1".into(), "enhanced text".into());
        assert_eq!(c.get("fp1"), Some("enhanced text".into()));
        assert_eq!(c.get("missing"), None);
    }

    #[test]
    fn capacity_evicts_oldest_entry() {
        let c = EnhancementCache::new(2);
        c.put("a".into(), "1".into());
        c.put("b".into(), "2".into());
        c.put("c".into(), "3".into()); // evicts "a"
        assert_eq!(c.get("a"), None, "oldest entry should be evicted");
        assert_eq!(c.get("b"), Some("2".into()));
        assert_eq!(c.get("c"), Some("3".into()));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn concurrent_puts_dont_panic_or_deadlock() {
        let cache = Arc::new(EnhancementCache::new(64));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let c = Arc::clone(&cache);
                thread::spawn(move || {
                    for j in 0..100 {
                        c.put(format!("k{i}_{j}"), format!("v{i}_{j}"));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread panicked");
        }
        assert!(cache.len() <= 64, "cache stays bounded under concurrent puts");
    }
}
