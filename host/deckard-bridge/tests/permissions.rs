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
use zbus::blocking::{Connection, Proxy};
use zbus::interface;
use zbus::zvariant::{OwnedValue, Str};

const WAIT: Duration = Duration::from_secs(5);

type Answer = async_channel::Sender<(String, bool)>;

struct Prompt {
    app_id: String,
    permission: String,
    options: VariantMap,
    answer: Answer,
}

/// The Shell's Permissions1: holds each prompt until the test answers it.
struct Permissions {
    prompts: mpsc::Sender<Prompt>,
    cancelled: mpsc::Sender<String>,
}

#[interface(name = "org.marathonos.Shell.Permissions1")]
impl Permissions {
    #[zbus(name = "PromptAndroid")]
    async fn prompt_android(
        &self,
        app_id: String,
        permission: String,
        options: VariantMap,
    ) -> VariantMap {
        let (answer, answered) = async_channel::bounded(1);
        let prompt = Prompt {
            app_id,
            permission,
            options,
            answer,
        };
        self.prompts.send(prompt).unwrap();
        let (answer, precise) = answered.recv().await.unwrap_or(("cancel".into(), false));
        VariantMap::from([
            ("answer".to_owned(), OwnedValue::from(Str::from(answer))),
            ("precise".to_owned(), precise.into()),
        ])
    }

    #[zbus(name = "CancelAndroidPrompt")]
    fn cancel_android_prompt(&self, request: String) {
        self.cancelled.send(request).unwrap();
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

fn option(prompt: &Prompt, key: &str) -> OwnedValue {
    prompt.options[key].try_clone().unwrap()
}

fn text(prompt: &Prompt, key: &str) -> String {
    String::try_from(option(prompt, key)).unwrap()
}

fn flag(prompt: &Prompt, key: &str) -> bool {
    bool::try_from(option(prompt, key)).unwrap()
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

fn prompts(bus: &Connection) -> (PermissionPrompts, Receiver<Map<String, Value>>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let send = Arc::new(move |message| tx.lock().unwrap().send(message).is_ok());
    (PermissionPrompts::new(bus.inner().clone(), send), rx)
}

fn answer(prompt: &Prompt, answer: &str, precise: bool) {
    prompt
        .answer
        .send_blocking((answer.into(), precise))
        .unwrap();
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
    let bus = Connection::session()?;

    // Without the Shell, Android asks.
    let (prompter, sent) = prompts(&bus);
    prompter.handle(&ask("0000000000000010", &["RECORD_AUDIO"], json!({})));
    assert_eq!(sent.recv_timeout(WAIT)?["answer"], "unsupported");

    let shell = Connection::session()?;
    let (prompt_tx, shown) = mpsc::channel();
    let (cancel_tx, cancelled) = mpsc::channel();
    shell.object_server().at(
        "/org/marathonos/Shell/Permissions",
        Permissions {
            prompts: prompt_tx,
            cancelled: cancel_tx,
        },
    )?;
    shell
        .object_server()
        .at("/org/marathonos/Shell/Navigation", Navigation)?;
    shell.request_name("org.marathonos.Shell")?;

    // A request becomes the Shell's prompt.
    prompter.handle(&ask("0123456789abcdef", &["CAMERA"], json!({"once":true})));
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(prompt.app_id, "waydroid.me.hackerchick.catima");
    assert_eq!(prompt.permission, "camera");
    assert_eq!(text(&prompt, "request"), "0123456789abcdef");
    assert_eq!(text(&prompt, "kind"), "foreground");
    assert!(flag(&prompt, "once"));
    assert_eq!(text(&prompt, "accuracy"), "");
    assert!(sent.try_recv().is_err());
    answer(&prompt, "once", false);
    let reply = sent.recv_timeout(WAIT)?;
    assert_eq!(
        Value::Object(reply),
        json!({"t":"perm_answer", "id":"0123456789abcdef", "answer":"once", "precise":false})
    );

    // Location keeps its choices.
    let fine = ask(
        "00000000000000aa",
        &["ACCESS_FINE_LOCATION", "ACCESS_COARSE_LOCATION"],
        json!({"once":true, "accuracy":"choice", "preciseDefault":true}),
    );
    prompter.handle(&fine);
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(prompt.permission, "location");
    assert_eq!(text(&prompt, "accuracy"), "choice");
    assert!(flag(&prompt, "preciseDefault"));
    answer(&prompt, "allow", true);
    let reply = sent.recv_timeout(WAIT)?;
    assert_eq!(
        (&reply["answer"], &reply["precise"]),
        (&json!("allow"), &json!(true))
    );
    let background = json!({"kind":"background"});
    prompter.handle(&ask(
        "00000000000000ab",
        &["ACCESS_BACKGROUND_LOCATION"],
        background,
    ));
    let prompt = shown.recv_timeout(WAIT)?;
    assert_eq!(text(&prompt, "kind"), "background");
    answer(&prompt, "sideways", false);
    assert_eq!(sent.recv_timeout(WAIT)?["answer"], "cancel");

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
        let reply = sent.recv_timeout(WAIT)?;
        assert_eq!(
            (&reply["id"], &reply["answer"]),
            (&message["id"], &json!("unsupported"))
        );
    }
    prompter.handle(&ask("not-an-id", &["CAMERA"], json!({})));

    // Cancels reach the Shell, and no answer follows.
    prompter.handle(&ask("0000000000000020", &["CAMERA"], json!({})));
    prompter.handle(&ask("0000000000000021", &["READ_CALENDAR"], json!({})));
    let first = shown.recv_timeout(WAIT)?;
    let second = shown.recv_timeout(WAIT)?;
    prompter.handle(
        &json!({"t":"perm_cancel", "id":"0000000000000020"})
            .as_object()
            .unwrap()
            .clone(),
    );
    prompter.handle(
        &json!({"t":"perm_cancel", "id":"00000000000000ff"})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(cancelled.recv_timeout(WAIT)?, "0000000000000020");
    answer(&first, "allow", false);
    prompter.system_lost();
    assert_eq!(cancelled.recv_timeout(WAIT)?, "0000000000000021");
    answer(&second, "deny", false);
    assert!(sent.recv_timeout(Duration::from_millis(200)).is_err());

    // Waiting is bounded, and a repeated id is the same request.
    for i in 0..=MAX_WAITING {
        prompter.handle(&ask(
            &format!("00000000000001{i:02x}"),
            &["CAMERA"],
            json!({}),
        ));
    }
    let reply = sent.recv_timeout(WAIT)?;
    assert_eq!(
        (&reply["id"], &reply["answer"]),
        (&json!("0000000000000108"), &json!("unsupported"))
    );
    let held: Vec<Prompt> = (0..MAX_WAITING)
        .map(|_| shown.recv_timeout(WAIT).unwrap())
        .collect();
    prompter.handle(&ask("0000000000000100", &["CAMERA"], json!({})));
    assert!(sent.recv_timeout(Duration::from_millis(200)).is_err());
    prompter.system_lost();
    for prompt in &held {
        answer(prompt, "cancel", false);
    }
    assert!(shown.recv_timeout(Duration::from_millis(200)).is_err());

    // Settings: Marathon's names in, Android's permissions out.
    let manage = Manage::default();
    let dir = tempfile::tempdir()?;
    let runtime = Runtime::new(
        dir.path().join("settings.json"),
        dir.path().join("applications"),
        dir.path().join("memory.current"),
    );
    let api_bus = Connection::session()?;
    let api = SettingsApi {
        bus: api_bus.inner().clone(),
        runtime,
        manage: manage.clone(),
        icon_dirs: vec![],
    };
    api_bus.object_server().at(OBJECT_PATH, api)?;
    api_bus.request_name(BUS_NAME)?;
    let (tx, android) = async_channel::bounded(4);
    let generation = manage.connect(tx);
    let replier = manage.clone();
    let responder = std::thread::spawn(move || {
        let mut ops = Vec::new();
        while let Ok(sent) = android.recv_blocking() {
            let apps = json!([{"pkg":"org.fdroid.fdroid", "label":"F-Droid", "granted":true, "fixed":true}]);
            let reply = json!({"t":"managed", "id":sent["id"], "ok":true, "apps":apps});
            replier.handle(generation, reply.as_object().unwrap());
            ops.push(Value::Object(sent));
        }
        ops
    });
    let settings = Proxy::new(&bus, BUS_NAME, OBJECT_PATH, "org.marathonos.Deckard1")?;
    let invalid =
        |method: &str, args: &(&str, &str, bool)| match settings.call::<_, _, ()>(method, args) {
            Err(zbus::Error::MethodError(name, ..)) => {
                name.as_str() == "org.freedesktop.DBus.Error.InvalidArgs"
            }
            _ => false,
        };
    assert!(invalid(
        "SetPermission",
        &(
            "org.fdroid.fdroid",
            "android.permission.INSTALL_PACKAGES",
            true
        )
    ));
    assert!(invalid(
        "SetPermission",
        &("org.fdroid.fdroid", "nearby", true)
    ));
    assert!(invalid("SetPermission", &("not a package", "camera", true)));
    settings.call::<_, _, ()>("SetPermission", &("org.fdroid.fdroid", "bluetooth", false))?;
    settings.call::<_, _, ()>(
        "SetPermission",
        &("org.fdroid.fdroid", "notifications", true),
    )?;
    let apps: Vec<VariantMap> = settings.call("GetPermissionApps", &("camera",))?;
    assert_eq!(String::try_from(apps[0]["label"].try_clone()?)?, "F-Droid");
    assert!(bool::try_from(apps[0]["fixed"].try_clone()?)?);
    let _: Vec<VariantMap> = settings.call("GetPermissionApps", &("notifications",))?;
    assert!(
        settings
            .call::<_, _, Vec<VariantMap>>("GetPermissionApps", &("system",))
            .is_err()
    );
    manage.disconnect(generation);
    let ops = responder.join().unwrap();
    assert_eq!(ops.len(), 4);
    assert_eq!(ops[0]["op"], "perm");
    assert!(
        ops[0]["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("android.permission.BLUETOOTH_SCAN"))
    );
    assert_eq!(ops[0]["granted"], false);
    assert_eq!(
        (&ops[1]["op"], &ops[1]["enabled"]),
        (&json!("notifications"), &json!(true))
    );
    assert_eq!(ops[2]["op"], "perm_apps");
    assert_eq!(ops[2]["permissions"], json!(["android.permission.CAMERA"]));
    assert_eq!(ops[3]["op"], "notif_apps");
    api_bus.release_name(BUS_NAME)?;
    shell.release_name("org.marathonos.Shell")?;
    Ok(())
}
