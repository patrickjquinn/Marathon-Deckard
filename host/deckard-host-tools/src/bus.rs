//! D-Bus connections and blocking work that leave no thread behind.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::thread;

/// # Errors
/// The session bus is unreachable.
pub fn session() -> zbus::Result<zbus::Connection> {
    zbus::block_on(zbus::Connection::session())
}

/// # Errors
/// The system bus is unreachable.
pub fn system() -> zbus::Result<zbus::Connection> {
    zbus::block_on(zbus::Connection::system())
}

/// Runs `work` on a thread of its own, which ends with it.
pub async fn unblock<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = async_channel::bounded(1);
    thread::spawn(move || sender.send_blocking(panic::catch_unwind(AssertUnwindSafe(work))));
    let gone = |error| Box::new(error) as Box<dyn Any + Send>;
    let outcome = receiver
        .recv()
        .await
        .map_err(gone)
        .and_then(|outcome| outcome);
    outcome.unwrap_or_else(|panic| panic::resume_unwind(panic))
}
