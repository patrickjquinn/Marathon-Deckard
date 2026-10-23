//! Root-side LXC, image mount, and network primitives. Not a production service.

pub mod command;
pub mod lxc;
pub mod manager;
pub mod mounts;
pub mod network;

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(String),
    Command(String),
    Timeout(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Invalid(message) | Self::Command(message) | Self::Timeout(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
