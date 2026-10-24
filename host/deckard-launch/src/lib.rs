//! Starts Android apps and intents and reads or writes Android properties,
//! through the Deckard session and Android's `IPlatform`.

pub mod binder;
pub mod session;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
