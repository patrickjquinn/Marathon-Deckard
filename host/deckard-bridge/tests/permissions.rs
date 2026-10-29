#![allow(
    clippy::unwrap_used,
    reason = "private-bus tests drive fixed protocol fixtures"
)]

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deckard_bridge::api::{BUS_NAME, OBJECT_PATH, SettingsApi};
use deckard_bridge::contract::{self, VariantMap};
use deckard_bridge::manage::Manage;
use deckard_bridge::prompts::{MAX_WAITING, PermissionPrompts};
use deckard_bridge::runtime::Runtime;
use serde_json::{Map, Value, json};
use zbus::interface;
use zbus::{Connection, Proxy};
use zbus::{OwnedVariant as OwnedValue, Str};

const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(200);

/// A prompt the fake Shell holds: app id, permission, options, and where its
/// answer goes.
type Prompt = (String, String, Value, async_channel::Sender<(String, bool)>);

/// The Shell's Permissions1: holds each prompt until the test answers it.
struct Permissions(mpsc::Sender<Prompt>, mpsc::Sender<String>);

#[interface(name = "org.marathonos.Shell.Permissions1")]
impl Permissions {
    #[zbus(name = "PromptAndroid")]
    async fn prompt_android(
        &self,
        app: String,
        permission: String,
        options: VariantMap,
    ) -> VariantMap {
        let options = options.iter().map(|(key, value)| {
            let text = value.downcast_ref::<&str>().map(Value::from);
            (
                key.clone(),
                text.or_else(|_| value.downcast_ref::<bool>().map(Value::from))
                    .unwrap(),
            )
        });
        let (answer, answered) = async_channel::bounded(1);
        self.0
            .send((app, permission, options.collect(), answer))
            .unwrap();
        let (answer, precise) = answered.recv().await.unwrap();
        VariantMap::from([
            ("answer".to_owned(), OwnedValue::from(Str::from(answer))),
            ("precise".to_owned(), precise.into()),
        ])
    }

    #[zbus(name = "CancelAndroidPrompt")]
    fn cancel_android_prompt(&self, request: String) {
        self.1.send(request).unwrap();
    }
}

struct Navigation;

#[interface(name = "org.marathonos.Shell.Navigation1")]
impl Navigation {
    #[zbus(name = "AppIdForPid")]
    fn app_id_for_pid(&self, _pid: u32) -> String {
        "settings".into()
    }
}

fn ask(id: &str, perms: &[&str], extra: Value) -> Map<String, Value> {
    let perms: Vec<String> = perms
        .iter()
        .map(|p| format!("android.permission.{p}"))
        .collect();
    let mut ask = json!({"t":"perm_ask", "id":id, "pkg":"me.hackerchick.catima",
        "perms":perms, "kind":"foreground"});
    for (key, value) in extra.as_object().unwrap() {
        ask[key] = value.clone();
    }
    ask.as_object().unwrap().clone()
}

fn answer(prompt: &Prompt, answer: &str, precise: bool) {
    prompt.3.send_blocking((answer.into(), precise)).unwrap();
}

fn reply(sent: &Receiver<Map<String, Value>>) -> Value {
    Value::Object(sent.recv_timeout(WAIT).unwrap())
}

#[test]
fn android_permissions_have_one_marathon_name() {
    let name = |p: &str| contract::marathon_permission(&format!("android.permission.{p}"));
    assert_eq!(name("READ_MEDIA_VIDEO"), Some("storage"));
    assert_eq!(name("NEARBY_WIFI_DEVICES"), Some("bluetooth"));
    assert_eq!(name("ACCESS_BACKGROUND_LOCATION"), Some("location"));
    assert_eq!(name("INSTALL_PACKAGES"), None);
    assert_eq!(contract::marathon_permission("CAMERA"), None);
}

// Run with: dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --test permissions
#[test]
fn cxx_permission_prompts_and_settings() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let bus = zbus::block_on(Connection::session())?;
    let (tx, sent) = mpsc::channel();
    let tx = Mutex::new(tx);
    let send = Arc::new(move |message| tx.lock().unwrap().send(message).is_ok());
    let prompter = PermissionPrompts::new(bus.clone(), send);
    let unsupported =
        |id| json!({"t":"perm_answer", "id":id, "answer":"unsupported", "precise":false});

    // Without the Shell, Android asks.
    prompter.handle(&ask("0000000000000010", &["RECORD_AUDIO"], json!({})));
    assert_eq!(reply(&sent), unsupported("0000000000000010"));

    let shell = zbus::block_on(Connection::session())?;
    let (shown_tx, shown) = mpsc::channel();
    let (cancel_tx, cancelled) = mpsc::channel();
    let server = shell.object_server();
    zbus::block_on(server.at(
        "/org/marathonos/Shell/Permissions",
        Permissions(shown_tx, cancel_tx),
    ))?;
    zbus::block_on(server.at("/org/marathonos/Shell/Navigation", Navigation))?;
    zbus::block_on(shell.request_name("org.marathonos.Shell"))?;

    // A request becomes the Shell's prompt.
    prompter.handle(&ask("0123456789abcdef", &["CAMERA"], json!({"once":true})));
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(
        (prompt.0.as_str(), prompt.1.as_str()),
        ("deckard.me.hackerchick.catima", "camera")
    );
    let options = json!({"request":"0123456789abcdef", "kind":"foreground", "once":true,
        "accuracy":"", "preciseDefault":false});
    assert_eq!(prompt.2, options);
    assert!(sent.try_recv().is_err());
    answer(&prompt, "once", false);
    let once =
        json!({"t":"perm_answer", "id":"0123456789abcdef", "answer":"once", "precise":false});
    assert_eq!(reply(&sent), once);

    // Location keeps its choices.
    let perms = ["ACCESS_FINE_LOCATION", "ACCESS_COARSE_LOCATION"];
    let choices = json!({"once":true, "accuracy":"choice", "preciseDefault":true});
    prompter.handle(&ask("00000000000000aa", &perms, choices));
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(prompt.1, "location");
    assert_eq!(
        (&prompt.2["accuracy"], &prompt.2["preciseDefault"]),
        (&json!("choice"), &json!(true))
    );
    answer(&prompt, "allow", true);
    assert_eq!(reply(&sent)["precise"], true);
    let background = json!({"kind":"background"});
    prompter.handle(&ask(
        "00000000000000ab",
        &["ACCESS_BACKGROUND_LOCATION"],
        background,
    ));
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(prompt.2["kind"], "background");
    answer(&prompt, "sideways", false);
    assert_eq!(reply(&sent)["answer"], "cancel");

    // Requests Marathon can't draw go back to Android's own dialog.
    let bad = [
        ask(
            "0000000000000001",
            &[],
            json!({"perms":["com.example.permission.CUSTOM"]}),
        ),
        ask("0000000000000002", &["CAMERA", "READ_CONTACTS"], json!({})),
        ask("0000000000000003", &[], json!({})),
        ask("0000000000000004", &["CAMERA"], json!({"kind":"always"})),
        ask("0000000000000005", &["CAMERA"], json!({"accuracy":"fine"})),
        ask("0000000000000006", &["CAMERA"], json!({"pkg":"bad pkg"})),
        ask("0000000000000007", &["CAMERA"], json!({"once":"yes"})),
    ];
    for message in &bad {
        prompter.handle(message);
        assert_eq!(reply(&sent), unsupported(message["id"].as_str().unwrap()));
    }
    prompter.handle(&ask("not-an-id", &["CAMERA"], json!({})));

    // Cancels reach the Shell, and no answer follows.
    prompter.handle(&ask("0000000000000020", &["CAMERA"], json!({})));
    prompter.handle(&ask("0000000000000021", &["READ_CALENDAR"], json!({})));
    let (first, second) = (shown.recv_timeout(WAIT)?, shown.recv_timeout(WAIT)?);
    for id in ["0000000000000020", "00000000000000ff"] {
        prompter.handle(json!({"t":"perm_cancel", "id":id}).as_object().unwrap());
    }
    assert_eq!(cancelled.recv_timeout(WAIT)?, "0000000000000020");
    answer(&first, "allow", false);
    prompter.system_lost();
    assert_eq!(cancelled.recv_timeout(WAIT)?, "0000000000000021");
    answer(&second, "deny", false);
    assert!(sent.recv_timeout(QUIET).is_err());

    // Waiting is bounded, and a repeated id is the same request.
    for i in 0..=MAX_WAITING {
        prompter.handle(&ask(
            &format!("00000000000001{i:02x}"),
            &["CAMERA"],
            json!({}),
        ));
    }
    assert_eq!(reply(&sent), unsupported("0000000000000108"));
    let held: Vec<Prompt> = (0..MAX_WAITING)
        .flat_map(|_| shown.recv_timeout(WAIT))
        .collect();
    prompter.handle(&ask("0000000000000100", &["CAMERA"], json!({})));
    prompter.system_lost();
    held.iter()
        .for_each(|prompt| answer(prompt, "cancel", false));
    assert_eq!(held.len(), MAX_WAITING);
    assert!(sent.recv_timeout(QUIET).is_err() && shown.try_recv().is_err());

    // Settings: arguments are checked before anything reaches Android, then
    // Marathon's names go in and Android's permissions come out.
    let manage = Manage::default();
    let dir = tempfile::tempdir()?;
    let runtime = Runtime::new(
        dir.path().join("s.json"),
        dir.path().join("a"),
        dir.path().join("m"),
    );
    let api_bus = zbus::block_on(Connection::session())?;
    let icon_dirs = vec![];
    let api = SettingsApi {
        bus: api_bus.clone(),
        runtime,
        manage: manage.clone(),
        icon_dirs,
    };
    zbus::block_on(api_bus.object_server().at(OBJECT_PATH, api))?;
    zbus::block_on(api_bus.request_name(BUS_NAME))?;
    let (tx, android) = async_channel::bounded(4);
    let (generation, replier) = (manage.connect(tx), manage.clone());
    let responder = std::thread::spawn(move || {
        let mut ops = Vec::new();
        while let Ok(mut sent) = android.recv_blocking() {
            let apps = json!([{"pkg":"org.fdroid.fdroid", "label":"F-Droid", "granted":true, "fixed":true}]);
            let reply = json!({"t":"managed", "id":sent.remove("id"), "ok":true, "apps":apps});
            replier.handle(generation, reply.as_object().unwrap());
            ops.push(Value::Object(sent));
        }
        ops
    });
    let settings = zbus::block_on(Proxy::new(
        &bus,
        BUS_NAME,
        OBJECT_PATH,
        "org.marathonos.Deckard1",
    ))?;
    let set = |pkg: &str, name: &str, on: bool| {
        zbus::block_on(settings.call::<_, _, ()>("SetPermission", &(pkg, name, on)))
    };
    assert!(
        set(
            "org.fdroid.fdroid",
            "android.permission.INSTALL_PACKAGES",
            true
        )
        .is_err()
    );
    assert!(set("org.fdroid.fdroid", "nearby", true).is_err());
    assert!(set("not a package", "camera", true).is_err());
    set("org.fdroid.fdroid", "bluetooth", false)?;
    set("org.fdroid.fdroid", "notifications", true)?;
    let apps = |name: &str| {
        zbus::block_on(settings.call::<_, _, Vec<VariantMap>>("GetPermissionApps", &(name,)))
    };
    let camera = apps("camera")?;
    assert_eq!(
        String::try_from(camera[0]["label"].try_clone()?)?,
        "F-Droid"
    );
    assert!(bool::try_from(camera[0]["fixed"].try_clone()?)?);
    apps("notifications")?;
    assert!(apps("system").is_err());
    manage.disconnect(generation);
    let bluetooth = contract::permission("bluetooth").unwrap();
    let expected = json!([
        {"t":"manage", "op":"perm", "pkg":"org.fdroid.fdroid", "permissions":bluetooth, "granted":false},
        {"t":"manage", "op":"notifications", "pkg":"org.fdroid.fdroid", "enabled":true},
        {"t":"manage", "op":"perm_apps", "permissions":["android.permission.CAMERA"]},
        {"t":"manage", "op":"notif_apps"},
    ]);
    assert_eq!(Value::Array(responder.join().unwrap()), expected);
    zbus::block_on(api_bus.release_name(BUS_NAME))?;
    zbus::block_on(shell.release_name("org.marathonos.Shell"))?;
    Ok(())
}
