//! Marathon's quiet profiles and alarm volume mirrored to Android.

use std::collections::HashMap;

use deckard_core::runtime_state::SHELL_BUS_NAME;
use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, Message};

use crate::app_channel::ChannelHandle;
use crate::{Relevant, Wake};

const SETTINGS_PATH: &str = "/org/marathonos/Shell/Settings";
const SETTINGS_INTERFACE: &str = "org.marathonos.Shell.Settings1";

/// The policy Android applies: alarms-only Do Not Disturb and alarm volume.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioState {
    /// Android goes to alarms-only Do Not Disturb.
    pub quiet: bool,
    /// Android's alarm stream volume, 0..=1.
    pub alarm: f64,
}

impl AudioState {
    /// Silent, vibrate and Do Not Disturb are quiet; alarm volume is 0..=1.
    #[must_use]
    pub fn from_shell(state: &HashMap<String, OwnedValue>) -> Self {
        let get = |key: &str| state.get(key).cloned();
        let profile = get("audioProfile").and_then(|value| String::try_from(value).ok());
        let dnd = get("dndEnabled").and_then(|value| bool::try_from(value).ok());
        let alarm = get("alarmVolume")
            .and_then(|value| f64::try_from(value).ok())
            .filter(|value| value.is_finite())
            .unwrap_or(0.9);
        Self {
            quiet: dnd == Some(true) || matches!(profile.as_deref(), Some("silent" | "vibrate")),
            alarm: alarm.clamp(0.0, 1.0),
        }
    }

    /// The `audio` message for DeckardBridge.
    #[must_use]
    pub fn message(&self) -> Map<String, Value> {
        crate::object(json!({"t": "audio", "quiet": self.quiet, "alarm": self.alarm}))
    }
}

/// Follows the Shell's audio settings and sends the policy to Android when it
/// changes or a new Android connection, announced with a wake, has not had it.
///
/// # Errors
///
/// A Shell signal subscription failed.
pub fn spawn(bus: Connection, channel: ChannelHandle) -> zbus::Result<Wake> {
    let audio = crate::signals(
        SHELL_BUS_NAME,
        "/org/marathonos/Shell/Audio",
        "org.marathonos.Shell.Audio1",
    );
    let settings = crate::signals(SHELL_BUS_NAME, SETTINGS_PATH, SETTINGS_INTERFACE)?;
    let rules: Vec<(MatchRule<'static>, Relevant)> = vec![
        (audio?.member("StateChanged")?.build(), |_| true),
        (settings.member("PropertyChanged")?.build(), setting_changed),
        (crate::owner_changes(SHELL_BUS_NAME)?, crate::gained_owner),
    ];
    let (mut last, mut sent) = (None, None);
    crate::follow(&bus.clone(), rules, move || {
        match read_shell(&bus) {
            Ok(state) => last = Some(state),
            Err(error) => {
                eprintln!("deckard-bridge: cannot read Marathon's audio settings: {error}")
            }
        }
        let (Some(generation), Some(state)) = (channel.generation(), &last) else {
            return;
        };
        let current = Some((generation, state.clone()));
        if sent != current && channel.send_to(generation, state.message()).is_ok() {
            sent = current;
        }
    })
}

fn setting_changed(message: &Message) -> bool {
    message
        .body()
        .deserialize::<(String, OwnedValue)>()
        .is_ok_and(|(name, _)| {
            matches!(name.as_str(), "dndEnabled" | "audioProfile" | "alarmVolume")
        })
}

fn read_shell(bus: &Connection) -> zbus::Result<AudioState> {
    let proxy = Proxy::new(bus, SHELL_BUS_NAME, SETTINGS_PATH, SETTINGS_INTERFACE)?;
    let state: HashMap<String, OwnedValue> = proxy.call("GetState", &())?;
    Ok(AudioState::from_shell(&state))
}
