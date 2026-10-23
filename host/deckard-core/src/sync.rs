//! Mutex access that treats poison as fatal.

use std::sync::{Mutex, MutexGuard};

/// Locks `mutex`.
///
/// # Panics
/// If a thread panicked while holding it, which leaves the state unknown.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(_) => panic!("lock poisoned by a panicked thread"),
    }
}
