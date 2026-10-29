//! Deckard's root container manager, upstream Waydroid's `container`
//! service: LXC lifecycle, image mounts, networking, session files, the
//! hardware service and image initialization.

pub mod command;
pub mod config;
pub mod dbus;
pub mod drivers;
pub mod hardware;
pub mod initializer;
pub mod initializer_dbus;
pub mod lxc;
pub mod mounts;
pub mod network;
pub mod properties;
mod resources;
pub mod session_config;

pub use deckard_launch::Result;

/// `Ok` when `valid`, else an error carrying `message`.
pub(crate) fn ensure(valid: bool, message: impl std::fmt::Display) -> Result<()> {
    if valid {
        Ok(())
    } else {
        Err(message.to_string().into())
    }
}
