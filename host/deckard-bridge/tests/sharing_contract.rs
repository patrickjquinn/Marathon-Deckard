#![allow(
    clippy::unwrap_used,
    reason = "contract tests drive fixed fixtures and fakes on a private bus"
)]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use deckard_bridge::sharing::{Sharing, target_id};
use serde_json::{Map, Value, json};
use zbus::blocking::connection::Builder;
use zbus::blocking::{Connection, Proxy};
use zbus::interface;
use zbus::zvariant::{Fd, OwnedValue, Value as Variant};

static PRIVATE_BUS: Mutex<()> = Mutex::new(());

type Sent = Arc<Mutex<Vec<Map<String, Value>>>>;
type Request = HashMap<&'static str, Variant<'static>>;
type Targets = Vec<HashMap<String, OwnedValue>>;

#[test]
fn target_ids() {
    assert!(target_id("org.example.chat/org.example.chat.ShareActivity"));
    assert!(target_id("org.example.chat/.Share$Inner"));
    let long = format!("org.example.chat/{}", "a".repeat(260));
    for bad in [
        "org.example.chat",
        "org.example.chat/a/b",
        "../x/Activity",
        &long,
    ] {
        assert!(!target_id(bad), "{bad}");
    }
}

fn private_bus() -> Option<MutexGuard<'static, ()>> {
    std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS")?;
    Some(PRIVATE_BUS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn map(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// The next message DeckardSystem was sent.
fn next(sent: &Sent) -> Map<String, Value> {
    for _ in 0..500 {
        if let Some(message) = sent.lock().unwrap().pop() {
            return message;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("nothing sent");
}

fn serve(name: &str, path: &str, iface: impl zbus::object_server::Interface) -> Connection {
    let builder = Builder::session().unwrap().name(name).unwrap();
    builder.serve_at(path, iface).unwrap().build().unwrap()
}

struct Navigation;

#[interface(name = "org.marathonos.Shell.Navigation1")]
impl Navigation {
    fn expect_app_window(&self, _app: String) -> bool {
        true
    }
}

fn shell() -> Connection {
    serve(
        "org.marathonos.Shell",
        "/org/marathonos/Shell/Navigation",
        Navigation,
    )
}

/// Sharing on its own connection with DeckardSystem as `sent`, and a client.
fn start(xfer: &Path) -> (Connection, Sharing, Sent, Proxy<'static>) {
    let sent = Sent::default();
    let sink = sent.clone();
    let send = Arc::new(move |message| {
        sink.lock().unwrap().push(message);
        true
    });
    let bus = Connection::session().unwrap();
    let sharing = Sharing::new(bus.inner().clone(), send, xfer.into()).unwrap();
    bus.object_server()
        .at("/org/marathonos/Deckard", sharing.clone())
        .unwrap();
    let (client, to) = (
        Connection::session().unwrap(),
        bus.unique_name().unwrap().to_string(),
    );
    let proxy = Proxy::new(
        &client,
        to,
        "/org/marathonos/Deckard",
        "org.marathonos.Deckard1.Share",
    );
    (bus, sharing, sent, proxy.unwrap())
}

fn error_name<T: std::fmt::Debug>(result: zbus::Result<T>) -> String {
    match result.unwrap_err() {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        error => error.to_string(),
    }
}

fn targets(proxy: &Proxy<'static>) -> JoinHandle<zbus::Result<Targets>> {
    let proxy = proxy.clone();
    thread::spawn(move || proxy.call("ShareTargets", &("image/png",)))
}

#[test]
fn targets_only_for_the_shell() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (_bus, sharing, sent, proxy) = start(dir.path());
    let denied = targets(&proxy).join().unwrap();
    assert_eq!(
        error_name(denied),
        "org.freedesktop.DBus.Error.AccessDenied"
    );
    let _shell = shell();
    assert!(targets(&proxy).join().unwrap().unwrap().is_empty());
    let invalid = proxy.call::<_, _, Targets>("ShareTargets", &("not a type",));
    assert_eq!(
        error_name(invalid),
        "org.freedesktop.DBus.Error.InvalidArgs"
    );
    assert!(sent.lock().unwrap().is_empty());

    sharing.connected(3);
    let asked = targets(&proxy);
    let message = next(&sent);
    assert_eq!(
        (&message["t"], &message["mime"]),
        (&json!("share_targets"), &json!("image/png"))
    );
    let mut png = Cursor::new(Vec::new());
    image::RgbaImage::new(200, 100)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let icon = STANDARD.encode(png.into_inner());
    let targets = json!([
        {"pkg":"org.example.chat","activity":"org.example.chat.ShareActivity","label":"Chat","icon":icon,"multiple":true},
        {"pkg":"bad pkg","activity":"A"},
        {"pkg":"org.marathonos.deckard.system","activity":"org.marathonos.deckard.system.Chooser"},
        {"pkg":"org.example.notes","activity":"org.example.notes/Evil"},
        {"pkg":"org.example.notes","activity":"org.example.notes.Add","label":"Add to Waydroid  notes","icon":"not base64 png"},
    ]);
    sharing.handle(
        3,
        &map(json!({"t":"share_targets_result","id":message["id"],"targets":targets})),
    );
    let list = asked.join().unwrap().unwrap();
    let text = |n: usize, key: &str| String::try_from(list[n][key].try_clone().unwrap()).unwrap();
    let multiple = |n: usize| bool::try_from(&list[n]["multiple"]).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(
        text(0, "id"),
        "org.example.chat/org.example.chat.ShareActivity"
    );
    assert_eq!(
        (text(0, "pkg"), text(0, "label")),
        ("org.example.chat".into(), "Chat".into())
    );
    assert!(text(0, "icon").starts_with("data:image/png;base64,") && multiple(0));
    assert_eq!(
        (text(1, "label"), text(1, "icon")),
        ("Add to Android notes".into(), String::new())
    );
    assert!(!multiple(1));
}

/// A request as the Shell's share sheet builds it: resources as `av`.
fn request(mime: &str, resources: Vec<Request>) -> Request {
    let resources: Vec<Variant<'static>> = resources.into_iter().map(Variant::from).collect();
    let mut request = HashMap::from([("resources", Variant::from(resources))]);
    if !mime.is_empty() {
        request.insert("mimeType", mime.to_owned().into());
        request.insert("title", "Holiday".into());
    }
    request
}

fn file(name: &str, fd: impl Into<OwnedFd>) -> Request {
    let fd = Variant::from(Fd::from(fd.into()));
    HashMap::from([
        ("name", Variant::from(name.to_owned())),
        ("fileDescriptor", fd),
    ])
}

fn note(text: &str) -> Vec<Request> {
    vec![HashMap::from([("data", Variant::from(text.to_owned()))])]
}

#[test]
fn shares_files_and_text() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (xfer, _shell) = (dir.path().join("xfer"), shell());
    let (_bus, sharing, sent, proxy) = start(&xfer);
    sharing.connected(3);
    let photo = dir.path().join("photo.jpg");
    fs::write(&photo, "jpeg bytes").unwrap();
    let target = "org.example.chat/org.example.chat.ShareActivity";
    let share = |id: &str, request: Request| {
        let (proxy, id) = (proxy.clone(), id.to_owned());
        thread::spawn(move || proxy.call::<_, _, ()>("Share", &(id, request)))
    };
    let refused = |id, request| error_name(share(id, request).join().unwrap());
    let invalid = "org.freedesktop.DBus.Error.InvalidArgs";
    assert_eq!(refused("not a target", request("", note("hi"))), invalid);
    assert_eq!(refused(target, request("", Vec::new())), invalid);
    let (pipe, _writer) = std::io::pipe().unwrap();
    assert_eq!(refused(target, request("", vec![file("p", pipe)])), invalid);
    assert!(sent.lock().unwrap().is_empty());

    let open = || File::open(&photo).unwrap();
    let mut files = vec![file("../../photo.jpg", open()), file("photo.jpg", open())];
    files.extend(note("Look at this"));
    let shared = share(target, request("image/jpeg", files));
    let message = next(&sent);
    let tid = message["id"].as_str().unwrap().to_owned();
    let names = ["photo.jpg", "2-photo.jpg"];
    let expected = json!({"t":"share","id":tid,"target":target,"mime":"image/jpeg","title":"Holiday","text":"Look at this","files":names});
    assert_eq!(message, map(expected));
    for name in names {
        assert_eq!(fs::read(xfer.join(&tid).join(name)).unwrap(), b"jpeg bytes");
    }
    sharing.handle(3, &map(json!({"t":"shared","id":tid,"ok":true})));
    shared.join().unwrap().unwrap();

    let failed = share(target, request("", note("hi")));
    let message = next(&sent);
    assert_eq!(message["mime"], "text/plain");
    sharing.handle(
        3,
        &map(json!({"t":"shared","id":message["id"],"ok":false,"error":"no activity"})),
    );
    assert_eq!(
        error_name(failed.join().unwrap()),
        "org.marathonos.Deckard1.Error.Failed"
    );

    let lost = share(target, request("", note("hi")));
    next(&sent);
    sharing.disconnected(3);
    assert_eq!(
        error_name(lost.join().unwrap()),
        "org.marathonos.Deckard1.Error.Failed"
    );
}

/// The Shell's org.sailfishos.share, recording what it was asked.
#[derive(Clone, Default)]
struct Sheet(Arc<Mutex<Vec<HashMap<String, OwnedValue>>>>);

#[interface(name = "org.sailfishos.share")]
impl Sheet {
    #[zbus(name = "share")]
    fn share(&self, configuration: HashMap<String, OwnedValue>) {
        self.0.lock().unwrap().push(configuration);
    }
}

fn bare<'a, 'v>(value: &'a Variant<'v>) -> &'a Variant<'v> {
    match value {
        Variant::Value(inner) => bare(inner),
        value => value,
    }
}

fn text(value: &Variant<'_>) -> String {
    String::try_from(bare(value).try_clone().unwrap()).unwrap()
}

#[test]
fn android_shares_open_the_sheet() {
    let Some(_bus) = private_bus() else { return };
    let sheet = Sheet::default();
    let _sheet = serve("org.sailfishos.share", "/", sheet.clone());
    let dir = tempfile::tempdir().unwrap();
    let (xfer, tid, pkg) = (
        dir.path().join("xfer"),
        "0123456789abcdef",
        "org.example.scanner",
    );
    fs::create_dir_all(xfer.join(tid)).unwrap();
    fs::write(xfer.join(tid).join("scan.pdf"), "%PDF").unwrap();
    std::os::unix::fs::symlink(dir.path().join("secret"), xfer.join(tid).join("link")).unwrap();
    let (_bus, sharing, _sent, _proxy) = start(&xfer);
    sharing.connected(3);

    sharing.handle(
        3,
        &map(json!({"t":"share_out","id":"../etc","pkg":pkg,"files":["scan.pdf"]})),
    );
    sharing.handle(
        3,
        &map(json!({"t":"share_out","id":tid,"pkg":pkg,"files":["link"]})),
    );
    thread::sleep(Duration::from_millis(300));
    assert!(sheet.0.lock().unwrap().is_empty());

    let files = ["scan.pdf", "../scan.pdf"];
    let out = json!({"t":"share_out","id":tid,"pkg":pkg,"mime":"application/pdf","title":"Receipt","text":"From the scanner","files":files});
    sharing.handle(3, &map(out));
    for _ in 0..500 {
        if !sheet.0.lock().unwrap().is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let request = sheet.0.lock().unwrap().remove(0);
    assert_eq!(text(&request["source"]), "waydroid.org.example.scanner");
    assert_eq!(
        (text(&request["mimeType"]), text(&request["title"])),
        ("application/pdf".into(), "Receipt".into())
    );
    let Variant::Array(resources) = &*request["resources"] else {
        panic!("resources")
    };
    let entries = |n: usize| {
        let Variant::Dict(dict) = bare(&resources[n]) else {
            panic!("resource")
        };
        HashMap::<String, OwnedValue>::try_from(dict.try_clone().unwrap()).unwrap()
    };
    assert_eq!(
        (resources.len(), text(&entries(0)["name"])),
        (2, "scan.pdf".into())
    );
    let file = entries(0);
    let Variant::Fd(fd) = bare(&file["fileDescriptor"]) else {
        panic!("descriptor")
    };
    let mut contents = String::new();
    File::from(fd.as_fd().try_clone_to_owned().unwrap())
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "%PDF");
    assert_eq!(text(&entries(1)["data"]), "From the scanner");
}
