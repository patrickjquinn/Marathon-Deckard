#![allow(clippy::expect_used, reason = "fixed temporary-directory fixtures")]
//! `deckard-reset` against a fake home, passwd, systemctl and lxc-info.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    uid: u32,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture directory");
        let uid = rustix::process::getuid().as_raw();
        // The helper refuses root's data: as root (an aport build), act for another uid.
        let uid = if uid == 0 { 4242 } else { uid };
        let fixture = Self { root, uid };
        fs::create_dir_all(fixture.path("bin")).expect("bin");
        fs::create_dir_all(fixture.path("etc")).expect("etc");
        fs::create_dir_all(fixture.home().join(".local/share/applications")).expect("applications");
        let home = fixture.home();
        fs::write(
            fixture.path("passwd"),
            format!("user:x:{uid}:{uid}::{}:/bin/sh\n", home.display()),
        )
        .expect("passwd");
        fixture.script(
            "systemctl",
            r#"echo "$*" >> "$W/systemctl.log"
case "$1" in
	is-active) [ -f "$W/active-$3" ] ;;
	stop) [ "${FAIL_ON:-}" != stop ] && rm -f "$W/active-$2" ;;
	*) [ "${FAIL_ON:-}" != "$1" ] ;;
esac"#,
        );
        fixture.script("lxc-info", r#"echo "State: ${LXC_STATE:-STOPPED}""#);
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn data(&self) -> PathBuf {
        self.home().join(".local/share/waydroid/data")
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path(&format!("bin/{name}"));
        let script = format!("#!/bin/sh\nW='{}'\n{body}\n", self.root.path().display());
        fs::write(&path, script).expect("script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("executable");
    }

    fn activate(&self, unit: &str) {
        fs::write(self.path(&format!("active-{unit}")), "").expect("active marker");
    }

    fn log(&self) -> Vec<String> {
        fs::read_to_string(self.path("systemctl.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn seed(&self) {
        let data = self.data();
        let _ = fs::remove_dir_all(&data);
        for directory in [
            "app/org.fdroid.fdroid",
            "system",
            "deckard/xfer/0123456789abcdef",
            "deckard/install",
            ".hidden",
        ] {
            fs::create_dir_all(data.join(directory)).expect("data directory");
        }
        fs::write(data.join("system/packages.list"), "secret").expect("packages");
        fs::write(data.join("deckard/palette"), "palette").expect("palette");
        fs::write(data.join("deckard/xfer/0123456789abcdef/photo.jpg"), "x").expect("transfer");
        fs::write(data.join("deckard/install/app.apk"), "x").expect("install");
        fs::write(self.path("etc/keep"), "keep").expect("outside file");
        std::os::unix::fs::symlink(self.path("etc"), data.join("etc-link")).expect("link");
        let applications = self.home().join(".local/share/applications");
        for entry in [
            "waydroid.org.fdroid.fdroid.desktop",
            "deckard.org.fdroid.fdroid.desktop",
            "org.marathonos.notes.desktop",
        ] {
            fs::write(applications.join(entry), "").expect("desktop entry");
        }
        let _ = fs::remove_file(self.path("systemctl.log"));
    }

    fn run(&self, environment: &[(&str, &str)], arguments: &[&str]) -> Output {
        let bin = self.path("bin");
        Command::new(env!("CARGO_BIN_EXE_deckard-reset"))
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("DECKARD_RESET_PASSWD", self.path("passwd"))
            .env("DECKARD_RESET_SYSTEMCTL", bin.join("systemctl"))
            .env("DECKARD_RESET_LXC_INFO", bin.join("lxc-info"))
            .envs(environment.iter().copied())
            .output()
            .expect("helper runs")
    }

    fn reset(&self, environment: &[(&str, &str)]) -> Output {
        // As root, the home must belong to the uid the helper acts for.
        if rustix::process::getuid().is_root() {
            own(&self.home(), self.uid);
        }
        let uid = self.uid.to_string();
        let mut all = vec![("PKEXEC_UID", uid.as_str())];
        all.extend_from_slice(environment);
        self.run(&all, &[])
    }
}

/// `chown -R` without following links.
fn own(path: &Path, uid: u32) {
    std::os::unix::fs::lchown(path, Some(uid), Some(uid)).expect("own fixture");
    let is_directory = path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_dir());
    if is_directory {
        let entries = fs::read_dir(path).expect("fixture entries");
        for entry in entries {
            own(&entry.expect("fixture entry").path(), uid);
        }
    }
}

fn status(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

#[test]
fn refuses_without_pkexec_for_root_or_with_arguments() {
    let fixture = Fixture::new();
    fixture.seed();
    assert_eq!(status(&fixture.run(&[("DECKARD_RESET_UID", "")], &[])), 2);
    assert_eq!(status(&fixture.run(&[("DECKARD_RESET_UID", "0")], &[])), 2);
    let uid = fixture.uid.to_string();
    assert_eq!(status(&fixture.run(&[("PKEXEC_UID", &uid)], &["extra"])), 2);
    assert!(
        fixture.data().join("system/packages.list").exists(),
        "a refusal removed data"
    );
}

#[test]
fn removes_android_data_and_restarts_the_legacy_container() {
    let fixture = Fixture::new();
    fixture.seed();
    fixture.activate("waydroid-container.service");
    let output = fixture.reset(&[]);
    assert_eq!(
        status(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fixture.data();
    for gone in [
        "app",
        "system",
        ".hidden",
        "etc-link",
        "deckard/xfer",
        "deckard/install",
    ] {
        assert!(!exists(&data.join(gone)), "{gone} is still there");
    }
    assert!(fixture.path("etc/keep").exists(), "a symlink was followed");
    assert!(
        data.join("deckard/palette").exists(),
        "deckard/ was removed"
    );
    let applications = fixture.home().join(".local/share/applications");
    assert!(!exists(
        &applications.join("waydroid.org.fdroid.fdroid.desktop")
    ));
    assert!(!exists(
        &applications.join("deckard.org.fdroid.fdroid.desktop")
    ));
    assert!(
        applications.join("org.marathonos.notes.desktop").exists(),
        "a Marathon app entry went"
    );
    let log = fixture.log();
    assert!(
        log.contains(&"stop waydroid-container.service".to_owned()),
        "not stopped: {log:?}"
    );
    assert_eq!(
        log.last().map(String::as_str),
        Some("start waydroid-container.service")
    );
}

#[test]
fn stops_and_restarts_the_deckard_container() {
    let fixture = Fixture::new();
    fixture.seed();
    fixture.activate("deckard-container.service");
    let output = fixture.reset(&[]);
    assert_eq!(
        status(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fixture.log();
    assert!(
        log.contains(&"stop deckard-container.service".to_owned()),
        "not stopped: {log:?}"
    );
    assert_eq!(
        log.last().map(String::as_str),
        Some("start deckard-container.service")
    );
    assert!(
        !log.iter()
            .any(|line| line == "stop waydroid-container.service"),
        "{log:?}"
    );
}

#[test]
fn starts_nothing_that_was_not_running() {
    let fixture = Fixture::new();
    fixture.seed();
    assert_eq!(status(&fixture.reset(&[])), 0);
    assert!(
        !fixture.log().iter().any(|line| line.starts_with("start")),
        "{:?}",
        fixture.log()
    );
}

#[test]
fn removes_nothing_while_the_container_runs() {
    let fixture = Fixture::new();
    fixture.seed();
    fixture.activate("deckard-container.service");
    assert_eq!(status(&fixture.reset(&[("FAIL_ON", "stop")])), 1);
    assert!(
        fixture.data().join("system/packages.list").exists(),
        "removed data with the unit up"
    );

    fixture.seed();
    assert_eq!(status(&fixture.reset(&[("LXC_STATE", "RUNNING")])), 1);
    assert!(
        fixture.data().join("system/packages.list").exists(),
        "removed data with LXC running"
    );
}

#[test]
fn refuses_data_outside_the_home_and_reports_none() {
    let fixture = Fixture::new();
    fixture.seed();
    let data = fixture.data();
    fs::remove_dir_all(&data).expect("remove data");
    fs::create_dir_all(fixture.path("elsewhere/system")).expect("elsewhere");
    std::os::unix::fs::symlink(fixture.path("elsewhere"), &data).expect("link");
    assert_eq!(status(&fixture.reset(&[])), 1);
    assert!(
        fixture.path("elsewhere/system").exists(),
        "followed data out of the home"
    );

    fs::remove_file(&data).expect("remove link");
    assert_eq!(status(&fixture.reset(&[])), 3);
}

#[test]
fn prefers_the_deckard_data_directory() {
    let fixture = Fixture::new();
    fixture.seed();
    let deckard = fixture.home().join(".local/share/deckard/data");
    fs::create_dir_all(deckard.join("system")).expect("deckard data");
    assert_eq!(status(&fixture.reset(&[])), 0);
    assert!(!exists(&deckard.join("system")), "the Deckard data stayed");
    assert!(
        fixture.data().join("system").exists(),
        "the legacy data went too"
    );
}
