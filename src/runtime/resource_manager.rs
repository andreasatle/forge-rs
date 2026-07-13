//! Bounded-concurrency gate for provider dispatch.
//!
//! One [`ResourceManager`] is intended per provider: it caps how many
//! callers may hold a permit for that provider at once. Acquisition blocks
//! the calling thread; release happens automatically when the returned
//! [`ResourcePermit`] guard drops, so a caller cannot forget to release.

use std::sync::{Arc, Condvar, Mutex};

struct Inner {
    state: Mutex<usize>,
    condvar: Condvar,
}

/// A bounded-concurrency gate with a fixed number of permits.
///
/// Cloning a [`ResourceManager`] shares the same underlying permit pool; all
/// clones contend for the same permits.
#[derive(Clone)]
pub struct ResourceManager {
    inner: Arc<Inner>,
}

impl ResourceManager {
    /// Create a gate with `max_concurrent` permits.
    ///
    /// # Panics
    ///
    /// Panics if `max_concurrent` is zero.
    pub fn new(max_concurrent: usize) -> Self {
        assert!(
            max_concurrent > 0,
            "ResourceManager requires at least one permit"
        );
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(max_concurrent),
                condvar: Condvar::new(),
            }),
        }
    }

    /// Block the calling thread until a permit is available, then return a
    /// guard that releases the permit on drop.
    pub fn acquire(&self) -> ResourcePermit {
        let mut available = self
            .inner
            .state
            .lock()
            .expect("resource manager mutex poisoned");
        while *available == 0 {
            available = self
                .inner
                .condvar
                .wait(available)
                .expect("resource manager mutex poisoned");
        }
        *available -= 1;
        ResourcePermit {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// RAII permit acquired from a [`ResourceManager`].
///
/// Releases the permit and wakes one waiting thread when dropped, including
/// on panic or early return — there is no manual release call.
pub struct ResourcePermit {
    inner: Arc<Inner>,
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        let mut available = self
            .inner
            .state
            .lock()
            .expect("resource manager mutex poisoned");
        *available += 1;
        drop(available);
        self.inner.condvar.notify_one();
    }
}

#[cfg(test)]
#[path = "resource_manager_tests.rs"]
mod tests;
