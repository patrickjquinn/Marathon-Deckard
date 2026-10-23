//! Inactive Rust user-session Deckard daemon slice.

pub mod api;
pub mod app_channel;
pub mod auth;
pub mod contract;
pub mod ime;
pub mod location;
pub mod manage;
pub mod notifications;
pub mod packages;
pub mod prompts;
pub mod runtime;
pub mod storage;

/// Sends one message to an Android peer; false when it could not.
pub type Outbound = dyn Fn(serde_json::Map<String, serde_json::Value>) -> bool + Send + Sync;
