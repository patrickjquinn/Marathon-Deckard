//! In-process IME channel adapter for the Rust Deckard bridge.

use std::sync::mpsc::Sender;

use serde_json::{Map, Value};
use zbus::blocking::{Connection, MessageIterator};
use zbus::{MatchRule, message};

use deckard_ime::ImeState;
pub use deckard_ime::{
    APP_ID_PREFIX, SHELL_NAME, ShellBus, ShellCall, TEXT_INPUT_INTERFACE, TEXT_INPUT_PATH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellSignal {
    CommitString(String, String, i32, i32),
    Key(String, String),
    PanelChanged(String, i32, i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    Listening,
    Signal(ShellSignal),
    Appeared,
}

#[derive(Default)]
pub struct ImeController {
    state: ImeState,
}

pub struct ImeEffects {
    pub shell_calls: Vec<ShellCall>,
    pub to_android: Option<Map<String, Value>>,
}

impl ImeController {
    pub fn android_message(&mut self, message: &Map<String, Value>) -> ImeEffects {
        let shell_calls = self.state.handle_android(message);
        let to_android = shell_calls
            .iter()
            .any(|call| matches!(call, ShellCall::SetEditor(..)))
            .then(|| self.state.current_panel())
            .flatten();
        ImeEffects {
            shell_calls,
            to_android,
        }
    }

    pub fn shell_signal(&mut self, signal: ShellSignal) -> Option<Map<String, Value>> {
        match signal {
            ShellSignal::CommitString(app, text, before, after) => {
                self.state.commit_string(&app, &text, before, after)
            }
            ShellSignal::Key(app, key) => self.state.key(&app, &key),
            ShellSignal::PanelChanged(app, covered, height) => {
                self.state.panel_changed(&app, covered, height)
            }
        }
    }

    pub fn disconnected(&mut self) -> Vec<ShellCall> {
        self.state.disconnect()
    }
}

pub fn send_shell_calls(bus: &ShellBus, calls: &[ShellCall]) -> zbus::Result<()> {
    let mut first_error = None;
    for call in calls {
        if let Err(error) = bus.call(call) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn parse_signal(message: &message::Message) -> Option<ShellSignal> {
    match message.header().member()?.as_str() {
        "CommitString" => {
            let (app, text, before, after) = message.body().deserialize().ok()?;
            Some(ShellSignal::CommitString(app, text, before, after))
        }
        "Key" => {
            let (app, key) = message.body().deserialize().ok()?;
            Some(ShellSignal::Key(app, key))
        }
        "PanelChanged" => {
            let (app, covered, height) = message.body().deserialize().ok()?;
            Some(ShellSignal::PanelChanged(app, covered, height))
        }
        _ => None,
    }
}

/// Run on a dedicated thread. Wait for `Listening` before registering with
/// Shell, so a first signal cannot overtake the match subscription.
pub fn listen_shell(connection: Connection, sender: Sender<ShellEvent>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender(SHELL_NAME)?
        .path(TEXT_INPUT_PATH)?
        .interface(TEXT_INPUT_INTERFACE)?
        .build();
    let messages = MessageIterator::for_match_rule(rule, &connection, Some(64))?;
    if sender.send(ShellEvent::Listening).is_err() {
        return Ok(());
    }
    for message in messages {
        match message {
            Ok(message) => {
                if let Some(signal) = parse_signal(&message)
                    && sender.send(ShellEvent::Signal(signal)).is_err()
                {
                    break;
                }
            }
            Err(error) => eprintln!("deckard-bridge: Shell IME signal failed: {error}"),
        }
    }
    Ok(())
}

/// Re-register `waydroid.` when Shell gets a new bus owner.
pub fn watch_shell_owner(connection: Connection, sender: Sender<ShellEvent>) -> zbus::Result<()> {
    let rule = MatchRule::builder()
        .msg_type(message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(SHELL_NAME)?
        .build();
    let messages = MessageIterator::for_match_rule(rule, &connection, Some(8))?;
    for message in messages {
        let Ok(message) = message else { continue };
        let Ok((_, _, owner)) = message.body().deserialize::<(String, String, String)>() else {
            continue;
        };
        if !owner.is_empty() && sender.send(ShellEvent::Appeared).is_err() {
            break;
        }
    }
    Ok(())
}
