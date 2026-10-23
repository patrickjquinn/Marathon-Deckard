//! Root-authorized local-image initializer bus contract.

use crate::command::{CommandSpec, ProcessRunner, Runner};
use crate::dbus::{Caller, caller};
use crate::initializer::{LocalImageInit, NodeInventory, ProductionInit};
use crate::lxc::{Lxc, State};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::connection::Connection;
use zbus::fdo;
use zbus::message::Header;

pub const OBJECT_PATH: &str = "/Initializer";
const SCRATCH_ROOT: &str = "/tmp/deckard-init-scratch";
const DEADLINE: Duration = Duration::from_secs(10);
const PRODUCTION_WORK: &str = "/var/lib/waydroid";
const PRODUCTION_TEMPLATES: &str = "/usr/lib/waydroid/data/configs";

enum InitRequest {
    Scratch(Box<LocalImageInit>),
    Production,
}

struct Job {
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub struct InitializerService {
    job: Arc<Mutex<Option<Arc<Job>>>>,
}

impl InitializerService {
    pub fn new() -> Self {
        Self::default()
    }

    async fn init_authorized(
        &self,
        params: HashMap<String, String>,
        who: Caller,
        conn: Connection,
    ) -> fdo::Result<()> {
        require_root(who)?;
        let init = parse_request(&params)?;
        let job = Arc::new(Job {
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        {
            let mut active = self
                .job
                .lock()
                .map_err(|_| fdo::Error::Failed("initializer state poisoned".into()))?;
            if active.is_some() {
                return Err(fdo::Error::Failed(
                    "initialization already in progress".into(),
                ));
            }
            *active = Some(job.clone());
        }
        let state = self.job.clone();
        let worker_job = job.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("deckard-local-init".into())
            .spawn(move || {
                let deadline = Instant::now() + DEADLINE;
                let _ = emit(&conn, "ProgressChanged", &("Preparing local images",));
                let result = NodeInventory::from_host().and_then(|inventory| match init {
                    InitRequest::Scratch(init) => init
                        .prepare_local_cancellable(
                            &inventory,
                            &worker_job.cancelled,
                            Some(deadline),
                        )
                        .map(|_| ()),
                    InitRequest::Production => {
                        let init = production_request()
                            .map_err(|error| crate::Error::Invalid(error.to_string()))?;
                        let lxc = Lxc::new(init.work.join("lxc"), "waydroid".into())?;
                        if lxc.root.join("waydroid/config").exists()
                            && lxc.status(&mut ProcessRunner)? != State::Stopped
                        {
                            return Err(crate::Error::Invalid(
                                "stop the Waydroid container before initialization".into(),
                            ));
                        }
                        init.initialize_stopped(&inventory, &worker_job.cancelled, Some(deadline))
                    }
                });
                let signal = match result {
                    Ok(_) => "Finished",
                    Err(error) => {
                        let _ = emit(&conn, "ProgressChanged", &(error.to_string(),));
                        "Interrupted"
                    }
                };
                let _ = emit(&conn, signal, &());
                if let Ok(mut active) = state.lock()
                    && active
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &worker_job))
                {
                    *active = None;
                }
            })
        {
            let mut active = self
                .job
                .lock()
                .map_err(|_| fdo::Error::Failed("initializer state poisoned".into()))?;
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &job))
            {
                *active = None;
            }
            return Err(fdo::Error::Failed(format!(
                "unable to start initializer: {error}"
            )));
        }
        Ok(())
    }

    fn cancel_authorized(&self, who: Caller) -> fdo::Result<()> {
        require_root(who)?;
        if let Some(job) = self
            .job
            .lock()
            .map_err(|_| fdo::Error::Failed("initializer state poisoned".into()))?
            .as_ref()
        {
            job.cancelled.store(true, Ordering::Release);
        }
        // Never wait on a filesystem operation or block the bus executor.
        Ok(())
    }
}

fn emit<B: serde::ser::Serialize + zbus::zvariant::DynamicType>(
    conn: &Connection,
    signal: &str,
    body: &B,
) -> zbus::Result<()> {
    async_io::block_on(conn.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        "id.waydro.Initializer",
        signal,
        body,
    ))
}

fn require_root(who: Caller) -> fdo::Result<()> {
    if who.uid == 0 {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(
            "initializer requires root; Polkit authorization is not implemented".into(),
        ))
    }
}

fn parse_request(params: &HashMap<String, String>) -> fdo::Result<InitRequest> {
    if params.len() == 3
        && params.contains_key("system_channel")
        && params.contains_key("vendor_channel")
        && params.contains_key("system_type")
    {
        for key in ["system_channel", "vendor_channel", "system_type"] {
            let value = &params[key];
            if value.len() > 2048 || value.chars().any(char::is_control) {
                return Err(fdo::Error::InvalidArgs(format!("invalid {key}")));
            }
        }
        return Ok(InitRequest::Production);
    }
    parse_scratch_request(params).map(|init| InitRequest::Scratch(Box::new(init)))
}

fn production_request() -> fdo::Result<ProductionInit> {
    let work = PathBuf::from(PRODUCTION_WORK);
    trusted_directory(&work)?;
    let images = [
        "/etc/waydroid-extra/images",
        "/usr/share/waydroid-extra/images",
        "/var/lib/waydroid/images",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.join("system.img").exists() && path.join("vendor.img").exists())
    .ok_or_else(|| {
        fdo::Error::NotSupported(
            "local system.img and vendor.img are required; OTA is not supported".into(),
        )
    })?;
    trusted_directory(&images)?;
    let templates = PathBuf::from(PRODUCTION_TEMPLATES);
    trusted_directory(&templates)?;
    let arch = std::env::consts::ARCH.to_owned();
    if !matches!(arch.as_str(), "aarch64" | "x86_64") {
        return Err(fdo::Error::NotSupported(format!(
            "unsupported architecture: {arch}"
        )));
    }
    let version = ProcessRunner
        .run(CommandSpec::new("lxc-info", ["--version"]).with_timeout(Duration::from_secs(3)))
        .map_err(|error| fdo::Error::Failed(format!("LXC version unavailable: {error}")))?;
    let lxc_major = version
        .trim()
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or_else(|| fdo::Error::Failed("invalid LXC version".into()))?;
    let binder = installed_node(&["binder", "anbox-binder", "puddlejumper", "bonder"])?;
    let vndbinder = installed_node(&[
        "vndbinder",
        "anbox-vndbinder",
        "vndpuddlejumper",
        "vndbonder",
    ])?;
    let hwbinder = installed_node(&["hwbinder", "anbox-hwbinder", "hwpuddlejumper", "hwbonder"])?;
    Ok(ProductionInit {
        work,
        images,
        templates,
        arch,
        lxc_major,
        binder,
        vndbinder,
        hwbinder,
        ota: None,
    })
}

fn installed_node(candidates: &[&str]) -> fdo::Result<String> {
    candidates
        .iter()
        .find(|name| {
            fs::metadata(Path::new("/dev").join(name)).is_ok_and(|metadata| {
                std::os::unix::fs::FileTypeExt::is_char_device(&metadata.file_type())
            })
        })
        .map(|name| (*name).to_owned())
        .ok_or_else(|| {
            fdo::Error::NotSupported(format!("missing Binder device: {}", candidates[0]))
        })
}

fn parse_scratch_request(params: &HashMap<String, String>) -> fdo::Result<LocalImageInit> {
    if params.len() > 16 || params.values().any(|value| value.len() > 4096) {
        return Err(fdo::Error::InvalidArgs(
            "initializer request too large".into(),
        ));
    }
    if params.get("mode").map(String::as_str) != Some("prepare-local-scratch") {
        return Err(fdo::Error::NotSupported(
            "standard Init requires OTA, configuration, mounts, and restart parity".into(),
        ));
    }
    const KEYS: [&str; 11] = [
        "mode",
        "work",
        "images",
        "templates",
        "name",
        "bridge",
        "arch",
        "lxc_major",
        "binder",
        "vndbinder",
        "hwbinder",
    ];
    if params.len() != KEYS.len() || params.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return Err(fdo::Error::InvalidArgs(
            "prepare-local-scratch requires exactly its eleven fields".into(),
        ));
    }
    let field = |key: &str| -> fdo::Result<String> {
        params
            .get(key)
            .cloned()
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("missing {key}")))
    };
    let work = PathBuf::from(field("work")?);
    let scratch = Path::new(SCRATCH_ROOT);
    let canonical_scratch = fs::canonicalize(scratch)
        .map_err(|error| fdo::Error::InvalidArgs(format!("scratch root unavailable: {error}")))?;
    if canonical_scratch != scratch {
        return Err(fdo::Error::InvalidArgs("scratch root is redirected".into()));
    }
    trusted_directory(scratch)?;
    let canonical_work = fs::canonicalize(&work)
        .map_err(|error| fdo::Error::InvalidArgs(format!("work directory unavailable: {error}")))?;
    if canonical_work.parent() != Some(scratch) || !canonical_work.is_dir() {
        return Err(fdo::Error::InvalidArgs(
            "work must be an existing direct child of the scratch root".into(),
        ));
    }
    trusted_directory(&canonical_work)?;
    trusted_directory(&canonical_work.join("lxc"))?;
    let images = scratch_child(&canonical_work, &field("images")?)?;
    let templates = scratch_child(&canonical_work, &field("templates")?)?;
    let lxc_major = field("lxc_major")?
        .parse()
        .map_err(|_| fdo::Error::InvalidArgs("invalid lxc_major".into()))?;
    Ok(LocalImageInit {
        work: canonical_work,
        images,
        templates,
        name: field("name")?,
        bridge: field("bridge")?,
        arch: field("arch")?,
        lxc_major,
        binder: field("binder")?,
        vndbinder: field("vndbinder")?,
        hwbinder: field("hwbinder")?,
    })
}

fn scratch_child(work: &Path, requested: &str) -> fdo::Result<PathBuf> {
    let path = fs::canonicalize(requested).map_err(|error| {
        fdo::Error::InvalidArgs(format!("input directory unavailable: {error}"))
    })?;
    if path.parent() != Some(work) {
        return Err(fdo::Error::InvalidArgs(
            "images and templates must be direct children of scratch work".into(),
        ));
    }
    trusted_directory(&path)?;
    Ok(path)
}

fn trusted_directory(path: &Path) -> fdo::Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| fdo::Error::InvalidArgs(error.to_string()))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(fdo::Error::InvalidArgs(
            "scratch directories must be owned by the daemon and not writable by others".into(),
        ));
    }
    Ok(())
}

#[zbus::interface(name = "id.waydro.Initializer")]
impl InitializerService {
    async fn init(
        &self,
        params: HashMap<String, String>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let who = caller(conn, &header).await?;
        self.init_authorized(params, who, conn.clone()).await
    }

    async fn cancel(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.cancel_authorized(caller(conn, &header).await?)
    }

    #[zbus(signal)]
    async fn progress_changed(
        context: &zbus::object_server::SignalEmitter<'_>,
        message: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn finished(context: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn interrupted(context: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::BUS_NAME;
    use futures_lite::{StreamExt, future};
    use std::error::Error;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use zbus::connection::Builder;

    struct Bus(Child);

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> Result<(Bus, String), Box<dyn Error>> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no private bus stdout")?;
        let bus = Bus(child);
        let mut address = String::new();
        BufReader::new(stdout).read_line(&mut address)?;
        if address.trim().is_empty() {
            return Err("private bus has no address".into());
        }
        Ok((bus, address.trim().to_owned()))
    }

    fn scratch_fields(work: &Path, images: &Path, templates: &Path) -> HashMap<String, String> {
        HashMap::from([
            ("mode".into(), "prepare-local-scratch".into()),
            ("work".into(), work.display().to_string()),
            ("images".into(), images.display().to_string()),
            ("templates".into(), templates.display().to_string()),
            ("name".into(), "deckard".into()),
            ("bridge".into(), "deckard0".into()),
            ("arch".into(), "aarch64".into()),
            ("lxc_major".into(), "4".into()),
            ("binder".into(), "binder".into()),
            ("vndbinder".into(), "vndbinder".into()),
            ("hwbinder".into(), "hwbinder".into()),
        ])
    }

    #[test]
    fn private_bus_matches_installed_initializer_signatures() -> Result<(), Box<dyn Error>> {
        let (_bus, address) = private_bus()?;
        async_io::block_on(async {
            let server = Builder::address(address.as_str())?
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, InitializerService::new())?
                .build()
                .await?;
            let client = Builder::address(address.as_str())?.build().await?;
            let introspect = zbus::Proxy::new(
                &client,
                BUS_NAME,
                OBJECT_PATH,
                "org.freedesktop.DBus.Introspectable",
            )
            .await?;
            let xml: String = introspect.call("Introspect", &()).await?;
            assert!(xml.contains("interface name=\"id.waydro.Initializer\""));
            assert!(xml.contains("name=\"Init\""));
            assert!(xml.contains("type=\"a{ss}\""));
            for name in ["Cancel", "ProgressChanged", "Finished", "Interrupted"] {
                assert!(xml.contains(&format!("name=\"{name}\"")));
            }
            let proxy =
                zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, "id.waydro.Initializer").await?;
            let legacy = HashMap::from([
                (
                    "system_channel".to_owned(),
                    "https://example.invalid".to_owned(),
                ),
                (
                    "vendor_channel".to_owned(),
                    "https://example.invalid".to_owned(),
                ),
                ("system_type".to_owned(), "VANILLA".to_owned()),
                ("unexpected".to_owned(), "rejected".to_owned()),
            ]);
            let result: zbus::Result<()> = proxy.call("Init", &(legacy,)).await;
            assert!(result.is_err());
            let uid = rustix::process::geteuid().as_raw();
            let cancel: zbus::Result<()> = proxy.call("Cancel", &()).await;
            assert_eq!(cancel.is_ok(), uid == 0);
            drop(client);
            drop(server);
            Ok::<(), zbus::Error>(())
        })?;
        Ok(())
    }

    #[test]
    fn authorization_and_standard_init_dispatch() {
        let service = InitializerService::new();
        assert!(
            service
                .cancel_authorized(Caller { uid: 1000, pid: 9 })
                .is_err()
        );
        assert!(service.cancel_authorized(Caller { uid: 0, pid: 1 }).is_ok());
        let params = HashMap::from([
            ("system_channel".into(), "default".into()),
            ("vendor_channel".into(), "default".into()),
            ("system_type".into(), "VANILLA".into()),
        ]);
        assert!(matches!(
            parse_request(&params),
            Ok(InitRequest::Production)
        ));
        let mut malformed = params;
        malformed.insert("system_type".into(), "VANILLA\nBAD".into());
        assert!(parse_request(&malformed).is_err());
    }

    #[test]
    fn scratch_job_completes_without_touching_production() -> Result<(), Box<dyn Error>> {
        let scratch = Path::new(SCRATCH_ROOT);
        fs::create_dir_all(scratch)?;
        let root = scratch.join(format!(
            "test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(root.join("lxc"))?;
        let images = root.join("images");
        let templates = root.join("templates");
        fs::create_dir(&images)?;
        fs::create_dir(&templates)?;
        for image in ["system.img", "vendor.img"] {
            let mut bytes = vec![0; 1082];
            bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
            fs::write(images.join(image), bytes)?;
        }
        fs::write(
            templates.join("config_base"),
            b"lxc.rootfs.path = /var/lib/waydroid/rootfs\nlxc.arch = LXCARCH\nlxc.include = /var/lib/waydroid/lxc/waydroid/config_nodes\nlxc.include = /var/lib/waydroid/lxc/waydroid/config_session\n",
        )?;
        fs::write(
            templates.join("config_3"),
            b"lxc.uts.name = waydroid\nlxc.seccomp.profile = /var/lib/waydroid/lxc/waydroid/waydroid.seccomp\nlxc.net.0.link = waydroid0\n",
        )?;
        fs::write(templates.join("waydroid.seccomp"), b"2\nblacklist\n")?;
        let (_bus, address) = private_bus()?;
        let result = async_io::block_on(async {
            let server = Builder::address(address.as_str())?
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, InitializerService::new())?
                .build()
                .await?;
            let client = Builder::address(address.as_str())?.build().await?;
            let service = InitializerService::new();
            let proxy =
                zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, "id.waydro.Initializer").await?;
            let mut finished = proxy.receive_signal("Finished").await?;
            service
                .init_authorized(
                    scratch_fields(&root, &images, &templates),
                    Caller { uid: 0, pid: 1 },
                    server.clone(),
                )
                .await
                .map_err(|error| zbus::Error::Failure(error.to_string()))?;
            let received = future::or(async { finished.next().await.is_some() }, async {
                async_io::Timer::after(Duration::from_secs(3)).await;
                false
            })
            .await;
            assert!(received, "initializer did not emit Finished");
            assert!(root.join("lxc/deckard/config").exists());
            drop(client);
            drop(server);
            Ok::<(), zbus::Error>(())
        });
        fs::remove_dir_all(&root)?;
        result?;
        Ok(())
    }
}
