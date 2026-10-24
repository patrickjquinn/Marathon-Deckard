//! `/Initializer` (`org.marathonos.Deckard.Initializer`): `Init(a{ss})`,
//! `Cancel()` and the `ProgressChanged(s)`, `Finished()` and `Interrupted()`
//! signals.
//!
//! Upstream downloads images from the requested OTA channels. Deckard only
//! initializes from images already on the device, so custom channels are
//! refused and the default ones select nothing. Non-root callers need polkit
//! authorization for [`POLKIT_ACTION`]; unlike upstream there is no exemption
//! for the default channels, because Init restarts the session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use zbus::connection::Connection;
use zbus::fdo;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{DynamicType, Value};

use crate::Result;
use crate::dbus::{Caller, ContainerService};
use crate::drivers;
use crate::initializer::{Host, Images, Init, Instance, PREINSTALLED};

pub const OBJECT_PATH: &str = "/Initializer";
pub const INTERFACE: &str = "org.marathonos.Deckard.Initializer";
pub const POLKIT_ACTION: &str = "org.marathonos.Deckard.Initializer.Init";
const SYSTEM_CHANNEL: &str = "https://ota.waydro.id/system";
const VENDOR_CHANNEL: &str = "https://ota.waydro.id/vendor";
const SYSTEM_TYPES: [&str; 2] = ["VANILLA", "GAPPS"];
const POLKIT_TIMEOUT: Duration = Duration::from_secs(300);
const DEADLINE: Duration = Duration::from_secs(10);

/// Reports one progress line to the caller.
pub type Progress<'a> = dyn Fn(&str) + 'a;
/// One initialization, run on a worker thread; `cancelled` asks it to stop.
pub type Job = dyn Fn(&Progress<'_>, &AtomicBool) -> Result<()> + Send + Sync;

struct Active {
    owner: u32,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct InitializerService {
    active: Arc<Mutex<Option<Arc<Active>>>>,
    job: Arc<Job>,
}

fn poisoned<T>(_: T) -> fdo::Error {
    fdo::Error::Failed("initializer state poisoned".into())
}

impl InitializerService {
    pub fn new(job: Arc<Job>) -> Self {
        Self {
            active: Arc::new(Mutex::new(None)),
            job,
        }
    }

    /// Initializes `/var/lib/deckard`, stopping and restarting `container`'s
    /// session around it.
    #[must_use]
    pub fn production(container: ContainerService) -> Self {
        Self::new(Arc::new(move |progress, cancelled| {
            container.with_stopped(|| initialize(progress, cancelled))
        }))
    }

    fn start(&self, who: Caller, conn: Connection) -> fdo::Result<()> {
        let active = Arc::new(Active {
            owner: who.uid,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        {
            let mut current = self.active.lock().map_err(poisoned)?;
            if current.is_some() {
                return Err(fdo::Error::Failed(
                    "initialization already in progress".into(),
                ));
            }
            *current = Some(Arc::clone(&active));
        }
        let state = Arc::clone(&self.active);
        let job = Arc::clone(&self.job);
        let worker = Arc::clone(&active);
        let spawned = thread::Builder::new()
            .name("deckard-init".into())
            .spawn(move || {
                let progress = |message: &str| emit(&conn, "ProgressChanged", &(message,));
                let signal = match job(&progress, &worker.cancelled) {
                    Ok(()) => "Finished",
                    Err(error) => {
                        progress(&error.to_string());
                        "Interrupted"
                    }
                };
                if let Ok(mut current) = state.lock()
                    && current
                        .as_ref()
                        .is_some_and(|job| Arc::ptr_eq(job, &worker))
                {
                    *current = None;
                }
                emit(&conn, signal, &());
            });
        if let Err(error) = spawned {
            *self.active.lock().map_err(poisoned)? = None;
            return Err(fdo::Error::Failed(format!(
                "unable to start initializer: {error}"
            )));
        }
        Ok(())
    }

    fn cancel_authorized(&self, who: Caller) -> fdo::Result<()> {
        let current = self.active.lock().map_err(poisoned)?;
        let Some(active) = current.as_ref() else {
            return Ok(());
        };
        if who.uid != 0 && who.uid != active.owner {
            return Err(fdo::Error::AccessDenied(
                "only root or the caller of Init may cancel it".into(),
            ));
        }
        // Cooperative: the worker checks before each step and never blocks the bus.
        active.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

fn emit(conn: &Connection, signal: &str, body: &(impl Serialize + DynamicType)) {
    let _ =
        async_io::block_on(conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, signal, body));
}

fn initialize(progress: &Progress<'_>, cancelled: &AtomicBool) -> Result<()> {
    let instance = Instance::production();
    drivers::ensure_binder_nodes()?;
    let host = Host::probe()?;
    let preinstalled: Vec<PathBuf> = PREINSTALLED.iter().map(PathBuf::from).collect();
    let images = Images::find(&preinstalled, &instance.work.join("images"))?;
    progress(&format!(
        "Initializing Android from {}",
        images.path().display()
    ));
    if matches!(images, Images::Installed(_)) {
        progress("Image download is not supported; keeping the installed images");
    }
    let init = Init {
        instance: &instance,
        images,
        host,
    };
    init.run(cancelled, Some(Instant::now() + DEADLINE))
}

/// Accepts upstream's three-field request with the default channels.
fn validate_request(params: &HashMap<String, String>) -> fdo::Result<()> {
    let field = |key: &str| {
        let valid = |value: &&String| value.len() <= 2048 && !value.chars().any(char::is_control);
        let value = params.get(key).filter(valid);
        value.ok_or_else(|| fdo::Error::InvalidArgs(format!("missing or invalid {key}")))
    };
    if params.len() != 3 {
        let message = "Init takes system_channel, vendor_channel and system_type";
        return Err(fdo::Error::InvalidArgs(message.into()));
    }
    let system = field("system_channel")?.trim_end_matches('/');
    let vendor = field("vendor_channel")?.trim_end_matches('/');
    if !SYSTEM_TYPES.contains(&field("system_type")?.as_str()) {
        return Err(fdo::Error::InvalidArgs(
            "system_type must be VANILLA or GAPPS".into(),
        ));
    }
    if (system, vendor) != (SYSTEM_CHANNEL, VENDOR_CHANNEL) {
        let message = "custom OTA channels are not supported: Deckard does not download images";
        return Err(fdo::Error::NotSupported(message.into()));
    }
    Ok(())
}

/// Asks polkit whether the sender may run `POLKIT_ACTION`, letting it prompt.
async fn authorize(conn: &Connection, header: &Header<'_>, who: Caller) -> fdo::Result<()> {
    if who.uid == 0 {
        return Ok(());
    }
    let denied = |error: zbus::Error| fdo::Error::AccessDenied(format!("polkit: {error}"));
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".into()))?;
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Value::from(sender.as_str()))]),
    );
    let details: HashMap<&str, &str> = HashMap::new();
    let allow_interaction = 1u32;
    let body = (subject, POLKIT_ACTION, details, allow_interaction, "");
    let call = conn.call_method(
        Some("org.freedesktop.PolicyKit1"),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "CheckAuthorization",
        &body,
    );
    let timeout = async {
        async_io::Timer::after(POLKIT_TIMEOUT).await;
        Err(zbus::Error::Failure("polkit timed out".into()))
    };
    let reply = futures_lite::future::or(call, timeout)
        .await
        .map_err(denied)?;
    let (authorized, _, _): (bool, bool, HashMap<String, String>) =
        reply.body().deserialize().map_err(denied)?;
    if !authorized {
        return Err(fdo::Error::AccessDenied(
            "polkit: not authorized to initialize Android".into(),
        ));
    }
    Ok(())
}

#[zbus::interface(name = "org.marathonos.Deckard.Initializer")]
impl InitializerService {
    async fn init(
        &self,
        params: HashMap<String, String>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        let who = Caller::of(conn, &header).await?;
        validate_request(&params)?;
        authorize(conn, &header, who).await?;
        self.start(who, conn.clone())
    }

    async fn cancel(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<()> {
        self.cancel_authorized(Caller::of(conn, &header).await?)
    }

    #[zbus(signal)]
    async fn progress_changed(context: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn finished(context: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn interrupted(context: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::BUS_NAME;
    use futures_lite::{StreamExt, future};
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use zbus::connection::Builder;
    use zbus::zvariant::OwnedValue;

    type TestResult = Result<()>;

    struct Bus(Child);

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> Result<(Bus, String)> {
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

    fn request(system_channel: &str) -> HashMap<String, String> {
        HashMap::from([
            ("system_channel".into(), system_channel.into()),
            ("vendor_channel".into(), VENDOR_CHANNEL.into()),
            ("system_type".into(), "VANILLA".into()),
        ])
    }

    struct Polkit {
        allow: bool,
        asked: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl Polkit {
        async fn check_authorization(
            &self,
            subject: (String, HashMap<String, OwnedValue>),
            action: String,
            _details: HashMap<String, String>,
            flags: u32,
            _cancel: String,
        ) -> fdo::Result<(bool, bool, HashMap<String, String>)> {
            assert_eq!(flags, 1);
            assert!(subject.1.contains_key("name"));
            self.asked
                .lock()
                .map_err(|_| fdo::Error::Failed("poisoned".into()))?
                .push((subject.0, action));
            Ok((self.allow, false, HashMap::new()))
        }
    }

    #[test]
    fn requests_follow_upstream_shape_and_refuse_downloads() {
        assert!(validate_request(&request(SYSTEM_CHANNEL)).is_ok());
        assert!(validate_request(&request("https://ota.waydro.id/system/")).is_ok());
        assert!(matches!(
            validate_request(&request("https://example.invalid/system")),
            Err(fdo::Error::NotSupported(_))
        ));
        let mut extra = request(SYSTEM_CHANNEL);
        extra.insert("mode".into(), "prepare-local-scratch".into());
        assert!(matches!(
            validate_request(&extra),
            Err(fdo::Error::InvalidArgs(_))
        ));
        let mut bad_type = request(SYSTEM_CHANNEL);
        bad_type.insert("system_type".into(), "VANILLA\nX".into());
        assert!(validate_request(&bad_type).is_err());
    }

    #[test]
    fn only_root_or_the_initiator_cancels() -> fdo::Result<()> {
        let service = InitializerService::new(Arc::new(|_: &Progress<'_>, _: &AtomicBool| Ok(())));
        assert!(
            service
                .cancel_authorized(Caller { uid: 1000, pid: 9 })
                .is_ok()
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        *service.active.lock().map_err(poisoned)? = Some(Arc::new(Active {
            owner: 1000,
            cancelled: Arc::clone(&cancelled),
        }));
        assert!(
            service
                .cancel_authorized(Caller { uid: 1001, pid: 9 })
                .is_err()
        );
        assert!(!cancelled.load(Ordering::Acquire));
        service.cancel_authorized(Caller { uid: 1000, pid: 9 })?;
        assert!(cancelled.load(Ordering::Acquire));
        Ok(())
    }

    fn served(allow: bool, job: Arc<Job>) -> TestResult {
        let (_bus, address) = private_bus()?;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let outcome = async_io::block_on(async {
            let _polkit = Builder::address(address.as_str())?
                .name("org.freedesktop.PolicyKit1")?
                .serve_at(
                    "/org/freedesktop/PolicyKit1/Authority",
                    Polkit {
                        allow,
                        asked: Arc::clone(&asked),
                    },
                )?
                .build()
                .await?;
            let _server = Builder::address(address.as_str())?
                .name(BUS_NAME)?
                .serve_at(OBJECT_PATH, InitializerService::new(job))?
                .build()
                .await?;
            let client = Builder::address(address.as_str())?.build().await?;
            let proxy = zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, INTERFACE).await?;
            let mut progress = proxy.receive_signal("ProgressChanged").await?;
            let mut finished = proxy.receive_signal("Finished").await?;
            let mut interrupted = proxy.receive_signal("Interrupted").await?;
            let refused: zbus::Result<()> = proxy
                .call("Init", &(request("https://example.invalid"),))
                .await;
            assert!(refused.is_err());
            let reply: zbus::Result<()> = proxy.call("Init", &(request(SYSTEM_CHANNEL),)).await;
            if reply.is_err() {
                return Ok::<_, zbus::Error>(None);
            }
            let done = future::or(
                async {
                    future::or(async { finished.next().await.map(|_| "Finished") }, async {
                        interrupted.next().await.map(|_| "Interrupted")
                    })
                    .await
                },
                async {
                    async_io::Timer::after(Duration::from_secs(5)).await;
                    None
                },
            )
            .await;
            let line = progress
                .next()
                .await
                .and_then(|message| message.body().deserialize::<String>().ok());
            Ok(done.map(|signal| (signal, line)))
        })?;
        let uid = rustix::process::geteuid().as_raw();
        let asked = asked.lock().map_err(|_| "poisoned")?.clone();
        if uid == 0 {
            assert!(asked.is_empty());
        } else {
            assert_eq!(
                asked,
                [("system-bus-name".to_owned(), POLKIT_ACTION.to_owned())]
            );
        }
        match (allow || uid == 0, outcome) {
            (false, None) => Ok(()),
            (true, Some((signal, line))) => {
                let expected = if line.as_deref() == Some("step") {
                    "Finished"
                } else {
                    "Interrupted"
                };
                assert_eq!(signal, expected);
                Ok(())
            }
            (allowed, outcome) => Err(format!("allowed {allowed}, outcome {outcome:?}").into()),
        }
    }

    #[test]
    fn private_bus_init_needs_polkit_and_signals_the_outcome() -> TestResult {
        served(
            false,
            Arc::new(|progress: &Progress<'_>, _: &AtomicBool| {
                progress("step");
                Ok(())
            }),
        )?;
        served(
            true,
            Arc::new(|progress: &Progress<'_>, _: &AtomicBool| {
                progress("step");
                Ok(())
            }),
        )?;
        served(
            true,
            Arc::new(|_: &Progress<'_>, _: &AtomicBool| Err("images missing".into())),
        )
    }

    #[test]
    fn introspection_matches_installed_signatures() -> TestResult {
        use zbus::object_server::Interface;
        let service = InitializerService::new(Arc::new(|_: &Progress<'_>, _: &AtomicBool| Ok(())));
        let mut xml = String::new();
        service.introspect_to_writer(&mut xml, 0);
        assert!(xml.contains("interface name=\"org.marathonos.Deckard.Initializer\""));
        assert!(xml.contains("type=\"a{ss}\""));
        for name in [
            "Init",
            "Cancel",
            "ProgressChanged",
            "Finished",
            "Interrupted",
        ] {
            assert!(xml.contains(&format!("name=\"{name}\"")), "{name}");
        }
        assert!(xml.contains("<arg name=\"message\" type=\"s\""));
        Ok(())
    }
}
