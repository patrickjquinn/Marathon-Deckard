//! `org.marathonos.Deckard1`: the Android runtime and its apps, for Marathon
//! Settings.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use async_io::Timer;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use deckard_core::runtime_state::{ResetError, State, UnitCall};
use deckard_core::{ids, png_guard};
use serde::Serialize;
use serde_json::{Map, Value, json};
use zbus::message::{Flags, Header, Message};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::DynamicType;
use zbus::{Connection, DBusError, interface};

use crate::auth;
use crate::contract::{self, Fixed, VariantMap};
use crate::intents::{Endpoint, NAVIGATION};
use crate::manage::Manage;
use crate::runtime::{self, Runtime};

pub use deckard_packages::{BUS_NAME, OBJECT_PATH};
/// The Settings interface.
pub const INTERFACE: &str = "org.marathonos.Deckard1";
const SYSTEMD: Endpoint<'static> = (
    "org.freedesktop.systemd1",
    runtime::MANAGER_PATH,
    runtime::MANAGER,
);
const RESET_HELPER: &str = "/usr/libexec/deckard-reset";
const MAX_ICON_BYTES: usize = 512 * 1024;

/// The errors of Deckard's Settings and Share interfaces.
#[derive(Debug, DBusError)]
#[zbus(prefix = "org")]
pub enum ApiError {
    #[zbus(name = "freedesktop.DBus.Error.InvalidArgs")]
    InvalidArgs(String),
    #[zbus(name = "freedesktop.DBus.Error.AccessDenied")]
    AccessDenied(String),
    #[zbus(name = "marathonos.Deckard1.Error.Failed")]
    Failed(String),
}

/// A Settings or Share method's result.
pub type Result<T> = std::result::Result<T, ApiError>;

fn invalid() -> ApiError {
    ApiError::InvalidArgs("Invalid arguments".into())
}

fn failed(why: impl ToString) -> ApiError {
    ApiError::Failed(why.to_string())
}

/// Refuses the call unless its arguments are `valid`.
fn valid(valid: bool) -> Result<()> {
    if valid { Ok(()) } else { Err(invalid()) }
}

/// `png` as a `data:` URI.
pub(crate) fn png_uri(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(png))
}

/// Calls `method` without waiting for a reply.
async fn notify<B>(bus: &Connection, to: Endpoint<'_>, method: &str, body: &B) -> zbus::Result<()>
where
    B: Serialize + DynamicType,
{
    let (service, path, interface) = to;
    let call = Message::method_call(path, method)?.destination(service)?;
    let call = call
        .interface(interface)?
        .with_flags(Flags::NoReplyExpected)?;
    bus.send(&call.build(body)?).await
}

/// `org.marathonos.Deckard1` at [`OBJECT_PATH`].
#[derive(Clone)]
pub struct SettingsApi {
    pub bus: Connection,
    pub runtime: Runtime,
    pub manage: Manage,
    pub icon_dirs: Vec<PathBuf>,
}

impl SettingsApi {
    async fn allowed(&self, header: Header<'_>) -> Result<()> {
        if auth::settings(&self.bus, header).await {
            return Ok(());
        }
        Err(ApiError::AccessDenied("Not allowed for this caller".into()))
    }

    /// DeckardSystem's answer to manage `op`; `changed` names the app it
    /// changed.
    async fn request(
        &self,
        op: &str,
        args: Value,
        changed: Option<&str>,
        header: Header<'_>,
    ) -> Result<Map<String, Value>> {
        self.allowed(header).await?;
        let reply = self.manage.request(op, crate::object(args)).await;
        let reply = reply.map_err(ApiError::Failed)?;
        if let Some(pkg) = changed {
            self.emit("AppChanged", &(pkg,)).await;
        }
        Ok(reply)
    }

    async fn emit<B: Serialize + DynamicType>(&self, member: &str, body: &B) {
        let bus = &self.bus;
        let signal = bus.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, member, body);
        let _ = signal.await;
    }

    /// Changes the app `pkg` through manage `op`.
    async fn change(&self, op: &str, pkg: &str, mut args: Value, header: Header<'_>) -> Result<()> {
        valid(ids::is_package_name(pkg))?;
        args["pkg"] = json!(pkg);
        self.request(op, args, Some(pkg), header).await.map(drop)
    }

    async fn unit(&self, call: UnitCall, header: Header<'_>) -> Result<()> {
        self.allowed(header).await?;
        let method = match call {
            UnitCall::Start => "StartUnit",
            UnitCall::Stop => "StopUnit",
            UnitCall::Restart => "RestartUnit",
        };
        let body = (runtime::UNIT, "replace");
        let sent = notify(&self.bus, SYSTEMD, method, &body).await;
        sent.map_err(failed)
    }

    /// Stops Android, waiting at most 90 seconds for it.
    async fn stop_android(&self) -> Result<()> {
        if self.runtime.state() == State::Stopped {
            return Ok(());
        }
        let stop = runtime::call_unit(&self.bus, UnitCall::Stop).await;
        stop.map_err(failed)?;
        let deadline = Instant::now() + Duration::from_secs(90);
        while self.runtime.state() != State::Stopped {
            if Instant::now() >= deadline {
                return Err(failed("Android didn't stop"));
            }
            Timer::after(Duration::from_millis(100)).await;
        }
        Ok(())
    }
}

/// The icon at `path`, sanitised.
fn icon(path: &Path) -> Option<Vec<u8>> {
    if fs::metadata(path).ok()?.len() > MAX_ICON_BYTES as u64 {
        return None;
    }
    png_guard::sanitize(&fs::read(path).ok()?, MAX_ICON_BYTES, 192)
}

/// Runs the reset helper for at most five minutes; whether it succeeded.
fn run_helper(helper: &[PathBuf]) -> bool {
    let Some((program, args)) = helper.split_first() else {
        return false;
    };
    let mut command = Command::new(program);
    let Ok(mut child) = command.args(args).stdin(Stdio::null()).spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return false,
        }
    }
    if child.kill().is_ok() {
        let _ = child.wait();
    }
    false
}

#[interface(name = "org.marathonos.Deckard1")]
impl SettingsApi {
    fn get_runtime(&self) -> VariantMap {
        self.runtime.snapshot()
    }

    async fn start(&self, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.unit(UnitCall::Start, header).await
    }

    async fn stop(&self, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.unit(UnitCall::Stop, header).await
    }

    async fn restart(&self, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.unit(UnitCall::Restart, header).await
    }

    async fn set_start_at_login(
        &self,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        self.allowed(header).await?;
        self.runtime.set_start_at_login(enabled).map_err(failed)?;
        self.emit("RuntimeChanged", &(self.runtime.snapshot(),))
            .await;
        Ok(())
    }

    fn app_icon(&self, pkg: &str) -> String {
        if !ids::is_package_name(pkg) {
            return String::new();
        }
        for dir in &self.icon_dirs {
            if let Some(png) = icon(&dir.join(format!("{pkg}.png"))) {
                return png_uri(&png);
            }
        }
        String::new()
    }

    async fn get_app(&self, pkg: &str, #[zbus(header)] header: Header<'_>) -> Result<VariantMap> {
        valid(ids::is_package_name(pkg))?;
        let reply = self.request("app", json!({"pkg": pkg}), None, header);
        Ok(contract::app(&reply.await?))
    }

    // `permission` is a Marathon permission; "notifications" is Android's
    // per-app notification switch.
    async fn set_permission(
        &self,
        pkg: &str,
        permission: &str,
        granted: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        valid(ids::is_package_name(pkg))?;
        let android = contract::permission(permission).ok_or_else(invalid)?;
        let (op, args) = match permission {
            "notifications" => ("notifications", json!({"enabled": granted})),
            _ => ("perm", json!({"permissions": android, "granted": granted})),
        };
        self.change(op, pkg, args, header).await
    }

    async fn get_permission_apps(
        &self,
        permission: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>> {
        let android = contract::permission(permission).ok_or_else(invalid)?;
        let (op, args) = match permission {
            "notifications" => ("notif_apps", json!({})),
            _ => ("perm_apps", json!({"permissions": android})),
        };
        let reply = self.request(op, args, None, header).await?;
        Ok(contract::apps(&reply, Fixed::Shown))
    }

    async fn set_notifications(
        &self,
        pkg: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        let args = json!({"enabled": enabled});
        self.change("notifications", pkg, args, header).await
    }

    async fn set_open_links(
        &self,
        pkg: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        let args = json!({"enabled": enabled});
        self.change("links", pkg, args, header).await
    }

    async fn set_battery(
        &self,
        pkg: &str,
        mode: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        valid(matches!(mode, "unrestricted" | "optimized"))?;
        let args = json!({"mode": mode});
        self.change("battery", pkg, args, header).await
    }

    async fn force_stop(&self, pkg: &str, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.change("stop", pkg, json!({}), header).await
    }

    async fn clear_cache(&self, pkg: &str, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.change("cache", pkg, json!({}), header).await
    }

    async fn clear_data(&self, pkg: &str, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.change("clear", pkg, json!({}), header).await
    }

    async fn get_special_access(
        &self,
        kind: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>> {
        valid(contract::SPECIAL_KINDS.contains(&kind))?;
        let reply = self.request("special", json!({"kind": kind}), None, header);
        Ok(contract::apps(&reply.await?, Fixed::Hidden))
    }

    async fn set_special_access(
        &self,
        kind: &str,
        pkg: &str,
        granted: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        valid(contract::SPECIAL_KINDS.contains(&kind))?;
        let args = json!({"kind": kind, "granted": granted});
        self.change("special_set", pkg, args, header).await
    }

    // microG (docs/microg.md).
    async fn get_google_services(&self, #[zbus(header)] header: Header<'_>) -> Result<VariantMap> {
        let reply = self.request("gms", json!({}), None, header).await?;
        Ok(contract::google(&reply))
    }

    async fn set_google_services(
        &self,
        key: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        valid(matches!(key, "enabled" | "push"))?;
        let args = json!({"key": key, "enabled": enabled});
        self.request("gms_set", args, None, header).await.map(drop)
    }

    // microG's window comes from Android, so the Shell is told to show it.
    async fn open_google_services(
        &self,
        page: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<()> {
        valid(matches!(page, "account" | "selfcheck"))?;
        let args = json!({"page": page});
        self.request("gms_open", args, None, header).await?;
        let body = ("waydroid.com.google.android.gms",);
        let _ = notify(&self.bus, NAVIGATION, "ExpectAppWindow", &body).await;
        Ok(())
    }

    async fn reset_android(&self, #[zbus(header)] header: Header<'_>) -> Result<()> {
        self.allowed(header).await?;
        let helper = match env::var_os("DECKARD_RESET_HELPER").filter(|value| !value.is_empty()) {
            Some(helper) => vec![PathBuf::from(helper)],
            None if Path::new(RESET_HELPER).exists() => vec!["pkexec".into(), RESET_HELPER.into()],
            None => Vec::new(),
        };
        let request = self.runtime.request_reset(!helper.is_empty());
        request.map_err(|error| match error {
            ResetError::AlreadyResetting => failed("Android is already being reset"),
            _ => failed("Resetting Android isn't available"),
        })?;
        if let Err(error) = self.stop_android().await {
            self.runtime.finish_reset();
            return Err(error);
        }
        let success = blocking::unblock(move || run_helper(&helper)).await;
        self.runtime.finish_reset();
        if success {
            return Ok(());
        }
        Err(failed("Resetting Android failed"))
    }

    #[zbus(signal)]
    async fn runtime_changed(emitter: &SignalEmitter<'_>, runtime: VariantMap) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn app_changed(emitter: &SignalEmitter<'_>, pkg: &str) -> zbus::Result<()>;
}
