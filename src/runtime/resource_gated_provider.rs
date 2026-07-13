//! Bounded-concurrency gate wrapped around a [`ProviderClient`].

use crate::providers::{ProviderClient, ProviderError, ProviderRequest, ProviderResponse};
use crate::runtime::ResourceManager;

/// Wraps a [`ProviderClient`] so that every call acquires a permit from a
/// shared [`ResourceManager`] before reaching `inner`, and releases it as
/// soon as that single call returns.
///
/// Placed below [`crate::providers::RetryingProvider`] in the provider
/// stack, so each retry attempt acquires its own permit rather than holding
/// one for the whole retry loop — a node's producer/critic/referee turns
/// (and their retries) each gate independently instead of one permit being
/// held for the node's entire dispatch.
pub struct ResourceGatedProvider<P> {
    inner: P,
    resource_manager: ResourceManager,
}

impl<P> ResourceGatedProvider<P> {
    /// Wrap `inner`, gating every call through `resource_manager`.
    pub fn new(inner: P, resource_manager: ResourceManager) -> Self {
        Self {
            inner,
            resource_manager,
        }
    }
}

impl<P: ProviderClient> ProviderClient for ResourceGatedProvider<P> {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let _permit = self.resource_manager.acquire();
        self.inner.call(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    struct DelayedProvider {
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    impl ProviderClient for DelayedProvider {
        fn call(&self, _request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(now_active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(50));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ProviderResponse {
                content: "done".to_string(),
                finish_reason: None,
            })
        }
    }

    fn request() -> ProviderRequest {
        ProviderRequest {
            prompt: "hi".to_string(),
            max_tokens: 16,
            output_schema: None,
        }
    }

    #[test]
    fn gate_caps_concurrent_calls_at_the_configured_permit_count() {
        // Invariant: a ResourceManager-gated provider never lets more than
        // `max_concurrent` calls execute at once, even when many more
        // threads are contending for it.
        let active = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));
        let gated = Arc::new(ResourceGatedProvider::new(
            DelayedProvider {
                active: Arc::clone(&active),
                max_observed: Arc::clone(&max_observed),
            },
            ResourceManager::new(2),
        ));
        let barrier = Arc::new(Barrier::new(5));

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let gated = Arc::clone(&gated);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    gated.call(request()).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        assert!(
            max_observed.load(Ordering::SeqCst) <= 2,
            "at most 2 calls should ever run concurrently, observed {}",
            max_observed.load(Ordering::SeqCst)
        );
    }
}
