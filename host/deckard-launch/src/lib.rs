use std::error::Error;

#[cfg(all(target_env = "musl", target_feature = "crt-static"))]
compile_error!("deckard-launch needs dynamic musl for libgbinder dlopen; build with RUSTFLAGS='-C target-feature=-crt-static'");

pub mod binder;
pub mod session;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub trait Backend {
    fn ready(&mut self, start_session: bool) -> Result<bool>;
    fn unfreeze(&mut self) -> Result<()>;
    fn unfreeze_for_launch(&mut self) -> Result<()> {
        if let Err(err) = self.unfreeze() {
            eprintln!("deckard-launch: container unfreeze failed; trying launch: {err}");
        }
        Ok(())
    }
    fn freeze(&mut self) -> Result<()>;
    fn setprop(&mut self, key: &str, value: &str) -> Result<()>;
    fn getprop(&mut self, key: &str, fallback: &str) -> Result<String>;
    fn launch_app(&mut self, package: &str) -> Result<()>;
    fn launch_intent(&mut self, action: &str, uri: &str) -> Result<String>;
    fn settings_put_string(&mut self, table: i32, key: &str, value: &str) -> Result<()>;
}

fn immersive<B: Backend>(backend: &mut B) -> Result<()> {
    let value = if backend.getprop("persist.waydroid.multi_windows", "false")? == "false" {
        "immersive.status=*"
    } else {
        "immersive.full=*"
    };
    backend.settings_put_string(2, "policy_control", value)
}

pub fn launch_app<B: Backend>(backend: &mut B, package: &str) -> Result<()> {
    if package.is_empty() || package.contains(char::is_whitespace) {
        return Err("invalid package name".into());
    }
    backend.ready(true)?;
    backend.unfreeze_for_launch()?;
    backend.setprop("waydroid.active_apps", package)?;
    backend.launch_app(package)?;
    immersive(backend)
}

pub fn launch_intent<B: Backend>(backend: &mut B, action: &str, uri: &str) -> Result<()> {
    if action.is_empty() || uri.is_empty() {
        return Err("action and URI must not be empty".into());
    }
    backend.ready(true)?;
    backend.unfreeze_for_launch()?;
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
    immersive(backend)
}

pub fn get_property<B: Backend>(backend: &mut B, key: &str) -> Result<String> {
    if key.is_empty() {
        return Err("property key must not be empty".into());
    }
    let frozen = backend.ready(false)?;
    if frozen {
        backend.unfreeze()?;
    }
    let result = backend.getprop(key, "");
    if frozen {
        let freeze = backend.freeze();
        if result.is_ok() {
            freeze?;
        }
    }
    result
}

pub fn set_property<B: Backend>(backend: &mut B, key: &str, value: &str) -> Result<()> {
    if key.is_empty() {
        return Err("property key must not be empty".into());
    }
    let frozen = backend.ready(false)?;
    if frozen {
        backend.unfreeze()?;
    }
    let result = backend.setprop(key, value);
    if frozen {
        let freeze = backend.freeze();
        if result.is_ok() {
            freeze?;
        }
    }
    result
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
        fn ready(&mut self, start_session: bool) -> Result<bool> {
            self.calls.push(format!("ready:{start_session}"));
            Ok(self.frozen)
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
    }

    #[test]
    fn app_preserves_active_app_and_immersive_order() {
        let mut fake = Fake {
            multiwin: "false".into(),
            ..Fake::default()
        };
        launch_app(&mut fake, "org.example.app").unwrap();
        assert_eq!(
            fake.calls,
            [
                "ready:true",
                "unfreeze",
                "set:waydroid.active_apps:org.example.app",
                "app:org.example.app",
                "get:persist.waydroid.multi_windows:false",
                "settings:2:policy_control:immersive.status=*"
            ]
        );
    }

    #[test]
    fn intent_updates_resolved_package_and_multiwindow_policy() {
        let mut fake = Fake {
            multiwin: "true".into(),
            intent_result: "org.example.app".into(),
            ..Fake::default()
        };
        launch_intent(
            &mut fake,
            "android.intent.action.VIEW",
            "https://example.org",
        )
        .unwrap();
        assert_eq!(
            fake.calls[2],
            "intent:android.intent.action.VIEW:https://example.org"
        );
        assert_eq!(fake.calls[3], "set:waydroid.active_apps:org.example.app");
        assert_eq!(fake.calls[5], "settings:2:policy_control:immersive.full=*");
    }

    #[test]
    fn unresolved_intent_does_not_change_active_app() {
        let mut fake = Fake::default();
        launch_intent(&mut fake, "VIEW", "content://example").unwrap();
        assert_eq!(fake.calls.len(), 3);
    }

    #[test]
    fn android_intent_maps_to_waydroid() {
        let mut fake = Fake {
            intent_result: "android".into(),
            ..Fake::default()
        };
        launch_intent(&mut fake, "VIEW", "package:org.example").unwrap();
        assert_eq!(fake.calls[3], "set:waydroid.active_apps:Waydroid");
    }

    #[test]
    fn property_restores_frozen_container() {
        let mut fake = Fake {
            frozen: true,
            ..Fake::default()
        };
        assert_eq!(
            get_property(&mut fake, "sys.boot_completed").unwrap(),
            "value"
        );
        assert_eq!(
            fake.calls,
            [
                "ready:false",
                "unfreeze",
                "get:sys.boot_completed:",
                "freeze"
            ]
        );
    }

    #[test]
    fn launch_tolerates_unfreeze_failure_but_property_does_not() {
        let mut launch = Fake {
            fail_unfreeze: true,
            ..Fake::default()
        };
        launch_app(&mut launch, "org.example.app").unwrap();
        assert!(launch
            .calls
            .iter()
            .any(|call| call == "app:org.example.app"));

        let mut property = Fake {
            frozen: true,
            fail_unfreeze: true,
            ..Fake::default()
        };
        assert!(get_property(&mut property, "sys.boot_completed").is_err());
        assert_eq!(property.calls, ["ready:false", "unfreeze"]);
    }

    #[test]
    fn invalid_package_is_rejected_before_io() {
        let mut fake = Fake::default();
        assert!(launch_app(&mut fake, "app; bad").is_err());
        assert!(fake.calls.is_empty());
    }
}
