#![allow(
    clippy::unwrap_used,
    reason = "private-bus tests construct fixed protocol fixtures"
)]

use deckard_bridge::api::{BUS_NAME, OBJECT_PATH, SettingsApi};
use deckard_bridge::contract;
use deckard_bridge::manage::Manage;
use deckard_bridge::runtime::Runtime;
use deckard_core::framing;
use deckard_core::ids::PackageName;
use deckard_core::transport::BridgeListener;
use serde_json::json;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use zbus::blocking::{Connection, Proxy};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;

static PRIVATE_BUS: Mutex<()> = Mutex::new(());

struct Shell(Arc<Mutex<String>>);

#[interface(name = "org.marathonos.Shell.Navigation1")]
impl Shell {
    #[zbus(name = "AppIdForPid")]
    fn app_id_for_pid(&self, _pid: u32) -> String {
        self.0.lock().unwrap().clone()
    }
}

#[test]
fn cxx_manage_request_and_reply_shapes() {
    assert_eq!(
        contract::permission("camera"),
        Some(vec!["android.permission.CAMERA".into()])
    );
    assert_eq!(
        contract::permission("notifications"),
        Some(vec!["android.permission.POST_NOTIFICATIONS".into()])
    );
    assert!(contract::permission("android.permission.CAMERA").is_none());
    assert!(contract::SPECIAL_KINDS.contains(&"install_apps"));
    assert!(!contract::SPECIAL_KINDS.contains(&"root"));
    let app = contract::app(json!({
        "pkg":"org.fdroid.fdroid", "label":"F-Droid", "dataBytes":4096,
        "battery":"invalid", "perms":[{"name":"android.permission.BLUETOOTH_SCAN","granted":true}]
    }).as_object().unwrap());
    assert_eq!(
        String::try_from(app.get("label").unwrap().clone()).unwrap(),
        "F-Droid"
    );
    assert_eq!(
        i64::try_from(app.get("dataBytes").unwrap().clone()).unwrap(),
        4096
    );
    assert_eq!(
        String::try_from(app.get("battery").unwrap().clone()).unwrap(),
        "optimized"
    );
    let renamed = contract::app(
        json!({"pkg":"com.example","label":"LineageOS Settings"})
            .as_object()
            .unwrap(),
    );
    assert_eq!(
        String::try_from(renamed["label"].clone()).unwrap(),
        "Android Settings"
    );
    assert!(contract::app(json!({"pkg":"../etc"}).as_object().unwrap()).is_empty());
    let apps = contract::apps(
        json!({"apps":[
            {"pkg":"com.example","granted":true}, {"pkg":"com.example","granted":false},
            {"pkg":"../etc"}
        ]})
        .as_object()
        .unwrap(),
        contract::Fixed::Shown,
    );
    assert_eq!(apps.len(), 1);
    assert!(apps[0].contains_key("fixed"));
    let gms = contract::google(
        json!({"installed":true,"enabled":false,"push":true,"accounts":[" a@b.c "]})
            .as_object()
            .unwrap(),
    );
    assert!(!bool::try_from(gms.get("push").unwrap().clone()).unwrap());
}

#[test]
fn cxx_system_socket_framing_and_peer_uid() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let package = PackageName::try_from("org.marathonos.deckard.system")?;
    let socket = dir.path().join("deckard/system.sock");
    let packages = dir.path().join("system/packages.list");
    std::fs::create_dir_all(packages.parent().unwrap())?;
    let uid = std::fs::metadata(dir.path())?.uid();
    std::fs::write(
        &packages,
        format!("{} {uid} 0 /data/user/0\n", package.as_str()),
    )?;
    let mut listener = BridgeListener::bind(&socket, &packages, package)?;
    let mut client = UnixStream::connect(&socket)?;
    let mut session = listener.accept()?;
    let message = json!({"t":"managed","id":"0000000000000001","ok":true});
    client.write_all(&framing::encode(message.as_object().unwrap())?)?;
    assert_eq!(session.read_message()?.unwrap()["id"], "0000000000000001");
    session.send_message(
        json!({"t":"manage","op":"info","id":"0000000000000002"})
            .as_object()
            .unwrap(),
    )?;
    let mut header = [0_u8; 4];
    client.read_exact(&mut header)?;
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    client.read_exact(&mut body)?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body)?["op"],
        "info"
    );
    std::fs::write(
        &packages,
        "org.marathonos.deckard.system 4294967294 0 /data/user/0\n",
    )?;
    let _denied = UnixStream::connect(&socket)?;
    assert_eq!(
        listener.accept().err().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    Ok(())
}

// Run with: dbus-run-session -- env DECKARD_BRIDGE_PRIVATE_BUS=1 cargo test -p deckard-bridge --test contract
#[test]
fn cxx_dbus_method_and_signal_signatures() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let _guard = PRIVATE_BUS.lock().unwrap();
    let dir = tempfile::tempdir()?;
    let bus = Connection::session()?;
    let api = SettingsApi {
        bus: bus.inner().clone(),
        runtime: Runtime::new(
            dir.path().join("settings.json"),
            dir.path().join("applications"),
            dir.path().join("memory.current"),
        ),
        manage: Manage::default(),
        icon_dirs: vec![],
    };
    bus.object_server().at(OBJECT_PATH, api)?;
    assert_eq!(
        bus.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())?,
        RequestNameReply::PrimaryOwner
    );
    let caller = Connection::session()?;
    let proxy = Proxy::new(
        &caller,
        BUS_NAME,
        OBJECT_PATH,
        "org.freedesktop.DBus.Introspectable",
    )?;
    let xml: String = proxy.call("Introspect", &())?;
    for (name, args, output) in [
        ("GetRuntime", "", "a{sv}"),
        ("Start", "", ""),
        ("Stop", "", ""),
        ("Restart", "", ""),
        ("SetStartAtLogin", "b", ""),
        ("AppIcon", "s", "s"),
        ("GetApp", "s", "a{sv}"),
        ("SetPermission", "ssb", ""),
        ("GetPermissionApps", "s", "aa{sv}"),
        ("SetNotifications", "sb", ""),
        ("SetOpenLinks", "sb", ""),
        ("SetBattery", "ss", ""),
        ("ForceStop", "s", ""),
        ("ClearCache", "s", ""),
        ("ClearData", "s", ""),
        ("GetSpecialAccess", "s", "aa{sv}"),
        ("SetSpecialAccess", "ssb", ""),
        ("ResetAndroid", "", ""),
        ("GetGoogleServices", "", "a{sv}"),
        ("SetGoogleServices", "sb", ""),
        ("OpenGoogleServices", "s", ""),
    ] {
        let start = format!("<method name=\"{name}\">");
        let body = xml
            .split(&start)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {name}"))
            .split("</method>")
            .next()
            .unwrap();
        let inputs: String = body
            .lines()
            .filter(|line| line.contains("direction=\"in\""))
            .filter_map(|line| line.split("type=\"").nth(1))
            .filter_map(|line| line.split('"').next())
            .collect();
        let outputs: String = body
            .lines()
            .filter(|line| line.contains("direction=\"out\""))
            .filter_map(|line| line.split("type=\"").nth(1))
            .filter_map(|line| line.split('"').next())
            .collect();
        assert_eq!(inputs, args, "{name} input signature");
        assert_eq!(outputs, output, "{name} output signature");
    }
    assert!(xml.contains("<signal name=\"RuntimeChanged\">"));
    assert!(xml.contains("<signal name=\"AppChanged\">"));
    bus.release_name(BUS_NAME)?;
    Ok(())
}

#[test]
fn cxx_dbus_authorization_and_manage_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS").is_none() {
        return Ok(());
    }
    let _guard = PRIVATE_BUS.lock().unwrap();
    let dir = tempfile::tempdir()?;
    let shell = Connection::session()?;
    let app_id = Arc::new(Mutex::new("notes".to_owned()));
    shell
        .object_server()
        .at("/org/marathonos/Shell/Navigation", Shell(app_id.clone()))?;
    shell.request_name("org.marathonos.Shell")?;
    let bus = Connection::session()?;
    let manage = Manage::default();
    let runtime = Runtime::new(
        dir.path().join("settings.json"),
        dir.path().join("applications"),
        dir.path().join("memory.current"),
    );
    bus.object_server().at(
        OBJECT_PATH,
        SettingsApi {
            bus: bus.inner().clone(),
            runtime,
            manage: manage.clone(),
            icon_dirs: vec![],
        },
    )?;
    bus.request_name(BUS_NAME)?;
    let caller = Connection::session()?;
    let api = Proxy::new(&caller, BUS_NAME, OBJECT_PATH, "org.marathonos.Deckard1")?;
    let snapshot: contract::VariantMap = api.call("GetRuntime", &())?;
    assert_eq!(String::try_from(snapshot["state"].clone())?, "stopped");
    let icon: String = api.call("AppIcon", &("../etc",))?;
    assert!(icon.is_empty());
    assert_eq!(
        method_error(
            &api.call::<_, _, ()>("SetStartAtLogin", &(true,))
                .unwrap_err()
        ),
        "org.freedesktop.DBus.Error.AccessDenied"
    );
    assert_eq!(
        method_error(
            &api.call::<_, _, contract::VariantMap>("GetApp", &("bad name",))
                .unwrap_err()
        ),
        "org.freedesktop.DBus.Error.InvalidArgs"
    );
    *app_id.lock().unwrap() = "settings".into();
    api.call::<_, _, ()>("SetStartAtLogin", &(true,))?;
    let policy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("settings.json"))?)?;
    assert_eq!(policy["startAtLogin"], true);
    let (tx, rx) = async_channel::bounded(2);
    let generation = manage.connect(tx);
    let responder = std::thread::spawn(move || {
        let sent = rx.recv_blocking().unwrap();
        assert_eq!(sent["op"], "app");
        assert_eq!(sent["pkg"], "org.fdroid.fdroid");
        let reply = json!({"t":"managed", "id":sent["id"], "ok":true,
            "pkg":"org.fdroid.fdroid", "label":"F-Droid", "dataBytes":4096});
        manage.handle(generation, reply.as_object().unwrap());
        let sent = rx.recv_blocking().unwrap();
        assert_eq!(sent["op"], "clear");
        let reply = json!({"t":"managed", "id":sent["id"], "ok":false, "error":"no such app"});
        manage.handle(generation, reply.as_object().unwrap());
    });
    let app: contract::VariantMap = api.call("GetApp", &("org.fdroid.fdroid",))?;
    assert_eq!(String::try_from(app["label"].clone())?, "F-Droid");
    let error = api
        .call::<_, _, ()>("ClearData", &("org.fdroid.fdroid",))
        .unwrap_err();
    assert_eq!(method_error(&error), "org.marathonos.Deckard1.Error.Failed");
    responder.join().unwrap();
    bus.release_name(BUS_NAME)?;
    shell.release_name("org.marathonos.Shell")?;
    Ok(())
}

fn method_error(error: &zbus::Error) -> &str {
    match error {
        zbus::Error::MethodError(name, _, _) => name.as_str(),
        _ => panic!("unexpected error: {error}"),
    }
}
