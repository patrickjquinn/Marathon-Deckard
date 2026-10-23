//! Marathon's user-session Deckard daemon (replaces C++ deckardd).
pub mod api;
pub mod app_channel;
pub mod audio;
pub mod auth;
pub mod contract;
pub mod ime;
pub mod intents;
pub mod keel;
pub mod location;
pub mod manage;
pub mod notifications;
pub mod packages;
pub mod prompts;
pub mod reclaim;
pub mod runtime;
pub mod sharing;
pub mod shell_sync;
pub mod storage;
pub mod watchdog;

/// Sends one message to an Android peer; false when it could not.
pub type Outbound = dyn Fn(serde_json::Map<String, serde_json::Value>) -> bool + Send + Sync;
