//! Marathon's quiet profiles and alarm volume mirrored to Android.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use deckard_core::runtime_state::SHELL_BUS_NAME;
use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::OwnedValue;
use zbus::{MatchRule, Message};

use crate::app_channel::ChannelHandle;

const SETTINGS_PATH: &str = "/org/marathonos/Shell/Settings";
const SETTINGS_INTERFACE: &str = "org.marathonos.Shell.Settings1";

type Relevant = fn(&Message) -> bool;

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

/// Wakes the audio thread to re-read the Shell and resend if needed.
#[derive(Clone)]
pub struct AudioWake(SyncSender<()>);

impl AudioWake {
    /// Call when Android connects; a wake already pending covers this one.
    /// False once the audio thread has stopped.
    pub fn wake(&self) -> bool {
        !matches!(self.0.try_send(()), Err(TrySendError::Disconnected(())))
    }
}

/// Follows the Shell's audio settings and sends the policy to Android when it
/// changes or a new Android connection has not had it yet.
///
/// # Errors
///
/// A Shell signal subscription failed.
pub fn spawn(bus: Connection, channel: ChannelHandle) -> zbus::Result<AudioWake> {
    let (sender, wakes) = mpsc::sync_channel(1);
    let wake = AudioWake(sender);
    let audio = crate::signals(
        SHELL_BUS_NAME,
        "/org/marathonos/Shell/Audio",
        "org.marathonos.Shell.Audio1",
    );
    let settings = crate::signals(SHELL_BUS_NAME, SETTINGS_PATH, SETTINGS_INTERFACE)?;
    let rules: [(MatchRule<'static>, Relevant); 3] = [
        (audio?.member("StateChanged")?.build(), |_| true),
        (settings.member("PropertyChanged")?.build(), setting_changed),
        (crate::owner_changes(SHELL_BUS_NAME)?, crate::gained_owner),
    ];
    for (rule, relevant) in rules {
        let messages = MessageIterator::for_match_rule(rule, &bus, Some(16))?;
        let wake = wake.clone();
        thread::spawn(move || {
            for message in messages {
                let Ok(message) = message else { continue };
                if relevant(&message) && !wake.wake() {
                    break;
                }
            }
        });
    }
    wake.wake();
    thread::spawn(move || run(&bus, &channel, &wakes));
    Ok(wake)
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

fn run(bus: &Connection, channel: &ChannelHandle, wakes: &Receiver<()>) {
    let mut last = None;
    let mut sent = None;
    for () in wakes {
        match read_shell(bus) {
            Ok(state) => last = Some(state),
            Err(error) => {
                eprintln!("deckard-bridge: cannot read Marathon's audio settings: {error}")
            }
        }
        let (Some(generation), Some(state)) = (channel.generation(), &last) else {
            continue;
        };
        let current = Some((generation, state.clone()));
        if sent != current && channel.send_to(generation, state.message()).is_ok() {
            sent = current;
        }
    }
}
