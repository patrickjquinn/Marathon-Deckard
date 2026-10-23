use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use deckard_core::ids::PackageName;
use deckard_core::runtime_state::{ResetError, State, UnitCall};
use serde_json::{Map, Value, json};
use zbus::message::{Flags, Header, Message};
use zbus::names::ErrorName;
use zbus::zvariant::OwnedValue;
use zbus::{DBusError, fdo, interface};

use crate::auth;
use crate::contract::{self, VariantMap};
use crate::manage::Manage;
use crate::runtime::{self, Runtime};

pub const BUS_NAME: &str = "org.marathonos.Deckard";
pub const OBJECT_PATH: &str = "/org/marathonos/Deckard";
pub const INTERFACE: &str = "org.marathonos.Deckard1";
const FAILED: &str = "org.marathonos.Deckard1.Error.Failed";

#[derive(Debug)]
pub enum ApiError {
    Standard(fdo::Error),
    Failed(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard(e) => e.fmt(f),
            Self::Failed(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for ApiError {}
impl DBusError for ApiError {
    fn name(&self) -> ErrorName<'_> {
        match self {
            Self::Standard(e) => e.name(),
            Self::Failed(_) => ErrorName::try_from(FAILED).unwrap_or_else(|_| unreachable!()),
        }
    }
    fn description(&self) -> Option<&str> {
        match self {
            Self::Standard(e) => e.description(),
            Self::Failed(e) => Some(e),
        }
    }
    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<Message> {
        match self {
            Self::Standard(e) => e.create_reply(call),
            Self::Failed(message) => Message::error(call, self.name())?.build(&(message,)),
        }
    }
}

fn invalid() -> ApiError {
    ApiError::Standard(fdo::Error::InvalidArgs("Invalid arguments".into()))
}
fn denied() -> ApiError {
    ApiError::Standard(fdo::Error::AccessDenied(
        "Not allowed for this caller".into(),
    ))
}
fn failed(message: impl Into<String>) -> ApiError {
    ApiError::Failed(message.into())
}
fn package(value: &str) -> Result<&str, ApiError> {
    PackageName::try_from(value).map_err(|_| invalid())?;
    Ok(value)
}
fn choice<'a>(value: &'a str, choices: &[&str]) -> Result<&'a str, ApiError> {
    if choices.contains(&value) {
        Ok(value)
    } else {
        Err(invalid())
    }
}
fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[derive(Clone)]
pub struct SettingsApi {
    pub bus: zbus::Connection,
    pub runtime: Runtime,
    pub manage: Manage,
    pub icon_dirs: Vec<PathBuf>,
}

impl SettingsApi {
    async fn allowed(&self, header: Header<'_>) -> Result<(), ApiError> {
        if auth::settings(&self.bus, header).await {
            Ok(())
        } else {
            Err(denied())
        }
    }

    async fn request(
        &self,
        op: &str,
        args: Value,
        changed: Option<&str>,
        header: Header<'_>,
    ) -> Result<Map<String, Value>, ApiError> {
        self.allowed(header).await?;
        let reply = self
            .manage
            .request(op, object(args))
            .await
            .map_err(failed)?;
        if let Some(pkg) = changed {
            let _ = self
                .bus
                .emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "AppChanged", &(pkg,))
                .await;
        }
        Ok(reply)
    }

    async fn unit(&self, call: UnitCall, header: Header<'_>) -> Result<(), ApiError> {
        self.allowed(header).await?;
        let method = match call {
            UnitCall::Start => "StartUnit",
            UnitCall::Stop => "StopUnit",
            UnitCall::Restart => "RestartUnit",
        };
        let message = Message::method_call(runtime::MANAGER_PATH, method)
            .and_then(|builder| builder.destination("org.freedesktop.systemd1"))
            .and_then(|builder| builder.interface(runtime::MANAGER))
            .and_then(|builder| builder.with_flags(Flags::NoReplyExpected))
            .and_then(|builder| builder.build(&(runtime::UNIT, "replace")))
            .map_err(|e| failed(e.to_string()))?;
        self.bus
            .send(&message)
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(())
    }
}

#[interface(name = "org.marathonos.Deckard1")]
impl SettingsApi {
    #[zbus(name = "GetRuntime")]
    fn get_runtime(&self) -> HashMap<String, OwnedValue> {
        self.runtime.snapshot()
    }

    #[zbus(name = "Start")]
    async fn start(&self, #[zbus(header)] header: Header<'_>) -> Result<(), ApiError> {
        self.unit(UnitCall::Start, header).await
    }
    #[zbus(name = "Stop")]
    async fn stop(&self, #[zbus(header)] header: Header<'_>) -> Result<(), ApiError> {
        self.unit(UnitCall::Stop, header).await
    }
    #[zbus(name = "Restart")]
    async fn restart(&self, #[zbus(header)] header: Header<'_>) -> Result<(), ApiError> {
        self.unit(UnitCall::Restart, header).await
    }
    #[zbus(name = "SetStartAtLogin")]
    async fn set_start_at_login(
        &self,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        self.allowed(header).await?;
        self.runtime
            .set_start_at_login(enabled)
            .map_err(|e| failed(e.to_string()))?;
        let _ = self
            .bus
            .emit_signal(
                None::<&str>,
                OBJECT_PATH,
                INTERFACE,
                "RuntimeChanged",
                &(self.runtime.snapshot(),),
            )
            .await;
        Ok(())
    }

    #[zbus(name = "AppIcon")]
    fn app_icon(&self, pkg: &str) -> String {
        if package(pkg).is_err() {
            return String::new();
        }
        for dir in &self.icon_dirs {
            let path = dir.join(format!("{pkg}.png"));
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > 512 * 1024 {
                continue;
            }
            let Ok(data) = fs::read(path) else { continue };
            if let Ok(png) = deckard_core::png_guard::sanitize(&data, 512 * 1024, 192) {
                return format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(png)
                );
            }
        }
        String::new()
    }

    #[zbus(name = "GetApp")]
    async fn get_app(
        &self,
        pkg: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<VariantMap, ApiError> {
        package(pkg)?;
        Ok(contract::app(
            &self
                .request("app", json!({"pkg":pkg}), None, header)
                .await?,
        ))
    }
    #[zbus(name = "SetPermission")]
    async fn set_permission(
        &self,
        pkg: &str,
        permission: &str,
        granted: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        let android = contract::permission(permission).ok_or_else(invalid)?;
        if permission == "notifications" {
            self.request(
                "notifications",
                json!({"pkg":pkg,"enabled":granted}),
                Some(pkg),
                header,
            )
            .await?;
        } else {
            self.request(
                "perm",
                json!({"pkg":pkg,"permissions":android,"granted":granted}),
                Some(pkg),
                header,
            )
            .await?;
        }
        Ok(())
    }
    #[zbus(name = "GetPermissionApps")]
    async fn get_permission_apps(
        &self,
        permission: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>, ApiError> {
        let android = contract::permission(permission).ok_or_else(invalid)?;
        let (op, args) = if permission == "notifications" {
            ("notif_apps", json!({}))
        } else {
            ("perm_apps", json!({"permissions":android}))
        };
        Ok(contract::apps(
            &self.request(op, args, None, header).await?,
            true,
        ))
    }
    #[zbus(name = "SetNotifications")]
    async fn set_notifications(
        &self,
        pkg: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        self.request(
            "notifications",
            json!({"pkg":pkg,"enabled":enabled}),
            Some(pkg),
            header,
        )
        .await?;
        Ok(())
    }
    #[zbus(name = "SetOpenLinks")]
    async fn set_open_links(
        &self,
        pkg: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        self.request(
            "links",
            json!({"pkg":pkg,"enabled":enabled}),
            Some(pkg),
            header,
        )
        .await?;
        Ok(())
    }
    #[zbus(name = "SetBattery")]
    async fn set_battery(
        &self,
        pkg: &str,
        mode: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        choice(mode, &["unrestricted", "optimized"])?;
        self.request("battery", json!({"pkg":pkg,"mode":mode}), Some(pkg), header)
            .await?;
        Ok(())
    }
    #[zbus(name = "ForceStop")]
    async fn force_stop(
        &self,
        pkg: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        self.request("stop", json!({"pkg":pkg}), Some(pkg), header)
            .await?;
        Ok(())
    }
    #[zbus(name = "ClearCache")]
    async fn clear_cache(
        &self,
        pkg: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        self.request("cache", json!({"pkg":pkg}), Some(pkg), header)
            .await?;
        Ok(())
    }
    #[zbus(name = "ClearData")]
    async fn clear_data(
        &self,
        pkg: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        package(pkg)?;
        self.request("clear", json!({"pkg":pkg}), Some(pkg), header)
            .await?;
        Ok(())
    }
    #[zbus(name = "GetSpecialAccess")]
    async fn get_special_access(
        &self,
        kind: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<Vec<VariantMap>, ApiError> {
        choice(kind, contract::SPECIAL_KINDS)?;
        Ok(contract::apps(
            &self
                .request("special", json!({"kind":kind}), None, header)
                .await?,
            false,
        ))
    }
    #[zbus(name = "SetSpecialAccess")]
    async fn set_special_access(
        &self,
        kind: &str,
        pkg: &str,
        granted: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        choice(kind, contract::SPECIAL_KINDS)?;
        package(pkg)?;
        self.request(
            "special_set",
            json!({"kind":kind,"pkg":pkg,"granted":granted}),
            Some(pkg),
            header,
        )
        .await?;
        Ok(())
    }
    #[zbus(name = "GetGoogleServices")]
    async fn get_google_services(
        &self,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<VariantMap, ApiError> {
        Ok(contract::google(
            &self.request("gms", json!({}), None, header).await?,
        ))
    }
    #[zbus(name = "SetGoogleServices")]
    async fn set_google_services(
        &self,
        key: &str,
        enabled: bool,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        choice(key, &["enabled", "push"])?;
        self.request(
            "gms_set",
            json!({"key":key,"enabled":enabled}),
            None,
            header,
        )
        .await?;
        Ok(())
    }
    #[zbus(name = "OpenGoogleServices")]
    async fn open_google_services(
        &self,
        page: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), ApiError> {
        choice(page, &["account", "selfcheck"])?;
        self.request("gms_open", json!({"page":page}), None, header)
            .await?;
        if let Ok(hint) =
            Message::method_call("/org/marathonos/Shell/Navigation", "ExpectAppWindow")
                .and_then(|builder| builder.destination("org.marathonos.Shell"))
                .and_then(|builder| builder.interface("org.marathonos.Shell.Navigation1"))
                .and_then(|builder| builder.with_flags(Flags::NoReplyExpected))
                .and_then(|builder| builder.build(&("waydroid.com.google.android.gms",)))
        {
            let _ = self.bus.send(&hint).await;
        }
        Ok(())
    }
    #[zbus(name = "ResetAndroid")]
    async fn reset_android(&self, #[zbus(header)] header: Header<'_>) -> Result<(), ApiError> {
        self.allowed(header).await?;
        let helper = if let Some(value) =
            std::env::var_os("DECKARD_RESET_HELPER").filter(|value| !value.is_empty())
        {
            vec![PathBuf::from(value)]
        } else if PathBuf::from("/usr/libexec/deckard-reset").exists() {
            vec![
                PathBuf::from("pkexec"),
                PathBuf::from("/usr/libexec/deckard-reset"),
            ]
        } else {
            Vec::new()
        };
        let request = self
            .runtime
            .request_reset(!helper.is_empty())
            .map_err(|error| {
                failed(match error {
                    ResetError::AlreadyResetting => "Android is already being reset",
                    _ => "Resetting Android isn't available",
                })
            })?;
        if self.runtime.state() != State::Stopped {
            if let Err(error) = runtime::call_unit(&self.bus, UnitCall::Stop).await {
                self.runtime.reset_timeout(request);
                return Err(failed(error.to_string()));
            }
            let deadline = Instant::now() + Duration::from_secs(90);
            while self.runtime.state() != State::Stopped {
                if Instant::now() >= deadline {
                    self.runtime.reset_timeout(request);
                    return Err(failed("Android didn't stop"));
                }
                async_io::Timer::after(Duration::from_millis(100)).await;
            }
        }
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let mut command = Command::new(&helper[0]);
            command.args(&helper[1..]).stdin(Stdio::null());
            let result = command.spawn().and_then(|mut child| {
                let deadline = Instant::now() + Duration::from_secs(300);
                loop {
                    if let Some(status) = child.try_wait()? {
                        break Ok(status.success());
                    }
                    if Instant::now() >= deadline {
                        child.kill()?;
                        let _ = child.wait();
                        break Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let _ = tx.send_blocking(result.unwrap_or(false));
        });
        let success = rx.recv().await.unwrap_or(false);
        self.runtime.reset_finished(request, success);
        if success {
            Ok(())
        } else {
            Err(failed("Resetting Android failed"))
        }
    }

    #[zbus(signal, name = "RuntimeChanged")]
    async fn runtime_changed(
        signal_context: &zbus::object_server::SignalEmitter<'_>,
        runtime: VariantMap,
    ) -> zbus::Result<()>;
    #[zbus(signal, name = "AppChanged")]
    async fn app_changed(
        signal_context: &zbus::object_server::SignalEmitter<'_>,
        pkg: &str,
    ) -> zbus::Result<()>;
}
