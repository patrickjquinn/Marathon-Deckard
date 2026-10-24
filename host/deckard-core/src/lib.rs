//! Protocol, validation and policy shared by Deckard's host services.

pub mod acl;
pub mod ids;
pub mod json;
pub mod keel;
pub mod messages;
pub mod palette;
pub mod png_guard;
pub mod pressure;
pub mod runtime_state;
pub mod sync;
pub mod transport;
pub mod validate;

/// Why a value from Android was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid identity")]
    InvalidIdentity,
    #[error("invalid bridge frame")]
    InvalidFrame,
    #[error("bridge buffer limit exceeded")]
    BufferLimit,
    #[error("bridge rate limit exceeded")]
    Flood,
    #[error("invalid RGB color")]
    InvalidColor,
}
