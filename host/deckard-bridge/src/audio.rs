//! Marathon Shell audio policy mirrored to the authenticated Android socket.

use std::collections::HashMap;
use std::io;
use std::sync::mpsc::Sender;

use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, message};

use crate::app_channel::ChannelHandle;

const SHELL: &str = "org.marathonos.Shell";
const SETTINGS_PATH: &str = "/org/marathonos/Shell/Settings";
const SETTINGS_INTERFACE: &str = "org.marathonos.Shell.Settings1";
const AUDIO_PATH: &str = "/org/marathonos/Shell/Audio";
const AUDIO_INTERFACE: &str = "org.marathonos.Shell.Audio1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioEvent {
    StateChanged,
    ShellAppeared,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioState {
    pub quiet: bool,
    pub alarm: f64,
}

impl AudioState {
    pub fn from_shell(state: &HashMap<String, OwnedValue>) -> Self {
        let profile = state
            .get("audioProfile")
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_default();
        let dnd = state
            .get("dndEnabled")
            .and_then(|value| bool::try_from(value.clone()).ok())
            .unwrap_or(false);
        let alarm = state
            .get("alarmVolume")
            .and_then(|value| f64::try_from(value.clone()).ok())
            .filter(|value| value.is_finite())
            .unwrap_or(0.9)
            .clamp(0.0, 1.0);
        Self {
            quiet: dnd || matches!(profile.as_str(), "silent" | "vibrate"),
            alarm,
        }
    }

    pub fn message(&self) -> Map<String, Value> {
        json!({"t":"audio", "quiet":self.quiet, "alarm":self.alarm})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }
}

pub struct AudioSync {
    bus: Connection,
    last: Option<AudioState>,
}

impl AudioSync {
    pub fn new(bus: Connection) -> Self {
        Self { bus, last: None }
    }

    /// Call after `StateChanged`, relevant `PropertyChanged`, or Shell restart.
    /// Returns nothing if the policy is unchanged. A failed D-Bus read keeps
    /// the last good policy and may be retried at the next signal.
    pub fn refresh(&mut self) -> zbus::Result<Option<Map<String, Value>>> {
        let proxy = Proxy::new(&self.bus, SHELL, SETTINGS_PATH, SETTINGS_INTERFACE)?;
        let state: HashMap<String, OwnedValue> = proxy.call("GetState", &())?;
        let state = AudioState::from_shell(&state);
        if self.last.as_ref() == Some(&state) {
            return Ok(None);
        }
        self.last = Some(state.clone());
        Ok(Some(state.message()))
    }

    pub fn resend(&mut self) -> zbus::Result<Map<String, Value>> {
        if let Some(last) = &self.last {
            return Ok(last.message());
        }
        self.refresh()?
            .ok_or_else(|| zbus::Error::Failure("missing audio state".into()))
    }

    /// Use the channel's current authenticated generation. The handle
    /// rejects a send if Android disconnected during the Shell D-Bus call.
    pub fn refresh_to(&mut self, channel: &ChannelHandle) -> io::Result<()> {
        if let Some(message) = self.refresh().map_err(io::Error::other)? {
            channel.send(message)?;
        }
        Ok(())
    }

    pub fn resend_to(&mut self, channel: &ChannelHandle) -> io::Result<()> {
        let message = self.resend().map_err(io::Error::other)?;
        channel.send(message)
    }
}

/// Run on a dedicated thread. Subscribe before the initial `refresh`, or a
/// Shell signal between the first read and match registration could be lost.
pub fn listen_audio_state(bus: Connection, sender: Sender<AudioEvent>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(SHELL)?
        .path(AUDIO_PATH)?
        .interface(AUDIO_INTERFACE)?
        .member("StateChanged")?
        .build();
    for event in MessageIterator::for_match_rule(rule, &bus, Some(16))? {
        if event.is_ok() && sender.send(AudioEvent::StateChanged).is_err() {
            break;
        }
    }
    Ok(())
}

/// Run on a dedicated thread beside `listen_audio_state`.
pub fn listen_audio_settings(bus: Connection, sender: Sender<AudioEvent>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(SHELL)?
        .path(SETTINGS_PATH)?
        .interface(SETTINGS_INTERFACE)?
        .member("PropertyChanged")?
        .build();
    for event in MessageIterator::for_match_rule(rule, &bus, Some(16))? {
        let Ok(event) = event else { continue };
        let Ok((name,)) = event.body().deserialize::<(String,)>() else {
            continue;
        };
        if matches!(name.as_str(), "dndEnabled" | "audioProfile" | "alarmVolume")
            && sender.send(AudioEvent::StateChanged).is_err()
        {
            break;
        }
    }
    Ok(())
}

/// Run on a dedicated thread so a restarted Shell replays its audio state.
pub fn watch_shell_owner(bus: Connection, sender: Sender<AudioEvent>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(SHELL)?
        .build();
    for event in MessageIterator::for_match_rule(rule, &bus, Some(8))? {
        let Ok(event) = event else { continue };
        let Ok((_, _, owner)) = event.body().deserialize::<(String, String, String)>() else {
            continue;
        };
        if !owner.is_empty() && sender.send(AudioEvent::ShellAppeared).is_err() {
            break;
        }
    }
    Ok(())
}
