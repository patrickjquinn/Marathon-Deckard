//! IME protocol state and the Shell TextInput1 adapter.

use std::collections::HashMap;

use deckard_core::ids::PackageName;
use deckard_core::validate;
use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, Proxy};

pub const APP_ID_PREFIX: &str = "waydroid.";
pub const SHELL_NAME: &str = "org.marathonos.Shell";
pub const TEXT_INPUT_PATH: &str = "/org/marathonos/Shell/TextInput";
pub const TEXT_INPUT_INTERFACE: &str = "org.marathonos.Shell.TextInput1";
const MAX_TEXT: usize = 4096;
const HINT_MASK: u32 = 0x3ff;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellCall {
    SetEditor(String, u32, u32),
    SetSurroundingText(String, String, i32, i32),
    ShowPanel(String),
    HidePanel(String),
    ClearEditor(String),
}

#[derive(Default)]
pub struct ImeState {
    app_id: Option<String>,
    panels: HashMap<String, (i32, i32)>,
}

fn qt_int(value: Option<&Value>) -> i32 {
    value
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .unwrap_or(0)
}

fn bounded_text(value: &str) -> bool {
    validate::truncate(value, MAX_TEXT) == value
}

impl ImeState {
    pub fn handle_android(&mut self, message: &Map<String, Value>) -> Vec<ShellCall> {
        let Some(kind) = message.get("t").and_then(Value::as_str) else {
            return Vec::new();
        };
        if kind == "start" {
            let Some(package) = message
                .get("package")
                .and_then(Value::as_str)
                .and_then(|name| PackageName::try_from(name).ok())
            else {
                return Vec::new();
            };
            let app_id = format!("{APP_ID_PREFIX}{}", package.as_str());
            self.app_id = Some(app_id.clone());
            let purpose = qt_int(message.get("purpose"));
            let purpose = if (0..=13).contains(&purpose) {
                purpose as u32
            } else {
                0
            };
            let hints = (qt_int(message.get("hints")) as u32) & HINT_MASK;
            return vec![ShellCall::SetEditor(app_id, purpose, hints)];
        }
        let Some(app_id) = self.app_id.clone() else {
            return Vec::new();
        };
        match kind {
            "state" => {
                let value = message.get("text").and_then(Value::as_str).unwrap_or("");
                if !bounded_text(value) {
                    return Vec::new();
                }
                let size = i32::try_from(value.encode_utf16().count()).unwrap_or(i32::MAX);
                vec![ShellCall::SetSurroundingText(
                    app_id,
                    value.to_owned(),
                    qt_int(message.get("cursor")).clamp(0, size),
                    qt_int(message.get("anchor")).clamp(0, size),
                )]
            }
            "show" => vec![ShellCall::ShowPanel(app_id)],
            "hide" => vec![ShellCall::HidePanel(app_id)],
            "finish" => {
                self.app_id = None;
                vec![ShellCall::ClearEditor(app_id)]
            }
            _ => Vec::new(),
        }
    }

    pub fn disconnect(&mut self) -> Vec<ShellCall> {
        self.app_id.take().map_or_else(Vec::new, |app_id| {
            vec![
                ShellCall::HidePanel(app_id.clone()),
                ShellCall::ClearEditor(app_id),
            ]
        })
    }

    pub fn commit_string(
        &self,
        app_id: &str,
        value: &str,
        before: i32,
        after: i32,
    ) -> Option<Map<String, Value>> {
        if self.app_id.as_deref() != Some(app_id) || !bounded_text(value) {
            return None;
        }
        json!({"t": "commit", "text": value, "before": before.max(0), "after": after.max(0)})
            .as_object()
            .cloned()
    }

    pub fn key(&self, app_id: &str, name: &str) -> Option<Map<String, Value>> {
        const KEYS: &[&str] = &[
            "enter",
            "backspace",
            "left",
            "right",
            "up",
            "down",
            "tab",
            "escape",
        ];
        if self.app_id.as_deref() != Some(app_id) || !KEYS.contains(&name) {
            return None;
        }
        json!({"t": "key", "key": name}).as_object().cloned()
    }

    pub fn panel_changed(
        &mut self,
        app_id: &str,
        covered: i32,
        height: i32,
    ) -> Option<Map<String, Value>> {
        if !app_id.starts_with(APP_ID_PREFIX) {
            return None;
        }
        let panel = (covered.max(0), height.max(0));
        self.panels.insert(app_id.to_owned(), panel);
        if self.app_id.as_deref() == Some(app_id) {
            Some(Self::panel_message(panel))
        } else {
            None
        }
    }

    pub fn current_panel(&self) -> Option<Map<String, Value>> {
        self.app_id
            .as_ref()
            .and_then(|app_id| self.panels.get(app_id))
            .copied()
            .map(Self::panel_message)
    }

    fn panel_message((covered, height): (i32, i32)) -> Map<String, Value> {
        json!({"t": "panel", "covered": covered, "height": height})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }
}

pub struct ShellBus {
    connection: Connection,
}

impl ShellBus {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    fn proxy(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            SHELL_NAME,
            TEXT_INPUT_PATH,
            TEXT_INPUT_INTERFACE,
        )
    }

    pub fn register(&self) -> zbus::Result<()> {
        self.proxy()?.call("Register", &(APP_ID_PREFIX,))
    }

    pub fn call(&self, call: &ShellCall) -> zbus::Result<()> {
        let proxy = self.proxy()?;
        match call {
            ShellCall::SetEditor(app, purpose, hints) => {
                proxy.call("SetEditor", &(app, purpose, hints))
            }
            ShellCall::SetSurroundingText(app, value, cursor, anchor) => {
                proxy.call("SetSurroundingText", &(app, value, cursor, anchor))
            }
            ShellCall::ShowPanel(app) => proxy.call("ShowPanel", &(app,)),
            ShellCall::HidePanel(app) => proxy.call("HidePanel", &(app,)),
            ShellCall::ClearEditor(app) => proxy.call("ClearEditor", &(app,)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "protocol fixtures use known-valid values"
)]
mod tests {
    use super::*;
    use deckard_core::transport;
    use std::sync::{Arc, Mutex};
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
            self.0
                .lock()
                .unwrap()
                .push(ShellCall::SetEditor(app, purpose, hints));
        }

        #[zbus(name = "SetSurroundingText")]
        fn set_surrounding_text(&self, app: String, value: String, cursor: i32, anchor: i32) {
            self.0
                .lock()
                .unwrap()
                .push(ShellCall::SetSurroundingText(app, value, cursor, anchor));
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

    #[test]
    fn fixture_start_state_and_utf16_offsets() {
        let mut ime = ImeState::default();
        assert!(ime.handle_android(&message(json!({"t":"show"}))).is_empty());
        assert!(
            ime.handle_android(&message(json!({"t":"start","package":"../bad"})))
                .is_empty()
        );
        assert_eq!(
            ime.handle_android(&message(
                json!({"t":"start","package":"org.example.App","purpose":99,"hints":2047})
            )),
            vec![ShellCall::SetEditor(
                "waydroid.org.example.App".into(),
                0,
                1023
            )]
        );
        assert_eq!(
            ime.handle_android(&message(
                json!({"t":"state","text":"a😀b","cursor":3,"anchor":1})
            )),
            vec![ShellCall::SetSurroundingText(
                "waydroid.org.example.App".into(),
                "a😀b".into(),
                3,
                1
            )]
        );
        assert_eq!(
            ime.handle_android(&message(
                json!({"t":"state","text":"a😀b","cursor":99,"anchor":-4})
            )),
            vec![ShellCall::SetSurroundingText(
                "waydroid.org.example.App".into(),
                "a😀b".into(),
                4,
                0
            )]
        );
        assert!(
            ime.handle_android(&message(json!({"t":"state","text":"x".repeat(4097)})))
                .is_empty()
        );
    }

    #[test]
    fn fixture_panel_replay_disconnect_and_filtered_shell_input() {
        let mut ime = ImeState::default();
        assert!(ime.panel_changed("waydroid.org.example", -4, 700).is_none());
        ime.handle_android(&message(json!({"t":"start","package":"org.example"})));
        assert_eq!(
            ime.current_panel(),
            Some(message(json!({"t":"panel","covered":0,"height":700})))
        );
        assert_eq!(
            ime.key("waydroid.org.example", "enter"),
            Some(message(json!({"t":"key","key":"enter"})))
        );
        assert!(ime.key("waydroid.org.other", "enter").is_none());
        assert!(ime.key("waydroid.org.example", "delete").is_none());
        assert_eq!(
            ime.commit_string("waydroid.org.example", "😀", -3, 2),
            Some(message(
                json!({"t":"commit","text":"😀","before":0,"after":2})
            ))
        );
        assert!(ime.commit_string("waydroid.org.other", "x", 0, 0).is_none());
        assert_eq!(
            ime.handle_android(&message(json!({"t":"show"}))),
            vec![ShellCall::ShowPanel("waydroid.org.example".into())]
        );
        assert_eq!(
            ime.handle_android(&message(json!({"t":"hide"}))),
            vec![ShellCall::HidePanel("waydroid.org.example".into())]
        );
        assert_eq!(
            ime.handle_android(&message(json!({"t":"finish"}))),
            vec![ShellCall::ClearEditor("waydroid.org.example".into())]
        );
        assert!(ime.disconnect().is_empty());
        ime.handle_android(&message(json!({"t":"start","package":"org.example"})));
        assert_eq!(
            ime.disconnect(),
            vec![
                ShellCall::HidePanel("waydroid.org.example".into()),
                ShellCall::ClearEditor("waydroid.org.example".into())
            ]
        );
        assert!(ime.disconnect().is_empty());
    }

    #[test]
    fn framed_fixture_round_trip() {
        let message = message(json!({"t":"commit","text":"😀","before":2,"after":0}));
        let frame = transport::encode(&message).unwrap();
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        assert_eq!(
            serde_json::from_slice::<Map<String, Value>>(&frame[4..]).unwrap(),
            message
        );
    }

    #[test]
    fn private_bus_matches_shell_method_signatures() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("DECKARD_IME_PRIVATE_BUS").is_none() {
            return Ok(());
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let owner = Connection::session()?;
        owner
            .object_server()
            .at(TEXT_INPUT_PATH, MockShell(Arc::clone(&calls)))?;
        owner.request_name(SHELL_NAME)?;
        let bus = ShellBus::new(Connection::session()?);
        bus.register()?;
        let expected = vec![
            ShellCall::SetEditor("waydroid.org.example".into(), 7, 0x200),
            ShellCall::SetSurroundingText("waydroid.org.example".into(), "a😀b".into(), 3, 1),
            ShellCall::ShowPanel("waydroid.org.example".into()),
            ShellCall::HidePanel("waydroid.org.example".into()),
            ShellCall::ClearEditor("waydroid.org.example".into()),
        ];
        for call in &expected {
            bus.call(call)?;
        }
        assert_eq!(*calls.lock().unwrap(), expected);
        Ok(())
    }
}
