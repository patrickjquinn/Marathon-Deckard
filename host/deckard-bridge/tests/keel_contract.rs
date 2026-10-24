#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "private-bus tests drive fixed protocol fixtures"
)]

//! Android apps as Keel Actions providers, end to end on a private bus.
//! Run with: dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1
//! cargo test -p deckard-bridge --test keel_contract

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deckard_bridge::keel::KeelApps;
use deckard_packages::{BridgeRoute, BridgeSend};
use serde_json::{Map, Value, json};
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, Proxy};
use zbus::interface;
use zbus::zvariant::Value as Variant;

const NAME: &str = "deckard.org.example.chat";
const PATH: &str = "/org/shipwright/Keel/Actions";
const ERROR: &str = "org.shipwright.Keel.Actions.Error.";

#[derive(Default)]
struct ShellState {
    locked: bool,
    front: String,
    running: Vec<String>,
    launched: Vec<String>,
    expected: Vec<String>,
    secure: Vec<(String, bool)>,
}

type Shared = Arc<Mutex<ShellState>>;

/// `org.marathonos.shell`'s Keel Actions, as the bridge uses them.
struct FakeShell(Shared);

#[interface(name = "org.shipwright.Keel.Actions")]
impl FakeShell {
    fn invoke(
        &self,
        action: String,
        arguments: String,
        _options: HashMap<String, zbus::zvariant::OwnedValue>,
    ) -> String {
        let args: Value = serde_json::from_str(&arguments).unwrap();
        let mut shell = self.0.lock().unwrap();
        match action.as_str() {
            "shell.state" => {
                json!({"locked": shell.locked, "screenOn": true, "foregroundApp": shell.front})
                    .to_string()
            }
            "apps.running" => {
                let list: Vec<Value> = shell
                    .running
                    .iter()
                    .map(|id| json!({"id": id, "name": id}))
                    .collect();
                json!({"result": list}).to_string()
            }
            "app.launch" => {
                let id = args["id"].as_str().unwrap().to_owned();
                shell.front = id.clone();
                shell.launched.push(id);
                "{}".into()
            }
            _ => String::new(),
        }
    }
}

/// The shell's `Navigation1`, the part the bridge calls.
struct FakeNavigation(Shared);

#[interface(name = "org.marathonos.Shell.Navigation1")]
impl FakeNavigation {
    fn expect_app_window(&self, app_id: String) -> bool {
        self.0.lock().unwrap().expected.push(app_id);
        true
    }

    fn set_app_window_secure(&self, app_id: String, secure: bool) {
        self.0.lock().unwrap().secure.push((app_id, secure));
    }
}

type Sent = Arc<Mutex<Vec<(BridgeRoute, Map<String, Value>)>>>;

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The first message the bridge sent to Android on `route`, once there is one.
fn take(sent: &Sent, route: BridgeRoute) -> Map<String, Value> {
    let mut found = None;
    wait("a message to Android", || {
        let mut sent = sent.lock().unwrap();
        let at = sent.iter().position(|(to, _)| *to == route);
        found = at.map(|at| sent.remove(at).1);
        found.is_some()
    });
    found.unwrap()
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

/// Android's answer to `request`.
fn answer(keel: &KeelApps, request: &Map<String, Value>, reply: Value) {
    let mut reply = object(reply);
    reply.insert("req".into(), request["req"].clone());
    if request["t"] == "keel" {
        reply.insert("t".into(), json!("keel_done"));
        keel.handle(BridgeRoute::System, &reply);
    } else {
        keel.handle(BridgeRoute::User, &reply);
    }
}

fn start(sent: &Sent, trusted: PathBuf, state: PathBuf) -> KeelApps {
    let sent = Arc::clone(sent);
    let send: Arc<BridgeSend> = Arc::new(move |route, value: &Value| {
        sent.lock().unwrap().push((route, object(value.clone())));
        true
    });
    let bus = Connection::session().unwrap().inner().clone();
    KeelApps::start(bus, send, vec![trusted], state).unwrap()
}

fn call(
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Result<String, String> {
    let client = Connection::session().unwrap();
    let proxy = Proxy::new(&client, NAME, PATH, "org.shipwright.Keel.Actions").unwrap();
    match proxy.call_method(method, body) {
        Ok(reply) => Ok(reply.body().deserialize::<String>().unwrap()),
        Err(zbus::Error::MethodError(name, _, _)) => Err(name.as_str().to_owned()),
        Err(error) => panic!("{method}: {error}"),
    }
}

fn invoke(action: &str, args: &str, pilot: bool) -> Result<String, String> {
    let mut options: HashMap<&str, Variant<'_>> = HashMap::new();
    if pilot {
        options.insert("userInitiated", Variant::Bool(true));
    }
    call("Invoke", &(action, args, options))
}

fn invoke_later(
    action: &'static str,
    args: &'static str,
    pilot: bool,
) -> JoinHandle<Result<String, String>> {
    thread::spawn(move || invoke(action, args, pilot))
}

fn error(code: &str) -> Result<String, String> {
    Err(format!("{ERROR}{code}"))
}

fn tool<'a>(manifest: &'a Value, action: &str) -> Option<&'a Value> {
    let tools = manifest["tools"].as_array()?;
    tools
        .iter()
        .find(|tool| tool["_meta"]["org.shipwright.keel/action"] == action)
}

fn sample() -> Map<String, Value> {
    object(json!({
        "t": "app_info", "pkg": "org.example.chat", "label": "Chat App",
        "links": [{"s": "https", "h": "chat.example.org"}], "share": true, "search": "gms",
        "shortcuts": [{"id": "wipe_all", "label": "Delete all chats"}]
    }))
}

#[test]
fn android_apps_serve_keel_actions() {
    if env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return;
    }
    let dbus = DBusProxy::new(&Connection::session().unwrap()).unwrap();
    let owner = || {
        dbus.get_name_owner(NAME.try_into().unwrap())
            .ok()
            .map(|owner| owner.to_string())
    };
    let shell_state = Shared::default();
    let shell = Connection::session().unwrap();
    shell
        .object_server()
        .at(PATH, FakeShell(Arc::clone(&shell_state)))
        .unwrap();
    shell.request_name("org.marathonos.shell").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state/apps.json");
    let sent = Sent::default();
    let keel = start(&sent, env::current_exe().unwrap(), state.clone());
    keel.set_connected(BridgeRoute::User, true);
    keel.set_connected(BridgeRoute::System, true);
    keel.handle(BridgeRoute::User, &sample());
    wait("the app's name", || owner().is_some());

    // Shortcuts come from DeckardSystem, and a new list is a new owner.
    let shortcuts = take(&sent, BridgeRoute::System);
    assert_eq!(shortcuts["op"], "shortcuts");
    assert_eq!(shortcuts["pkg"], "org.example.chat");
    let first = owner();
    answer(
        &keel,
        &shortcuts,
        json!({"ok": true, "shortcuts": [
            {"id": "compose", "label": "New message"},
            {"id": "anna", "label": "Anna", "long": "Chat with Anna"},
        ]}),
    );
    wait("a new owner", || {
        owner().is_some_and(|now| Some(now) != first)
    });

    // keel-mcp's probe: Describe answers the manifest of the name asked.
    let described: Value = serde_json::from_str(&call("Describe", &()).unwrap()).unwrap();
    assert_eq!(described["appId"], NAME);
    assert!(tool(&described, "shortcut.anna").is_some());
    assert!(tool(&described, "shortcut.wipe_all").is_none());

    // app.show without a link is the shell's launch.
    assert_eq!(invoke("app.show", "{}", false), Ok("{}".into()));
    assert_eq!(shell_state.lock().unwrap().launched, [NAME]);
    assert_eq!(
        invoke("app.show", r#"{"bogus":"x"}"#, false),
        error("InvalidArguments")
    );
    assert_eq!(
        invoke("app.show", r#"{"link":"file:///etc/passwd"}"#, false),
        error("InvalidArguments")
    );
    assert_eq!(invoke("nope.nope", "{}", false), error("UnknownAction"));

    // A shortcut: DeckardSystem starts it; with a shell that does not expect
    // windows, the bridge waits for the window and shows it.
    shell_state.lock().unwrap().launched.clear();
    let shortcut = invoke_later("shortcut.anna", "{}", false);
    let start_shortcut = take(&sent, BridgeRoute::System);
    assert_eq!(start_shortcut["op"], "start");
    assert_eq!(start_shortcut["id"], "anna");
    assert!(!shortcut.is_finished());
    shell_state.lock().unwrap().running = vec![NAME.into()];
    answer(&keel, &start_shortcut, json!({"ok": true}));
    assert_eq!(shortcut.join().unwrap(), Ok("{}".into()));
    assert_eq!(shell_state.lock().unwrap().launched, [NAME]);

    // Android refusing to start it is the call's failure.
    let share = invoke_later("share.text", r#"{"text":"hi"}"#, false);
    let send = take(&sent, BridgeRoute::User);
    assert_eq!(send["kind"], "send");
    assert_eq!(send["text"], "hi");
    answer(
        &keel,
        &send,
        json!({"t": "launched", "ok": false, "error": "no activity"}),
    );
    assert_eq!(share.join().unwrap(), error("Failed"));

    // A call in flight when the manifest changes still gets its answer.
    let share = invoke_later("share.text", r#"{"text":"hi"}"#, false);
    let send = take(&sent, BridgeRoute::User);
    let changed = object(json!({"t": "keel_changed", "pkg": "org.example.chat"}));
    keel.handle(BridgeRoute::System, &changed);
    let before = owner();
    answer(
        &keel,
        &take(&sent, BridgeRoute::System),
        json!({"ok": true, "shortcuts": [{"id": "anna", "label": "Anna", "long": "Chat with Anna"}]}),
    );
    wait("a new owner", || {
        owner().is_some_and(|now| Some(now) != before)
    });
    answer(&keel, &send, json!({"t": "launched", "ok": true}));
    assert_eq!(share.join().unwrap(), Ok("{}".into()));

    // A shell that expects the window shows it itself.
    shell
        .object_server()
        .at(
            "/org/marathonos/Shell/Navigation",
            FakeNavigation(Arc::clone(&shell_state)),
        )
        .unwrap();
    shell.request_name("org.marathonos.Shell").unwrap();
    shell_state.lock().unwrap().launched.clear();
    let search = invoke_later("app.search", r#"{"query":"anna"}"#, false);
    let query = take(&sent, BridgeRoute::User);
    assert_eq!(shell_state.lock().unwrap().expected, [NAME]);
    assert_eq!(query["kind"], "search");
    assert_eq!(query["mode"], "gms");
    answer(&keel, &query, json!({"t": "launched", "ok": true}));
    assert_eq!(search.join().unwrap(), Ok("{}".into()));
    assert!(shell_state.lock().unwrap().launched.is_empty());

    // The screen: only of the app in front, never of a secure window.
    shell_state.lock().unwrap().front = "browser".into();
    assert_eq!(invoke("screen.read", "{}", true), error("NotAvailable"));
    shell_state.lock().unwrap().front = NAME.into();
    let secure = invoke_later("screen.read", "{}", false);
    let check = take(&sent, BridgeRoute::System);
    assert_eq!(check["op"], "secure");
    answer(&keel, &check, json!({"ok": true, "secure": true}));
    assert_eq!(secure.join().unwrap(), error("NotAvailable"));
    assert!(sent.lock().unwrap().is_empty());

    let read = invoke_later("screen.read", "{}", false);
    answer(
        &keel,
        &take(&sent, BridgeRoute::System),
        json!({"ok": true, "secure": false}),
    );
    let ask = take(&sent, BridgeRoute::User);
    assert_eq!(ask["t"], "screen_read");
    assert_eq!(ask["pkg"], "org.example.chat");
    answer(
        &keel,
        &ask,
        json!({"t": "screen", "ok": true, "pkg": "org.example.chat", "snap": 7,
        "nodes": [{"i": 2, "c": "android.widget.Button", "t": "Send", "f": 1}]}),
    );
    let result: Value = serde_json::from_str(&read.join().unwrap().unwrap()).unwrap();
    assert_eq!(result["screen"], "[7.2] button \"Send\"\n");
    assert_eq!(result["snapshot"], 7);
    // The shell heard what DeckardSystem said, each change once.
    wait("secure reports", || {
        shell_state.lock().unwrap().secure.len() == 2
    });
    assert_eq!(
        shell_state.lock().unwrap().secure,
        [(NAME.into(), true), (NAME.into(), false)]
    );

    // An Android app coming to the front is checked for the shell.
    let navigation = "org.marathonos.Shell.Navigation1";
    let path = "/org/marathonos/Shell/Navigation";
    shell
        .emit_signal(
            None::<&str>,
            path,
            navigation,
            "ForegroundAppChanged",
            &(NAME,),
        )
        .unwrap();
    let check = take(&sent, BridgeRoute::System);
    assert_eq!(check["op"], "secure");
    answer(&keel, &check, json!({"ok": true, "secure": true}));
    wait("a third secure report", || {
        shell_state.lock().unwrap().secure.len() == 3
    });
    shell
        .emit_signal(
            None::<&str>,
            path,
            navigation,
            "ForegroundAppChanged",
            &("browser",),
        )
        .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(sent.lock().unwrap().is_empty());

    // A tap: Pilot only, on an id of the latest read.
    assert_eq!(
        invoke("ui.tap", r#"{"node":"7.2"}"#, false),
        error("NotAvailable")
    );
    assert_eq!(
        invoke("ui.tap", r#"{"node":"6.2"}"#, true),
        error("InvalidArguments")
    );
    let tap = invoke_later("ui.tap", r#"{"node":"7.2"}"#, true);
    let act = take(&sent, BridgeRoute::User);
    assert_eq!(act["t"], "ui_act");
    assert_eq!(act["act"], "click");
    assert_eq!(act["node"], 2);
    assert_eq!(act["snap"], 7);
    answer(&keel, &act, json!({"t": "ui_done", "ok": true}));
    assert_eq!(tap.join().unwrap(), Ok("{}".into()));

    // Keel's context: the same screen, bounded.
    let context = thread::spawn(|| call("GetContext", &()));
    answer(
        &keel,
        &take(&sent, BridgeRoute::System),
        json!({"ok": true, "secure": false}),
    );
    let ask = take(&sent, BridgeRoute::User);
    answer(
        &keel,
        &ask,
        json!({"t": "screen", "ok": true, "pkg": "org.example.chat", "snap": 8, "nodes": []}),
    );
    let context: Value = serde_json::from_str(&context.join().unwrap().unwrap()).unwrap();
    assert_eq!(context["app"], NAME);

    // Without DeckardSystem nobody can say the screen is safe to read.
    keel.set_connected(BridgeRoute::System, false);
    assert_eq!(invoke("screen.read", "{}", false), error("NotAvailable"));
    keel.set_connected(BridgeRoute::System, true);
    let refresh = take(&sent, BridgeRoute::System);
    assert_eq!(refresh["op"], "shortcuts");
    answer(&keel, &refresh, json!({"ok": false}));
    keel.handle(
        BridgeRoute::System,
        &object(json!({"t": "keel_changed", "pkg": "org.example.chat"})),
    );
    let changed = take(&sent, BridgeRoute::System);
    assert_eq!(changed["op"], "shortcuts");
    answer(&keel, &changed, json!({"ok": false}));

    // Locked: nothing runs.
    shell_state.lock().unwrap().locked = true;
    assert_eq!(
        invoke("app.search", r#"{"query":"x"}"#, false),
        error("NotAvailable")
    );
    shell_state.lock().unwrap().locked = false;

    // Android going away fails what it was asked.
    let lost = invoke_later("screen.read", "{}", false);
    answer(
        &keel,
        &take(&sent, BridgeRoute::System),
        json!({"ok": true, "secure": false}),
    );
    let _ = take(&sent, BridgeRoute::User);
    keel.set_connected(BridgeRoute::User, false);
    assert_eq!(lost.join().unwrap(), error("NotAvailable"));
    keel.set_connected(BridgeRoute::User, true);

    // An unchanged manifest keeps its owner.
    let kept = owner();
    keel.handle(BridgeRoute::User, &sample());
    let _ = take(&sent, BridgeRoute::System);
    assert_eq!(owner(), kept);

    // The catalogue survives a restart, and only keel-mcp and Pilot are served.
    keel.set_connected(BridgeRoute::System, false);
    drop(keel);
    wait("the name released", || owner().is_none());
    let keel = start(&sent, PathBuf::from("/nonexistent/keel-mcp"), state);
    wait("the restored name", || owner().is_some());
    let restored: Value = serde_json::from_str(&call("Describe", &()).unwrap()).unwrap();
    assert_eq!(restored["appId"], NAME);
    assert!(tool(&restored, "shortcut.anna").is_some());
    assert_eq!(
        invoke("app.show", "{}", false),
        Err("org.freedesktop.DBus.Error.AccessDenied".into())
    );

    keel.handle(
        BridgeRoute::User,
        &object(json!({"t": "app_list", "pkgs": []})),
    );
    wait("the name withdrawn", || owner().is_none());
}
