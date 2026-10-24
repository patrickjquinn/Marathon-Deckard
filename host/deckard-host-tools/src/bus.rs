//! D-Bus connections and blocking work that leave no thread behind.
//! `blocking` 1.7.0 never retires its last pool thread, which then wakes
//! every 500 ms; zbus uses that pool to connect a Unix socket.

use std::any::Any;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::panic::{self, AssertUnwindSafe};
use std::thread;

use zbus::Address;
use zbus::address::transport::{Transport, UnixSocket};
use zbus::connection::Builder;

/// A builder for `address`, connected here for a Unix socket address.
///
/// # Errors
/// The address names a Unix socket that refuses the connection.
pub fn builder(address: Address) -> zbus::Result<Builder<'static>> {
    let stream = match address.transport() {
        Transport::Unix(unix) => match unix.path() {
            UnixSocket::File(path) => UnixStream::connect(path)?,
            UnixSocket::Abstract(name) => {
                UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_encoded_bytes())?)?
            }
            _ => return Builder::address(address),
        },
        _ => return Builder::address(address),
    };
    Ok(Builder::async_io_unix_stream(stream))
}

/// # Errors
/// The session bus is unreachable.
pub fn session() -> zbus::Result<zbus::blocking::Connection> {
    Ok(async_io::block_on(builder(Address::session()?)?.build())?.into())
}

/// # Errors
/// The system bus is unreachable.
pub fn system() -> zbus::Result<zbus::blocking::Connection> {
    Ok(async_io::block_on(builder(Address::system()?)?.build())?.into())
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
