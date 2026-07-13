use super::*;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Acquiring up to `max_concurrent` permits must never block the caller.
#[test]
fn acquires_up_to_max_without_blocking() {
    let manager = ResourceManager::new(3);
    let permits: Vec<_> = (0..3).map(|_| manager.acquire()).collect();
    assert_eq!(permits.len(), 3);
}

/// An acquire beyond the permit count must block until a held permit is released.
#[test]
fn acquire_beyond_max_blocks_until_release() {
    let manager = ResourceManager::new(1);
    let first = manager.acquire();

    let (tx, rx) = mpsc::channel();
    let waiter_manager = manager.clone();
    let handle = thread::spawn(move || {
        let _second = waiter_manager.acquire();
        tx.send(()).unwrap();
    });

    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "waiter acquired a permit while none were free"
    );

    drop(first);

    rx.recv_timeout(Duration::from_secs(1))
        .expect("waiter did not acquire the permit after it was released");
    handle.join().unwrap();
}

/// A permit must be released even when the holding thread panics.
#[test]
fn permit_releases_on_drop_after_panic() {
    let manager = ResourceManager::new(1);
    let panicking_manager = manager.clone();

    let result = std::panic::catch_unwind(move || {
        let _permit = panicking_manager.acquire();
        panic!("simulated failure while holding a permit");
    });
    assert!(result.is_err());

    let regained = manager.acquire();
    drop(regained);
}

/// A permit must be released when the guard drops via an early return.
#[test]
fn permit_releases_on_early_return() {
    fn acquire_and_return_early(manager: &ResourceManager) {
        let _permit = manager.acquire();
    }

    let manager = ResourceManager::new(1);
    acquire_and_return_early(&manager);

    let regained = manager.acquire();
    drop(regained);
}
