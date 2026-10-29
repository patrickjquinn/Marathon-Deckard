#![allow(
    clippy::unwrap_used,
    reason = "private-bus fixtures assert wire behavior"
)]

use std::fs;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use deckard_packages::{
    BUS_NAME, BridgeRoute, INTERFACE, MAX_PENDING, OBJECT_PATH, Packages, VariantMap,
};
use serde_json::{Value, json};
use zbus::MatchRule;
use zbus::message::Type;
use zbus::{Connection, MessageStream, Proxy, connection::Builder};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Commands = Arc<Mutex<Vec<(BridgeRoute, Value)>>>;

struct PrivateBus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("bus");
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = daemon
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing bus address"))?;
        let mut address = String::new();
        BufReader::new(stdout).read_line(&mut address)?;
        Ok(Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        })
    }

    fn connection(&self) -> TestResult<Connection> {
        Ok(zbus::block_on(
            Builder::address(self.address.as_str())
                .method_timeout(Duration::from_secs(3))
                .build(),
        )?)
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn fixture(
    allowed: bool,
    connected: bool,
) -> TestResult<(
    PrivateBus,
    Connection,
    Packages,
    Commands,
    tempfile::TempDir,
)> {
    let bus = PrivateBus::start()?;
    let owner = zbus::block_on(
        Builder::address(bus.address.as_str())
            .name(BUS_NAME)
            .build(),
    )?;
    let client = bus.connection()?;
    let directory = tempfile::tempdir()?;
    let commands: Commands = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&commands);
    let service = Packages::new(
        owner.clone(),
        directory.path().join("install"),
        Arc::new(move |route, command| {
            captured.lock().unwrap().push((route, command.clone()));
            connected
        }),
        Arc::new(move |sender| Box::pin(async move { allowed && sender.starts_with(':') })),
    )?;
    zbus::block_on(owner.object_server().at(OBJECT_PATH, service.interface()))?;
    Ok((bus, client, service, commands, directory))
}

fn proxy(client: &Connection) -> TestResult<Proxy<'_>> {
    Ok(zbus::block_on(Proxy::new(
        client,
        BUS_NAME,
        OBJECT_PATH,
        INTERFACE,
    ))?)
}

fn command(commands: &Commands, count: usize) -> (BridgeRoute, Value) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(command) = commands.lock().unwrap().get(count).cloned() {
            return command;
        }
        assert!(
            Instant::now() < deadline,
            "bridge command {count} never arrived"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn map_string(map: &VariantMap, key: &str) -> String {
    <&str>::try_from(map.get(key).unwrap()).unwrap().to_owned()
}

fn map_bool(map: &VariantMap, key: &str) -> bool {
    bool::try_from(map.get(key).unwrap().clone()).unwrap()
}

#[test]
fn wire_signatures_match_live_pi() -> TestResult<()> {
    let (_bus, client, _service, _, _) = fixture(true, true)?;
    let introspect = zbus::block_on(Proxy::new(
        &client,
        BUS_NAME,
        OBJECT_PATH,
        "org.freedesktop.DBus.Introspectable",
    ))?;
    let xml: String = zbus::block_on(introspect.call("Introspect", &()))?;
    assert!(xml.contains("interface name=\"org.marathonos.Deckard1.Packages\""));
    for name in [
        "DeviceProfile",
        "InspectPackage",
        "InstallPackage",
        "ListPackages",
        "UninstallPackage",
        "Progress",
        "Finished",
        "PackageChanged",
    ] {
        assert!(
            xml.contains(&format!("name=\"{name}\"")),
            "missing {name}: {xml}"
        );
    }
    assert!(xml.contains("type=\"aa{sv}\""));
    assert!(xml.contains("type=\"sbss\"") || xml.contains("name=\"Finished\""));
    Ok(())
}

#[test]
fn inspect_and_profile_return_bounded_typed_maps() -> TestResult<()> {
    let (_bus, client, service, commands, directory) = fixture(true, true)?;
    let apk = directory.path().join("sample.apk");
    fs::write(&apk, b"PK\x03\x04payload")?;
    let caller = client.clone();
    let path = apk.to_string_lossy().to_string();
    let task = thread::spawn(move || -> TestResult<VariantMap> {
        Ok(zbus::block_on(
            proxy(&caller)?.call("InspectPackage", &(path,)),
        )?)
    });
    let (route, first) = command(&commands, 0);
    assert_eq!(route, BridgeRoute::User);
    assert_eq!(first["t"], "inspect");
    let staged = first["path"].as_str().unwrap().rsplit('/').next().unwrap();
    assert!(directory.path().join("install").join(staged).exists());
    service.handle(BridgeRoute::User, "inspect_result", json!({"id":first["id"], "ok":true, "pkg":"org.example.maps", "label":"Waydroid Maps", "features":["android.hardware.camera", "invalid feature"], "abis":["arm64-v8a", "../bad"], "icon":"aGVsbG8="}).as_object().unwrap())?;
    let info = task.join().unwrap()?;
    assert!(map_bool(&info, "ok"));
    assert_eq!(map_string(&info, "label"), "Android Maps");
    assert!(!directory.path().join("install").join(staged).exists());
    let caller = client.clone();
    let task = thread::spawn(move || -> TestResult<VariantMap> {
        Ok(zbus::block_on(proxy(&caller)?.call("DeviceProfile", &()))?)
    });
    let (_, second) = command(&commands, 1);
    service.handle(
        BridgeRoute::User,
        "device_result",
        json!({"id":second["id"], "abis":["arm64-v8a"], "sdk":33, "gms":true})
            .as_object()
            .unwrap(),
    )?;
    let profile = task.join().unwrap()?;
    assert_eq!(i32::try_from(profile["sdk"].clone()).unwrap(), 33);
    assert!(map_bool(&profile, "gms"));
    Ok(())
}

#[test]
fn install_uninstall_auth_and_bridge_loss() -> TestResult<()> {
    let (_bus, client, service, commands, directory) = fixture(true, true)?;
    let apk = directory.path().join("sample.apk");
    fs::write(&apk, b"PK\x03\x04payload")?;
    let job: String = zbus::block_on(
        proxy(&client)?.call("InstallPackage", &(apk.to_string_lossy().to_string(),)),
    )?;
    let (route, sent) = command(&commands, 0);
    assert_eq!(route, BridgeRoute::User);
    assert_eq!(sent["id"], job);
    assert_eq!(service.pending_count(), 1);
    service.handle(
        BridgeRoute::User,
        "install_result",
        json!({"id":job, "ok":true, "pkg":"org.example.app"})
            .as_object()
            .unwrap(),
    )?;
    assert_eq!(service.pending_count(), 0);
    let job: String =
        zbus::block_on(proxy(&client)?.call("UninstallPackage", &("org.example.app",)))?;
    assert_eq!(command(&commands, 1).0, BridgeRoute::System);
    service.handle(
        BridgeRoute::User,
        "install_result",
        json!({"id":job, "ok":true, "pkg":"org.example.app"})
            .as_object()
            .unwrap(),
    )?;
    assert_eq!(service.pending_count(), 1);
    let _: String = zbus::block_on(
        proxy(&client)?.call("InstallPackage", &(apk.to_string_lossy().to_string(),)),
    )?;
    assert_eq!(service.pending_count(), 2);
    service.bridge_lost(BridgeRoute::User)?;
    assert_eq!(service.pending_count(), 1);
    service.bridge_lost(BridgeRoute::System)?;
    assert_eq!(service.pending_count(), 0);
    assert!(!job.is_empty());
    let denied =
        zbus::block_on(proxy(&client)?.call::<_, _, String>("UninstallPackage", &("../invalid",)));
    assert!(denied.is_err());
    let (_bus, client, _, commands, _) = fixture(false, true)?;
    let denied = zbus::block_on(
        proxy(&client)?.call::<_, _, String>("UninstallPackage", &("org.example.app",)),
    );
    assert!(denied.is_err());
    assert!(commands.lock().unwrap().is_empty());
    Ok(())
}

#[test]
fn list_is_capped_and_disconnected_bridge_fails_fast() -> TestResult<()> {
    let (_bus, client, service, commands, _) = fixture(true, true)?;
    let caller = client.clone();
    let task = thread::spawn(move || -> TestResult<Vec<VariantMap>> {
        Ok(zbus::block_on(proxy(&caller)?.call("ListPackages", &()))?)
    });
    let (_, sent) = command(&commands, 0);
    let mut packages = vec![json!({"pkg":"../bad"})];
    for index in 0..2100 {
        packages.push(json!({"pkg":format!("org.example.app{index}"), "label":"LineageOS app", "versionCode":index}));
    }
    service.handle(
        BridgeRoute::User,
        "packages_result",
        json!({"id":sent["id"], "packages":packages})
            .as_object()
            .unwrap(),
    )?;
    let list = task.join().unwrap()?;
    assert_eq!(list.len(), 2000);
    assert_eq!(map_string(&list[0], "label"), "Android app");
    let (_bus, client, _, _, directory) = fixture(true, false)?;
    let apk = directory.path().join("sample.apk");
    fs::write(&apk, b"PK\x03\x04payload")?;
    let result = zbus::block_on(
        proxy(&client)?
            .call::<_, _, String>("InstallPackage", &(apk.to_string_lossy().to_string(),)),
    );
    assert!(result.is_err());
    assert_eq!(fs::read_dir(directory.path().join("install"))?.count(), 0);
    Ok(())
}

#[test]
fn pending_cap_rejects_new_jobs_without_leaking_staged_files() -> TestResult<()> {
    let (_bus, client, service, _commands, directory) = fixture(true, true)?;
    let apk = directory.path().join("sample.apk");
    fs::write(&apk, b"PK\x03\x04payload")?;
    let path = apk.to_string_lossy().to_string();
    for _ in 0..MAX_PENDING {
        let _: String = zbus::block_on(proxy(&client)?.call("InstallPackage", &(path.clone(),)))?;
    }
    assert_eq!(service.pending_count(), MAX_PENDING);
    let rejected = zbus::block_on(proxy(&client)?.call::<_, _, String>("InstallPackage", &(path,)));
    assert!(rejected.is_err());
    assert_eq!(
        fs::read_dir(directory.path().join("install"))?.count(),
        MAX_PENDING
    );
    service.bridge_lost(BridgeRoute::User)?;
    assert_eq!(service.pending_count(), 0);
    assert_eq!(fs::read_dir(directory.path().join("install"))?.count(), 0);
    Ok(())
}

#[test]
fn invalid_apk_is_a_failure_map_and_stale_apks_are_reaped() -> TestResult<()> {
    let (_bus, client, _service, commands, directory) = fixture(true, true)?;
    let invalid = directory.path().join("notes.txt");
    fs::write(&invalid, b"not an apk")?;
    let result: VariantMap = zbus::block_on(
        proxy(&client)?.call("InspectPackage", &(invalid.to_string_lossy().to_string(),)),
    )?;
    assert!(!map_bool(&result, "ok"));
    assert_eq!(map_string(&result, "message"), "not an Android app");
    assert!(commands.lock().unwrap().is_empty());

    let bus = PrivateBus::start()?;
    let install = tempfile::tempdir()?;
    fs::write(install.path().join("orphan.apk"), b"old")?;
    fs::write(install.path().join("keep.txt"), b"keep")?;
    let _service = Packages::new(
        bus.connection()?,
        install.path(),
        Arc::new(|_, _| false),
        Arc::new(|_| Box::pin(async { false })),
    )?;
    assert!(!install.path().join("orphan.apk").exists());
    assert!(install.path().join("keep.txt").exists());
    Ok(())
}

#[test]
fn progress_finished_and_package_changed_arrive_on_private_bus() -> TestResult<()> {
    let (bus, client, service, commands, directory) = fixture(true, true)?;
    let listener = bus.connection()?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .path(OBJECT_PATH)
        .interface(INTERFACE)
        .build();
    let mut signals = futures_lite::stream::block_on(zbus::block_on(
        MessageStream::for_match_rule(rule?, &listener, std::num::NonZeroUsize::new(8)),
    )?);
    let (tx, rx) = mpsc::sync_channel(3);
    thread::spawn(move || {
        for signal in signals.by_ref().take(3) {
            let Ok(signal) = signal else {
                break;
            };
            let member = signal
                .header()
                .member()
                .map(|name| name.as_str().to_owned());
            if tx.send((member, signal)).is_err() {
                break;
            }
        }
    });
    let apk = directory.path().join("sample.apk");
    fs::write(&apk, b"PK\x03\x04payload")?;
    let job: String = zbus::block_on(
        proxy(&client)?.call("InstallPackage", &(apk.to_string_lossy().to_string(),)),
    )?;
    assert_eq!(command(&commands, 0).0, BridgeRoute::User);
    service.handle(
        BridgeRoute::System,
        "install_progress",
        json!({"id":job, "percent":75}).as_object().unwrap(),
    )?;
    service.handle(
        BridgeRoute::User,
        "install_progress",
        json!({"id":job, "percent":250}).as_object().unwrap(),
    )?;
    service.handle(
        BridgeRoute::User,
        "install_result",
        json!({"id":job, "ok":true, "pkg":"org.example.app"})
            .as_object()
            .unwrap(),
    )?;
    service.handle(
        BridgeRoute::System,
        "package_changed",
        json!({"pkg":"org.example.app", "change":"installed"})
            .as_object()
            .unwrap(),
    )?;
    service.handle(
        BridgeRoute::User,
        "package_changed",
        json!({"pkg":"org.example.app", "change":"installed"})
            .as_object()
            .unwrap(),
    )?;
    let mut received = Vec::new();
    for _ in 0..3 {
        received.push(rx.recv_timeout(Duration::from_secs(2))?);
    }
    assert_eq!(received[0].0.as_deref(), Some("Progress"));
    let (signal_job, percent): (String, i32) = received[0].1.body().deserialize()?;
    assert_eq!((signal_job, percent), (job.clone(), 100));
    assert_eq!(received[1].0.as_deref(), Some("Finished"));
    let (signal_job, ok, package, _message): (String, bool, String, String) =
        received[1].1.body().deserialize()?;
    assert_eq!(
        (signal_job, ok, package),
        (job, true, "org.example.app".to_owned())
    );
    assert_eq!(received[2].0.as_deref(), Some("PackageChanged"));
    let (package, change): (String, String) = received[2].1.body().deserialize()?;
    assert_eq!(
        (package, change),
        ("org.example.app".to_owned(), "installed".to_owned())
    );
    Ok(())
}
