#![allow(
    clippy::unwrap_used,
    reason = "contract tests drive fixed fixtures and fakes on a private bus"
)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use deckard_bridge::intents::{Intents, Target, request_id, route, safe_name, settings_route};
use serde_json::{Map, Value, json};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::message::Header;
use zbus::object_server::{Interface, ObjectServer};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as Variant};
use zbus::{fdo, interface};

static PRIVATE_BUS: Mutex<()> = Mutex::new(());

fn map(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn keel(app: &'static str, action: &'static str, args: Value) -> Option<Target> {
    Some(Target::Keel(app, action, map(args)))
}

fn routed(app: &'static str, route: &str, args: Value) -> Option<Target> {
    Some(Target::Route(app, route.into(), map(args)))
}

#[test]
#[rustfmt::skip]
fn opens_route_like_cxx() {
    let cases = [
        (json!({"kind":"url","uri":"https://f-droid.org/packages/"}),
         keel("browser", "browser.open", json!({"url":"https://f-droid.org/packages/","newTab":true}))),
        (json!({"kind":"url","uri":"https://user@de.wikipedia.org/wiki/Köln"}),
         keel("browser", "browser.open", json!({"url":"https://user@de.wikipedia.org/wiki/K%C3%B6ln","newTab":true}))),
        (json!({"kind":"url","uri":"https://example.org/%zz"}), None),
        (json!({"kind":"url","uri":"javascript:alert(1)"}), None),
        (json!({"kind":"url","uri":"file:///etc/passwd"}), None),
        (json!({"kind":"tel","uri":"tel:+353%2087%20123-4567"}), routed("phone", "/call", json!({"number":"+353871234567"}))),
        (json!({"kind":"tel","uri":"tel:"}), Some(Target::Launch("phone"))),
        (json!({"kind":"tel","uri":"tel:$(reboot)"}), Some(Target::Launch("phone"))),
        (json!({"kind":"tel","uri":"sms:1"}), None),
        (json!({"kind":"sms","uri":"smsto:0871234567?body=hi"}),
         routed("messages", "conversation", json!({"number":"0871234567","body":"hi"}))),
        (json!({"kind":"sms","uri":"smsto:0871234567","body":"See you at 8"}),
         routed("messages", "conversation", json!({"number":"0871234567","body":"See you at 8"}))),
        (json!({"kind":"sms","uri":"sms:","body":"x".repeat(20_000)}), routed("messages", "compose", json!({"body":"x".repeat(10_000)}))),
        (json!({"kind":"sms","uri":"sms:"}), Some(Target::Launch("messages"))),
        (json!({"kind":"mailto","uri":"mailto:a@b.ie","to":["a@b.ie","bad address"],"subject":"Hi"}),
         keel("email", "mail.compose", json!({"to":"a@b.ie","subject":"Hi"}))),
        (json!({"kind":"geo","uri":"geo:53.3498,-6.2603"}), keel("maps", "map.show", json!({"lat":53.3498,"lon":-6.2603}))),
        (json!({"kind":"geo","uri":"geo:0,0?q=53.34,-6.26(Trinity%20College)"}),
         keel("maps", "map.show", json!({"lat":53.34,"lon":-6.26,"name":"Trinity College"}))),
        (json!({"kind":"geo","uri":"geo:0,0?q=coffee%20near%20me"}), keel("maps", "search.show", json!({"query":"coffee near me"}))),
        (json!({"kind":"geo","uri":format!("geo:0,0?q={}", "a".repeat(300))}), keel("maps", "search.show", json!({"query":"a".repeat(256)}))),
        (json!({"kind":"geo","uri":"geo:0,0"}), Some(Target::Launch("maps"))),
        (json!({"kind":"geo","uri":"https://maps"}), None),
        (json!({"kind":"settings","action":"android.settings.SETTINGS"}), Some(Target::Launch("settings"))),
    ];
    for (message, target) in cases {
        assert_eq!(route(&map(message.clone())).ok(), target, "{message}");
    }
    let pages = [
        ("android.settings.WIFI_SETTINGS", "", "wifi"),
        ("android.settings.APPLICATION_DETAILS_SETTINGS", "org.fdroid.fdroid", "appmanager/deckard.org.fdroid.fdroid"),
        ("android.settings.APP_NOTIFICATION_SETTINGS", "../x", "notifications"),
        ("android.settings.LOCATION_SOURCE_SETTINGS", "", ""),
        ("android.intent.action.POWER_USAGE_SUMMARY", "", "battery"),
        ("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION", "me.zhanghai.android.files",
         "androidaccess/all_files/me.zhanghai.android.files"),
        ("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION", "", "androidaccess/all_files"),
        ("android.settings.MANAGE_UNKNOWN_APP_SOURCES", "../../x", "androidaccess/install_apps"),
        ("android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS", "org.fdroid.fdroid", "androidaccess/battery/org.fdroid.fdroid"),
    ];
    for (action, pkg, page) in pages {
        assert_eq!(settings_route(action, pkg), page, "{action}");
    }
    assert!(request_id("0123456789abcdef") && !request_id("../0123456789abc") && !request_id("0123456789ABCDEF"));
    assert_eq!(safe_name("/home/user/Documents/report.pdf"), "report.pdf");
    assert_eq!(safe_name(".."), "file");
    assert_eq!(safe_name("a\nb\\c\u{202e}.txt"), "abc\u{202e}.txt");
}

/// What DeckardSystem was sent.
#[derive(Clone, Default)]
struct Sent(Arc<Mutex<Vec<Map<String, Value>>>>);

impl Sent {
    fn none_after(&self, wait: Duration) -> bool {
        thread::sleep(wait);
        self.0.lock().unwrap().is_empty()
    }

    fn next(&self) -> Map<String, Value> {
        wait(|| !self.0.lock().unwrap().is_empty());
        self.0.lock().unwrap().remove(0)
    }
}

fn wait(done: impl Fn() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out");
}

/// Serialises the private-bus tests; `None` without one.
fn private_bus() -> Option<MutexGuard<'static, ()>> {
    std::env::var_os("DECKARD_BRIDGE_PRIVATE_BUS")?;
    Some(deckard_core::sync::lock(&PRIVATE_BUS))
}

fn serve(name: &str, path: &str, iface: impl Interface) -> Connection {
    let builder = Builder::session().unwrap().name(name).unwrap();
    builder.serve_at(path, iface).unwrap().build().unwrap()
}

/// Intents connected as generation 7, past its first focus message.
fn start(xfer: &Path, camera: &Path) -> (Connection, Intents, Sent) {
    let (bus, sent) = (Connection::session().unwrap(), Sent::default());
    let sink = sent.clone();
    let send = Arc::new(move |message| {
        sink.0.lock().unwrap().push(message);
        true
    });
    let intents = Intents::start(bus.inner().clone(), send, xfer.into(), camera.into());
    intents.connected(7);
    assert_eq!(sent.next(), map(json!({"t":"focus","pkg":""})));
    (bus, intents, sent)
}

fn send(intents: &Intents, message: Value) {
    intents.handle(7, &map(message));
}

struct Navigation;

#[interface(name = "org.marathonos.Shell.Navigation1")]
impl Navigation {}

#[test]
fn focus_follows_marathon() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let path = "/org/marathonos/Shell/Navigation";
    let shell = serve("org.marathonos.Shell", path, Navigation);
    let (_bus, intents, sent) = start(&dir.path().join("xfer"), dir.path());
    thread::sleep(Duration::from_millis(200));
    let emit = |app: &str| {
        let iface = "org.marathonos.Shell.Navigation1";
        let body = (app,);
        shell
            .emit_signal(None::<&str>, path, iface, "ForegroundAppChanged", &body)
            .unwrap();
    };
    emit("deckard.org.example.files");
    assert_eq!(
        sent.next(),
        map(json!({"t":"focus","pkg":"org.example.files"}))
    );
    emit("browser");
    emit("notes");
    assert_eq!(sent.next(), map(json!({"t":"focus","pkg":""})));
    intents.foreground_changed("deckard.../x");
    assert!(sent.none_after(Duration::from_millis(100)));
}

/// Marathon Camera's Keel Actions: answers camera.capture when the test says.
struct Camera(Sender<String>, Receiver<Option<String>>);

#[interface(name = "org.shipwright.Keel.Actions")]
impl Camera {
    async fn invoke(
        &self,
        action: String,
        args: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<String> {
        let _ = self.0.send(format!("{action} {args}")).await;
        let answer = self.1.recv().await.ok().flatten();
        answer.ok_or_else(|| fdo::Error::Failed("Cancelled".into()))
    }
}

#[test]
fn capture_answers_with_the_camera_file() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let pictures = dir.path().join("Pictures/Marathon");
    fs::create_dir_all(&pictures).unwrap();
    for (file, bytes) in [
        ("IMG_1.jpg", "jpeg"),
        ("VID_1.mp4", "mp4"),
        ("../../secret.jpg", "secret"),
    ] {
        fs::write(pictures.join(file), bytes).unwrap();
    }
    let ((calls, called), (answer, answers)) =
        (async_channel::unbounded(), async_channel::unbounded());
    let _camera = serve(
        "org.marathonos.camera",
        "/org/shipwright/Keel/Actions",
        Camera(calls, answers),
    );
    let xfer = dir.path().join("xfer");
    let (_bus, intents, sent) = start(&xfer, &pictures);
    let capture = |id: &str, kind: &str| {
        send(
            &intents,
            json!({"t":"pick","id":id,"mode":"capture","kind":kind}),
        );
        called.recv_blocking().unwrap()
    };
    let shot = |file: &Path| {
        answer
            .send_blocking(Some(json!({"path": file}).to_string()))
            .unwrap()
    };

    assert_eq!(
        capture("00000000000000a1", "image"),
        r#"camera.capture {"mode":"photo"}"#
    );
    assert!(sent.none_after(Duration::ZERO));
    shot(&pictures.join("IMG_1.jpg"));
    let file = json!([{"name":"IMG_1.jpg","size":4}]);
    assert_eq!(
        sent.next(),
        map(json!({"t":"picked","id":"00000000000000a1","ok":true,"files":file}))
    );
    assert_eq!(
        fs::read(xfer.join("00000000000000a1/IMG_1.jpg")).unwrap(),
        b"jpeg"
    );

    // Only files in Marathon Camera's folder, of the kind asked for.
    capture("00000000000000a2", "image");
    shot(&dir.path().join("secret.jpg"));
    assert_eq!(sent.next()["ok"], false);
    assert!(!xfer.join("00000000000000a2/secret.jpg").exists());
    capture("00000000000000a3", "image");
    shot(&pictures.join("VID_1.mp4"));
    assert_eq!(sent.next()["ok"], false);

    // The person left the camera.
    assert_eq!(
        capture("00000000000000a4", "video"),
        r#"camera.capture {"mode":"video"}"#
    );
    answer.send_blocking(None).unwrap();
    assert_eq!(
        sent.next(),
        map(json!({"t":"picked","id":"00000000000000a4","ok":false,"files":[]}))
    );

    // Android gave up first: the late answer goes nowhere.
    capture("00000000000000a5", "video");
    send(&intents, json!({"t":"pick_cancel","id":"00000000000000a5"}));
    shot(&pictures.join("VID_1.mp4"));
    assert!(sent.none_after(Duration::from_millis(200)));
}

type Call = (String, String, HashMap<String, OwnedValue>);

/// The FileChooser portal, with a closable Request per call.
#[derive(Clone, Default)]
struct Portal(Arc<Mutex<Vec<Call>>>, Arc<Mutex<Vec<String>>>);

struct Request(String, Arc<Mutex<Vec<String>>>);

#[interface(name = "org.freedesktop.portal.Request")]
impl Request {
    fn close(&self) {
        self.1.lock().unwrap().push(self.0.clone());
    }
}

impl Portal {
    async fn ask(
        &self,
        method: &str,
        options: HashMap<String, OwnedValue>,
        header: Header<'_>,
        server: &ObjectServer,
    ) -> OwnedObjectPath {
        let sender = header
            .sender()
            .unwrap()
            .trim_start_matches(':')
            .replace('.', "_");
        let token = text(&options, "handle_token");
        let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        server
            .at(path.as_str(), Request(path.clone(), self.1.clone()))
            .await
            .unwrap();
        self.0
            .lock()
            .unwrap()
            .push((method.into(), path.clone(), options));
        OwnedObjectPath::try_from(path).unwrap()
    }

    /// Call `n`: method, request path and options.
    fn call(&self, n: usize) -> Call {
        wait(|| self.0.lock().unwrap().len() > n);
        let calls = self.0.lock().unwrap();
        let options = calls[n]
            .2
            .iter()
            .map(|(k, v)| (k.clone(), v.try_clone().unwrap()));
        (calls[n].0.clone(), calls[n].1.clone(), options.collect())
    }

    fn closed(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
}

#[interface(name = "org.freedesktop.portal.FileChooser")]
impl Portal {
    async fn open_file(
        &self,
        _parent: String,
        _title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> OwnedObjectPath {
        self.ask("OpenFile", options, header, server).await
    }

    async fn save_file(
        &self,
        _parent: String,
        _title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> OwnedObjectPath {
        self.ask("SaveFile", options, header, server).await
    }
}

fn respond(portal: &Connection, path: &str, file: &Path) {
    let uri = format!("file://{}", file.display().to_string().replace(' ', "%20"));
    let results = HashMap::from([("uris", Variant::from(vec![uri]))]);
    let iface = "org.freedesktop.portal.Request";
    portal
        .emit_signal(None::<&str>, path, iface, "Response", &(0_u32, results))
        .unwrap();
}

fn text(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    String::try_from(options[key].try_clone().unwrap()).unwrap()
}

#[test]
fn chooser_copies_in_and_saves_out() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let portal = Portal::default();
    let bus = serve(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        portal.clone(),
    );
    let xfer = dir.path().join("xfer");
    let (_client, intents, sent) = start(&xfer, dir.path());
    let sample = dir.path().join("sample one.txt");
    fs::write(&sample, "hello").unwrap();

    let id = "0123456789abcdef";
    send(
        &intents,
        json!({"t":"pick","id":id,"mode":"open","multiple":true,"mime":["IMAGE/PNG","text/plain","bad"]}),
    );
    let (method, path, options) = portal.call(0);
    assert_eq!(
        (method.as_str(), text(&options, "handle_token")),
        ("OpenFile", "deckard_0123456789abcdef".into())
    );
    assert!(
        path.ends_with("/deckard_0123456789abcdef")
            && bool::try_from(&options["multiple"]).unwrap()
    );
    let filters =
        <Vec<(String, Vec<(u32, String)>)>>::try_from(options["filters"].try_clone().unwrap())
            .unwrap();
    assert_eq!(
        filters,
        [(
            "Files".into(),
            vec![(1, "image/png".into()), (1, "text/plain".into())]
        )]
    );
    respond(&bus, &path, &sample);
    let file = json!([{"name":"sample one.txt","size":5}]);
    assert_eq!(
        sent.next(),
        map(json!({"t":"picked","id":id,"ok":true,"files":file}))
    );
    assert_eq!(
        fs::read(xfer.join(id).join("sample one.txt")).unwrap(),
        b"hello"
    );

    // A link is never followed.
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&sample, &link).unwrap();
    send(
        &intents,
        json!({"t":"pick","id":"0000000000000001","mode":"open","mime":["*/*"]}),
    );
    let (_, path, options) = portal.call(1);
    assert!(!options.contains_key("filters"));
    respond(&bus, &path, &link);
    assert_eq!(sent.next()["ok"], false);

    // Each close of the saved file copies it to where the person chose.
    let save = "0000000000000002";
    send(
        &intents,
        json!({"t":"pick","id":save,"mode":"save","name":"../report.pdf"}),
    );
    let (method, path, options) = portal.call(2);
    assert_eq!(
        (method.as_str(), text(&options, "current_name")),
        ("SaveFile", "report.pdf".into())
    );
    let target = dir.path().join("report.pdf");
    respond(&bus, &path, &target);
    assert_eq!(
        sent.next(),
        map(json!({"t":"picked","id":save,"ok":true,"files":[{"name":"report.pdf"}]}))
    );
    fs::create_dir_all(xfer.join(save)).unwrap();
    for contents in ["draft", "final"] {
        fs::write(xfer.join(save).join("report.pdf"), contents).unwrap();
        send(&intents, json!({"t":"pick_done","id":save}));
        wait(|| fs::read_to_string(&target).is_ok_and(|text| text == contents));
    }
    assert!(!dir.path().join("report.pdf.deckard-part").exists());

    // The app gave up: the chooser goes too.
    send(
        &intents,
        json!({"t":"pick","id":"0000000000000003","mode":"open"}),
    );
    let (_, path, _) = portal.call(3);
    send(&intents, json!({"t":"pick_cancel","id":"0000000000000003"}));
    wait(|| portal.closed().contains(&path));
}

#[test]
fn picks_are_bounded_and_end_with_android() {
    let Some(_bus) = private_bus() else { return };
    let dir = tempfile::tempdir().unwrap();
    let portal = Portal::default();
    let _portal = serve(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        portal.clone(),
    );
    let (_client, intents, sent) = start(&dir.path().join("xfer"), dir.path());
    send(
        &intents,
        json!({"t":"pick","id":"fffffffffffffffe","mode":"print"}),
    );
    assert_eq!(
        sent.next(),
        map(json!({"t":"picked","id":"fffffffffffffffe","ok":false,"files":[]}))
    );
    intents.handle(
        6,
        &map(json!({"t":"pick","id":"00000000000000ff","mode":"open"})),
    );
    for n in 0..4 {
        send(
            &intents,
            json!({"t":"pick","id":format!("{n:016x}"),"mode":"open"}),
        );
        portal.call(n);
    }
    send(
        &intents,
        json!({"t":"pick","id":"0000000000000000","mode":"open"}),
    );
    send(
        &intents,
        json!({"t":"pick","id":"ffffffffffffffff","mode":"open"}),
    );
    assert_eq!(sent.next()["id"], "ffffffffffffffff");
    intents.disconnected(6);
    intents.disconnected(7);
    wait(|| portal.closed().len() == 4);
    assert_eq!(portal.0.lock().unwrap().len(), 4);
}
