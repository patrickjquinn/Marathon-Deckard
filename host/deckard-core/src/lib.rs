//! Protocol, validation and policy shared by Deckard's host services.

use std::fmt::{self, Display, Formatter};

pub mod acl;
pub mod ids;
pub mod json;
pub mod keel;
pub mod messages;
pub mod notification_state;
pub mod palette;
pub mod png_guard;
pub mod pressure;
pub mod runtime_state;
pub mod sync;
pub mod transport;
pub mod validate;

/// Why a value from Android was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidIdentity,
    InvalidFrame,
    BufferLimit,
    Flood,
    InvalidColor,
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentity => "invalid identity",
            Self::InvalidFrame => "invalid bridge frame",
            Self::BufferLimit => "bridge buffer limit exceeded",
            Self::Flood => "bridge rate limit exceeded",
            Self::InvalidColor => "invalid RGB color",
        })
    }
}

impl std::error::Error for Error {}
