//! The IME channel: DeckardIme's editor relayed to the Shell's TextInput1,
//! and the Shell's input for the focused editor relayed back.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use deckard_core::ids::PackageName;
use deckard_core::keel::APP_ID_PREFIX;
use deckard_core::runtime_state::SHELL_BUS_NAME;
use deckard_core::sync;
use deckard_core::validate;
use serde_json::{Map, Value, json};
use zbus::Message;
use zbus::blocking::{Connection, MessageIterator, Proxy};

use crate::app_channel::{ChannelEvent, ChannelHandle};

const TEXT_INPUT_PATH: &str = "/org/marathonos/Shell/TextInput";
const TEXT_INPUT_INTERFACE: &str = "org.marathonos.Shell.TextInput1";
const MAX_TEXT: usize = 4096;
const MAX_PANELS: usize = 256;
const HINT_MASK: u32 = 0x3ff;

/// A TextInput1 call.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ShellCall {
    SetEditor(String, u32, u32),
    SetSurroundingText(String, String, i32, i32),
    ShowPanel(String),
    HidePanel(String),
    ClearEditor(String),
}

/// The focused Android editor, and the Shell's last panel for each app.
#[derive(Default)]
struct ImeState {
    app_id: Option<String>,
    panels: HashMap<String, (i32, i32)>,
}

fn qt_int(value: Option<&Value>) -> i32 {
    let value = value.and_then(Value::as_i64);
    value
        .and_then(|number| i32::try_from(number).ok())
        .unwrap_or(0)
}

fn bounded(value: &str) -> bool {
    validate::truncate(value, MAX_TEXT) == value
}

fn panel((covered, height): (i32, i32)) -> Map<String, Value> {
    crate::object(json!({"t": "panel", "covered": covered, "height": height}))
}

impl ImeState {
    /// Takes a DeckardIme message: the Shell call it makes, and the panel
    /// to replay to Android when an editor starts.
    fn android(
        &mut self,
        message: &Map<String, Value>,
    ) -> (Option<ShellCall>, Option<Map<String, Value>>) {
        let kind = message.get("t").and_then(Value::as_str);
        if kind == Some("start") {
            let package = message.get("package").and_then(Value::as_str);
            let Some(package) = package.and_then(|name| PackageName::try_from(name).ok()) else {
                return (None, None);
            };
            let app_id = format!("{APP_ID_PREFIX}{}", package.as_str());
            let purpose = u32::try_from(qt_int(message.get("purpose")));
            let purpose = purpose.ok().filter(|purpose| *purpose <= 13).unwrap_or(0);
            let hints = qt_int(message.get("hints")).cast_unsigned() & HINT_MASK;
            let replay = self.panels.get(&app_id).copied().map(panel);
            self.app_id = Some(app_id.clone());
            return (Some(ShellCall::SetEditor(app_id, purpose, hints)), replay);
        }
        let Some(app_id) = self.app_id.clone() else {
            return (None, None);
        };
        let call = match kind {
            Some("state") => {
                let value = message.get("text").and_then(Value::as_str).unwrap_or("");
                if !bounded(value) {
                    return (None, None);
                }
                let size = i32::try_from(value.encode_utf16().count()).unwrap_or(i32::MAX);
                let offset = |key| qt_int(message.get(key)).clamp(0, size);
                let (cursor, anchor) = (offset("cursor"), offset("anchor"));
                ShellCall::SetSurroundingText(app_id, value.to_owned(), cursor, anchor)
            }
            Some("show") => ShellCall::ShowPanel(app_id),
            Some("hide") => ShellCall::HidePanel(app_id),
            Some("finish") => {
                self.app_id = None;
                ShellCall::ClearEditor(app_id)
            }
            _ => return (None, None),
        };
        (Some(call), None)
    }

    /// The calls that withdraw the editor when DeckardIme goes away.
    fn disconnect(&mut self) -> Vec<ShellCall> {
        let Some(app_id) = self.app_id.take() else {
            return Vec::new();
        };
        vec![
            ShellCall::HidePanel(app_id.clone()),
            ShellCall::ClearEditor(app_id),
        ]
    }

    /// A TextInput1 signal as DeckardIme's message, when it is for the
    /// focused editor.
    fn shell(&mut self, signal: &Message) -> Option<Map<String, Value>> {
        let body = signal.body();
        match signal.header().member()?.as_str() {
            "CommitString" => {
                let (app, text, before, after): (String, String, i32, i32) =
                    body.deserialize().ok()?;
                let (before, after) = (before.max(0), after.max(0));
                let message =
                    json!({"t": "commit", "text": text, "before": before, "after": after});
                (self.focused(&app) && bounded(&text)).then(|| crate::object(message))
            }
            "Key" => {
                let (app, key): (String, String) = body.deserialize().ok()?;
                let known = matches!(
                    key.as_str(),
                    "enter" | "backspace" | "left" | "right" | "up" | "down" | "tab" | "escape"
                );
                let message = json!({"t": "key", "key": key});
                (self.focused(&app) && known).then(|| crate::object(message))
            }
            "PanelChanged" => {
                let (app, covered, height): (String, i32, i32) = body.deserialize().ok()?;
                if !app.starts_with(APP_ID_PREFIX) {
                    return None;
                }
                if self.panels.len() >= MAX_PANELS && !self.panels.contains_key(&app) {
                    self.panels.clear();
                }
                let shown = (covered.max(0), height.max(0));
                self.panels.insert(app.clone(), shown);
                self.focused(&app).then(|| panel(shown))
            }
            _ => None,
        }
    }

    fn focused(&self, app: &str) -> bool {
        self.app_id.as_deref() == Some(app)
    }
}

fn call(shell: &Proxy<'_>, call: &ShellCall) -> zbus::Result<()> {
    match call {
        ShellCall::SetEditor(app, purpose, hints) => {
            shell.call("SetEditor", &(app, purpose, hints))
        }
        ShellCall::SetSurroundingText(app, value, cursor, anchor) => {
            shell.call("SetSurroundingText", &(app, value, cursor, anchor))
        }
        ShellCall::ShowPanel(app) => shell.call("ShowPanel", &(app,)),
        ShellCall::HidePanel(app) => shell.call("HidePanel", &(app,)),
        ShellCall::ClearEditor(app) => shell.call("ClearEditor", &(app,)),
    }
}

fn send(shell: &Proxy<'_>, calls: &[ShellCall]) {
    for each in calls {
        crate::report("IME Shell call failed", call(shell, each));
    }
}

/// Registers the `waydroid.` app ids with the Shell.
fn register(shell: &Proxy<'_>) -> zbus::Result<()> {
    shell.call("Register", &(APP_ID_PREFIX,))
}

/// Relays between DeckardIme (`events`, `handle`) and the Shell on `bus`;
/// runs on its own thread. Messages to Android are queued under the lock,
/// which is never held across a D-Bus call.
pub fn serve(events: &Receiver<ChannelEvent>, handle: &ChannelHandle, bus: &Connection) {
    let shell = Proxy::new(bus, SHELL_BUS_NAME, TEXT_INPUT_PATH, TEXT_INPUT_INTERFACE);
    let shell = match shell {
        Ok(shell) => shell,
        Err(error) => return eprintln!("deckard-bridge: no Shell TextInput1: {error}"),
    };
    let state = Arc::new(Mutex::new(ImeState::default()));
    let (bus, owners) = (bus.clone(), bus.clone());
    let (listener, owner) = (shell.clone(), shell.clone());
    let (listener_state, listener_handle) = (Arc::clone(&state), handle.clone());
    crate::spawn("IME signal listener failed", move || {
        listen(&bus, &listener, &listener_state, &listener_handle)
    });
    crate::spawn("Shell owner watch failed", move || {
        follow_owner(&owners, &owner)
    });
    let mut current = None;
    for event in events {
        match event {
            ChannelEvent::Connected(generation) => {
                if current.replace(generation).is_some() {
                    let calls = sync::lock(&state).disconnect();
                    send(&shell, &calls);
                }
                crate::report("IME register failed", register(&shell));
            }
            ChannelEvent::Message(generation, message) if current == Some(generation) => {
                let mut ime = sync::lock(&state);
                let (call, replay) = ime.android(&message);
                if let Some(replay) = replay {
                    let _ = handle.send_to(generation, replay);
                }
                drop(ime);
                send(&shell, call.as_slice());
            }
            ChannelEvent::Disconnected(generation) if current == Some(generation) => {
                current = None;
                let calls = sync::lock(&state).disconnect();
                send(&shell, &calls);
            }
            _ => {}
        }
    }
}

/// Registers once subscribed, so no signal can overtake the subscription.
fn listen(
    bus: &Connection,
    shell: &Proxy<'_>,
    state: &Mutex<ImeState>,
    handle: &ChannelHandle,
) -> zbus::Result<()> {
    let rule = crate::signals(SHELL_BUS_NAME, TEXT_INPUT_PATH, TEXT_INPUT_INTERFACE)?.build();
    let signals = MessageIterator::for_match_rule(rule, bus, Some(64))?;
    crate::report("IME register failed", register(shell));
    for signal in signals {
        match signal {
            Ok(signal) => {
                let mut ime = sync::lock(state);
                if let Some(message) = ime.shell(&signal) {
                    let _ = handle.send(message);
                }
            }
            Err(error) => eprintln!("deckard-bridge: Shell IME signal failed: {error}"),
        }
    }
    Ok(())
}

/// Registers again whenever the Shell gets a new bus owner.
fn follow_owner(bus: &Connection, shell: &Proxy<'_>) -> zbus::Result<()> {
    let changes =
        MessageIterator::for_match_rule(crate::owner_changes(SHELL_BUS_NAME)?, bus, Some(8))?;
    for change in changes {
        if change.is_ok_and(|change| crate::gained_owner(&change)) {
            crate::report("IME register failed", register(shell));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "protocol fixtures use known-valid values"
)]
mod tests {
    use super::*;

    use zbus::interface;

    struct MockShell(Arc<Mutex<Vec<ShellCall>>>);

    #[interface(name = "org.marathonos.Shell.TextInput1")]
    impl MockShell {
        #[zbus(name = "Register")]
        fn register(&self, prefix: String) {
            assert_eq!(prefix, APP_ID_PREFIX);
        }

        #[zbus(name = "SetEditor")]
        fn set_editor(&self, app: String, purpose: u32, hints: u32) {
            let call = ShellCall::SetEditor(app, purpose, hints);
            self.0.lock().unwrap().push(call);
        }

        #[zbus(name = "SetSurroundingText")]
        fn set_surrounding_text(&self, app: String, value: String, cursor: i32, anchor: i32) {
            let call = ShellCall::SetSurroundingText(app, value, cursor, anchor);
            self.0.lock().unwrap().push(call);
        }

        #[zbus(name = "ShowPanel")]
        fn show_panel(&self, app: String) {
            self.0.lock().unwrap().push(ShellCall::ShowPanel(app));
        }

        #[zbus(name = "HidePanel")]
        fn hide_panel(&self, app: String) {
            self.0.lock().unwrap().push(ShellCall::HidePanel(app));
        }

        #[zbus(name = "ClearEditor")]
        fn clear_editor(&self, app: String) {
            self.0.lock().unwrap().push(ShellCall::ClearEditor(app));
        }
    }

    fn message(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    fn signal<B>(member: &str, body: &B) -> Message
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let builder = Message::signal(TEXT_INPUT_PATH, TEXT_INPUT_INTERFACE, member).unwrap();
        builder.build(body).unwrap()
    }

    fn call_of(ime: &mut ImeState, value: Value) -> Option<ShellCall> {
        ime.android(&message(value)).0
    }

    #[test]
    fn fixture_start_state_and_utf16_offsets() {
        let mut ime = ImeState::default();
        assert_eq!(call_of(&mut ime, json!({"t":"show"})), None);
        assert_eq!(
            call_of(&mut ime, json!({"t":"start","package":"../bad"})),
            None
        );
        let start = json!({"t":"start","package":"org.example.App","purpose":99,"hints":2047});
        let app = String::from("waydroid.org.example.App");
        assert_eq!(
            call_of(&mut ime, start),
            Some(ShellCall::SetEditor(app.clone(), 0, 1023))
        );
        assert_eq!(
            call_of(
                &mut ime,
                json!({"t":"state","text":"a😀b","cursor":3,"anchor":1})
            ),
            Some(ShellCall::SetSurroundingText(
                app.clone(),
                "a😀b".into(),
                3,
                1
            ))
        );
        assert_eq!(
            call_of(
                &mut ime,
                json!({"t":"state","text":"a😀b","cursor":99,"anchor":-4})
            ),
            Some(ShellCall::SetSurroundingText(app, "a😀b".into(), 4, 0))
        );
        let long = json!({"t":"state","text":"x".repeat(4097)});
        assert_eq!(call_of(&mut ime, long), None);
    }

    #[test]
    fn fixture_panel_replay_disconnect_and_filtered_shell_input() {
        let app = "waydroid.org.example";
        let mut ime = ImeState::default();
        assert!(
            ime.shell(&signal("PanelChanged", &(app, -4, 700)))
                .is_none()
        );
        let (_, replay) = ime.android(&message(json!({"t":"start","package":"org.example"})));
        assert_eq!(
            replay,
            Some(message(json!({"t":"panel","covered":0,"height":700})))
        );
        assert_eq!(
            ime.shell(&signal("Key", &(app, "enter"))),
            Some(message(json!({"t":"key","key":"enter"})))
        );
        assert!(
            ime.shell(&signal("Key", &("waydroid.org.other", "enter")))
                .is_none()
        );
        assert!(ime.shell(&signal("Key", &(app, "delete"))).is_none());
        assert_eq!(
            ime.shell(&signal("CommitString", &(app, "😀", -3, 2))),
            Some(message(
                json!({"t":"commit","text":"😀","before":0,"after":2})
            ))
        );
        let other = signal("CommitString", &("waydroid.org.other", "x", 0, 0));
        assert!(ime.shell(&other).is_none());
        assert_eq!(
            call_of(&mut ime, json!({"t":"show"})),
            Some(ShellCall::ShowPanel(app.into()))
        );
        assert_eq!(
            call_of(&mut ime, json!({"t":"hide"})),
            Some(ShellCall::HidePanel(app.into()))
        );
        assert_eq!(
            call_of(&mut ime, json!({"t":"finish"})),
            Some(ShellCall::ClearEditor(app.into()))
        );
        assert!(ime.disconnect().is_empty());
        call_of(&mut ime, json!({"t":"start","package":"org.example"}));
        assert_eq!(
            ime.disconnect(),
            vec![
                ShellCall::HidePanel(app.into()),
                ShellCall::ClearEditor(app.into())
            ]
        );
        assert!(ime.shell(&signal("Key", &(app, "enter"))).is_none());
    }

    #[test]
    fn private_bus_matches_shell_method_signatures() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let owner = Connection::session()?;
        owner
            .object_server()
            .at(TEXT_INPUT_PATH, MockShell(Arc::clone(&calls)))?;
        owner.request_name(SHELL_BUS_NAME)?;
        let bus = Connection::session()?;
        let shell = Proxy::new(&bus, SHELL_BUS_NAME, TEXT_INPUT_PATH, TEXT_INPUT_INTERFACE)?;
        register(&shell)?;
        let expected = vec![
            ShellCall::SetEditor("waydroid.org.example".into(), 7, 0x200),
            ShellCall::SetSurroundingText("waydroid.org.example".into(), "a😀b".into(), 3, 1),
            ShellCall::ShowPanel("waydroid.org.example".into()),
            ShellCall::HidePanel("waydroid.org.example".into()),
            ShellCall::ClearEditor("waydroid.org.example".into()),
        ];
        for each in &expected {
            call(&shell, each)?;
        }
        assert_eq!(*calls.lock().unwrap(), expected);
        Ok(())
    }
}
