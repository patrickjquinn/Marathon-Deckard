//! Starts Android apps and intents and reads or writes Android properties,
//! through the Deckard session and Android's `IPlatform`.

pub mod binder;
pub mod session;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Whether [`Backend::ready`] may start a stopped session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// Start it and wait for Android's user to unlock.
    Start,
    /// Fail when it is stopped.
    Existing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Running,
    Frozen,
}

/// What launching needs from the session, the container and Android.
///
/// # Errors
/// Each method fails when the session, the container manager, Android or the
/// Shell is unavailable or refuses.
pub trait Backend {
    /// Makes sure the session runs and reports whether the container is frozen.
    fn ready(&mut self, session: Session) -> Result<Container>;
    fn unfreeze(&mut self) -> Result<()>;
    fn freeze(&mut self) -> Result<()>;
    fn setprop(&mut self, key: &str, value: &str) -> Result<()>;
    fn getprop(&mut self, key: &str, fallback: &str) -> Result<String>;
    fn launch_app(&mut self, package: &str) -> Result<()>;
    /// The package that handles the intent, or empty.
    fn launch_intent(&mut self, action: &str, uri: &str) -> Result<String>;
    fn settings_put_string(&mut self, table: i32, key: &str, value: &str) -> Result<()>;
    fn show_android_app(&mut self, package: &str) -> Result<()>;
}

fn wake(backend: &mut impl Backend) -> Result<()> {
    if backend.ready(Session::Start)? == Container::Frozen {
        backend.unfreeze()?;
    }
    Ok(())
}

fn immersive(backend: &mut impl Backend) -> Result<()> {
    let value = if backend.getprop("persist.waydroid.multi_windows", "false")? == "false" {
        "immersive.status=*"
    } else {
        "immersive.full=*"
    };
    backend.settings_put_string(2, "policy_control", value)
}

/// # Errors
/// The package name is invalid or a step fails.
pub fn launch_app(backend: &mut impl Backend, package: &str) -> Result<()> {
    if package.is_empty() || package.contains(char::is_whitespace) {
        return Err("invalid package name".into());
    }
    wake(backend)?;
    backend.setprop("waydroid.active_apps", package)?;
    backend.show_android_app(package)?;
    backend.launch_app(package)?;
    immersive(backend)
}

/// # Errors
/// The action or URI is empty or a step fails.
pub fn launch_intent(backend: &mut impl Backend, action: &str, uri: &str) -> Result<()> {
    if action.is_empty() || uri.is_empty() {
        return Err("action and URI must not be empty".into());
    }
    wake(backend)?;
    let package = backend.launch_intent(action, uri)?;
    if package.is_empty() {
        return Ok(());
    }
    let active = if package == "android" {
        "Waydroid"
    } else {
        &package
    };
    backend.setprop("waydroid.active_apps", active)?;
    // DeckardSystem relays system intents but is not itself a visible app.
    if package != "android" && package != "org.marathonos.deckard.system" {
        backend.show_android_app(&package)?;
    }
    immersive(backend)
}

/// Runs `action` on a running session, thawing a frozen container around it.
fn thawed<B: Backend, T>(
    backend: &mut B,
    key: &str,
    action: impl FnOnce(&mut B) -> Result<T>,
) -> Result<T> {
    if key.is_empty() {
        return Err("property key must not be empty".into());
    }
    let frozen = backend.ready(Session::Existing)? == Container::Frozen;
    if frozen {
        backend.unfreeze()?;
    }
    let result = action(backend);
    if frozen {
        let freeze = backend.freeze();
        if result.is_ok() {
            freeze?;
        }
    }
    result
}

/// # Errors
/// The key is empty, the session is stopped or Android refuses.
pub fn get_property(backend: &mut impl Backend, key: &str) -> Result<String> {
    thawed(backend, key, |backend| backend.getprop(key, ""))
}

/// # Errors
/// The key is empty, the session is stopped or Android refuses.
pub fn set_property(backend: &mut impl Backend, key: &str, value: &str) -> Result<()> {
    thawed(backend, key, |backend| backend.setprop(key, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        frozen: bool,
        fail_unfreeze: bool,
        multiwin: String,
        intent_result: String,
    }

    impl Backend for Fake {
        fn ready(&mut self, session: Session) -> Result<Container> {
            self.calls.push(format!("ready:{session:?}"));
            Ok(if self.frozen {
                Container::Frozen
            } else {
                Container::Running
            })
        }
        fn unfreeze(&mut self) -> Result<()> {
            self.calls.push("unfreeze".into());
            if self.fail_unfreeze {
                Err("container unavailable".into())
            } else {
                Ok(())
            }
        }
        fn freeze(&mut self) -> Result<()> {
            self.calls.push("freeze".into());
            Ok(())
        }
        fn setprop(&mut self, key: &str, value: &str) -> Result<()> {
            self.calls.push(format!("set:{key}:{value}"));
            Ok(())
        }
        fn getprop(&mut self, key: &str, fallback: &str) -> Result<String> {
            self.calls.push(format!("get:{key}:{fallback}"));
            if key == "persist.waydroid.multi_windows" {
                Ok(self.multiwin.clone())
            } else {
                Ok("value".into())
            }
        }
        fn launch_app(&mut self, package: &str) -> Result<()> {
            self.calls.push(format!("app:{package}"));
            Ok(())
        }
        fn launch_intent(&mut self, action: &str, uri: &str) -> Result<String> {
            self.calls.push(format!("intent:{action}:{uri}"));
            Ok(self.intent_result.clone())
        }
        fn settings_put_string(&mut self, table: i32, key: &str, value: &str) -> Result<()> {
            self.calls.push(format!("settings:{table}:{key}:{value}"));
            Ok(())
        }
        fn show_android_app(&mut self, package: &str) -> Result<()> {
            self.calls.push(format!("show:{package}"));
            Ok(())
        }
    }

    #[test]
    fn app_preserves_active_app_and_immersive_order() -> Result<()> {
        let mut fake = Fake {
            multiwin: "false".into(),
            ..Fake::default()
        };
        launch_app(&mut fake, "org.example.app")?;
        assert_eq!(
            fake.calls,
            [
                "ready:Start",
                "set:waydroid.active_apps:org.example.app",
                "show:org.example.app",
                "app:org.example.app",
                "get:persist.waydroid.multi_windows:false",
                "settings:2:policy_control:immersive.status=*"
            ]
        );
        Ok(())
    }

    #[test]
    fn intent_updates_resolved_package_and_multiwindow_policy() -> Result<()> {
        let mut fake = Fake {
            multiwin: "true".into(),
            intent_result: "org.example.app".into(),
            ..Fake::default()
        };
        launch_intent(
            &mut fake,
            "android.intent.action.VIEW",
            "https://example.org",
        )?;
        assert_eq!(
            fake.calls[1..],
            [
                "intent:android.intent.action.VIEW:https://example.org",
                "set:waydroid.active_apps:org.example.app",
                "show:org.example.app",
                "get:persist.waydroid.multi_windows:false",
                "settings:2:policy_control:immersive.full=*"
            ]
        );
        Ok(())
    }

    #[test]
    fn system_intents_map_or_skip_the_window() -> Result<()> {
        let mut unresolved = Fake::default();
        launch_intent(&mut unresolved, "VIEW", "content://example")?;
        assert_eq!(unresolved.calls.len(), 2);
        for (package, active) in [
            ("android", "Waydroid"),
            (
                "org.marathonos.deckard.system",
                "org.marathonos.deckard.system",
            ),
        ] {
            let mut fake = Fake {
                intent_result: package.into(),
                ..Fake::default()
            };
            launch_intent(&mut fake, "VIEW", "package:org.example")?;
            assert_eq!(fake.calls[2], format!("set:waydroid.active_apps:{active}"));
            assert!(!fake.calls.iter().any(|call| call.starts_with("show:")));
        }
        Ok(())
    }

    #[test]
    fn property_restores_frozen_container() -> Result<()> {
        let mut fake = Fake {
            frozen: true,
            ..Fake::default()
        };
        assert_eq!(get_property(&mut fake, "sys.boot_completed")?, "value");
        assert_eq!(
            fake.calls,
            [
                "ready:Existing",
                "unfreeze",
                "get:sys.boot_completed:",
                "freeze"
            ]
        );
        Ok(())
    }

    #[test]
    fn stops_on_unfreeze_failure_and_rejects_bad_input_before_io() {
        let mut launch = Fake {
            frozen: true,
            fail_unfreeze: true,
            ..Fake::default()
        };
        assert!(launch_app(&mut launch, "org.example.app").is_err());
        assert_eq!(launch.calls, ["ready:Start", "unfreeze"]);
        let mut property = Fake {
            frozen: true,
            fail_unfreeze: true,
            ..Fake::default()
        };
        assert!(get_property(&mut property, "sys.boot_completed").is_err());
        assert_eq!(property.calls, ["ready:Existing", "unfreeze"]);
        let mut fake = Fake::default();
        assert!(launch_app(&mut fake, "app; bad").is_err());
        assert!(fake.calls.is_empty());
    }
}
