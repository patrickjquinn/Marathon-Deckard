#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use deckard_overlay_sync::{Paths, Whiteouts, sync_with};
use tempfile::TempDir;

struct FakeWhiteouts(Mutex<HashSet<PathBuf>>);

impl FakeWhiteouts {
    fn new() -> Self {
        Self(Mutex::new(HashSet::new()))
    }
}

impl Whiteouts for FakeWhiteouts {
    fn is_whiteout(&self, path: &Path) -> io::Result<bool> {
        Ok(self.0.lock().unwrap().contains(path))
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        fs::write(path, b"whiteout")?;
        self.0.lock().unwrap().insert(path.to_owned());
        Ok(())
    }
}

fn setup() -> (TempDir, Paths) {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths {
        share: root.path().join("share"),
        waydroid: root.path().join("waydroid"),
    };
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::create_dir_all(paths.share.join("overlay/system/etc/init")).unwrap();
    fs::create_dir_all(paths.share.join("overlay/vendor/lib64/hw")).unwrap();
    fs::create_dir_all(&paths.waydroid).unwrap();
    for path in ["waydroid.prop", "whiteouts"] {
        fs::copy(fixture.join(path), paths.share.join(path)).unwrap();
    }
    for path in ["waydroid.cfg", "waydroid_base.prop"] {
        fs::copy(fixture.join(path), paths.waydroid.join(path)).unwrap();
    }
    for path in [
        "system/etc/init/deckard-watch.rc",
        "vendor/lib64/hw/hwcomposer.waydroid.so",
    ] {
        fs::copy(
            fixture.join("overlay").join(path),
            paths.share.join("overlay").join(path),
        )
        .unwrap();
    }
    (root, paths)
}

#[test]
fn reconciles_assets_guards_whiteouts_and_properties() {
    let (_root, paths) = setup();
    let whiteouts = FakeWhiteouts::new();
    let dst = paths.waydroid.join("overlay");
    fs::create_dir_all(dst.join("system")).unwrap();
    fs::write(dst.join("system/conflict.apk"), "keep").unwrap();
    fs::write(dst.join("system/obsolete.apk"), "stale").unwrap();
    fs::write(
        paths.waydroid.join("deckard-overlay.list"),
        "./system/obsolete.apk\n",
    )
    .unwrap();
    fs::write(paths.waydroid.join("deckard-props.list"), "old.property\n").unwrap();
    fs::write(
        paths.waydroid.join("waydroid_base.prop"),
        "old.property=x\nro.product.cpu.abilist=obsolete\nkeep.me=yes\n",
    )
    .unwrap();
    fs::write(
        paths.waydroid.join("waydroid.cfg"),
        format!(
            "{}old.property = x\n",
            include_str!("fixtures/waydroid.cfg")
        ),
    )
    .unwrap();
    let source = paths.share.join("overlay/system/etc/init/deckard-watch.rc");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
    symlink(
        "deckard-watch.rc",
        paths.share.join("overlay/system/etc/init/watch-link.rc"),
    )
    .unwrap();

    let mut log = Vec::new();
    sync_with(&paths, &whiteouts, &mut log).unwrap();
    assert!(!dst.join("system/obsolete.apk").exists());
    assert_eq!(
        fs::read_to_string(dst.join("vendor/lib64/hw/hwcomposer.waydroid.so")).unwrap(),
        "patched composer\n"
    );
    assert_eq!(
        fs::symlink_metadata(dst.join("system/etc/init/deckard-watch.rc"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert_eq!(
        fs::read_link(dst.join("system/etc/init/watch-link.rc")).unwrap(),
        Path::new("deckard-watch.rc")
    );
    assert!(dst.join("system/removed.apk").exists());
    assert_eq!(
        fs::read_to_string(dst.join("system/conflict.apk")).unwrap(),
        "keep"
    );
    assert!(!dst.join("../escape").exists());
    assert!(
        String::from_utf8(log)
            .unwrap()
            .contains("exists in the overlay; not hiding it")
    );
    let cfg = fs::read_to_string(paths.waydroid.join("waydroid.cfg")).unwrap();
    assert!(cfg.contains("ro.product.cpu.abilist = arm64-v8a"));
    assert!(!cfg.contains("obsolete"));
    assert!(!cfg.contains("old.property"));
    assert!(cfg.contains("keep.me = yes"));
    let base = fs::read_to_string(paths.waydroid.join("waydroid_base.prop")).unwrap();
    assert!(base.contains("ro.product.cpu.abilist=arm64-v8a"));
    assert!(!base.contains("old.property"));
    assert_eq!(
        fs::read_to_string(paths.waydroid.join("deckard-props.list")).unwrap(),
        "ro.product.cpu.abilist\nro.lmk.use_new_strategy\n"
    );

    let mut log = Vec::new();
    sync_with(&paths, &whiteouts, &mut log).unwrap();
    assert_eq!(
        fs::read_to_string(dst.join("system/removed.apk")).unwrap(),
        "whiteout"
    );
    assert_eq!(
        fs::read_to_string(paths.waydroid.join("deckard-whiteouts.list")).unwrap(),
        "system/removed.apk\n"
    );

    fs::write(
        paths.waydroid.join("waydroid.cfg"),
        "[waydroid]\nvendor_datetime = 1800000000\n[properties]\n",
    )
    .unwrap();
    fs::write(paths.share.join("whiteouts"), "").unwrap();
    fs::remove_file(paths.share.join("overlay/system/etc/init/deckard-watch.rc")).unwrap();
    let mut log = Vec::new();
    sync_with(&paths, &whiteouts, &mut log).unwrap();
    assert!(!dst.join("vendor/lib64/hw/hwcomposer.waydroid.so").exists());
    assert!(!dst.join("system/etc/init/deckard-watch.rc").exists());
    assert!(!dst.join("system/removed.apk").exists());
    assert!(dst.join("system/conflict.apk").exists());
    assert!(String::from_utf8(log).unwrap().contains("not '1800000000'"));
}

#[test]
fn removed_property_is_retracted_and_hardlinks_are_preserved() {
    let (_root, paths) = setup();
    let source = paths.share.join("overlay/system/etc/init/deckard-watch.rc");
    let link = paths.share.join("overlay/system/etc/init/second.rc");
    fs::hard_link(&source, &link).unwrap();
    let mut log = Vec::new();
    sync_with(&paths, &FakeWhiteouts::new(), &mut log).unwrap();
    let dst = paths.waydroid.join("overlay/system/etc/init");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(dst.join("deckard-watch.rc")).unwrap().ino(),
        fs::metadata(dst.join("second.rc")).unwrap().ino()
    );
    fs::write(paths.share.join("waydroid.prop"), "").unwrap();
    fs::write(paths.share.join("whiteouts"), "").unwrap();
    sync_with(&paths, &FakeWhiteouts::new(), &mut log).unwrap();
    let cfg = fs::read_to_string(paths.waydroid.join("waydroid.cfg")).unwrap();
    assert!(!cfg.contains("ro.product.cpu.abilist"));
    assert!(!cfg.contains("ro.lmk.use_new_strategy"));
    assert!(cfg.contains("keep.me = yes"));
    assert_eq!(
        fs::read_to_string(paths.waydroid.join("deckard-props.list")).unwrap(),
        ""
    );
}

#[test]
fn absent_source_is_noop_and_unsafe_old_list_is_rejected() {
    let (_root, paths) = setup();
    fs::rename(
        paths.share.join("overlay"),
        paths.share.join("removed-overlay"),
    )
    .unwrap();
    let mut log = Vec::new();
    sync_with(&paths, &FakeWhiteouts::new(), &mut log).unwrap();
    assert!(!paths.waydroid.join("overlay").exists());
    fs::rename(
        paths.share.join("removed-overlay"),
        paths.share.join("overlay"),
    )
    .unwrap();
    fs::write(
        paths.waydroid.join("deckard-overlay.list"),
        "../../escape\n",
    )
    .unwrap();
    assert_eq!(
        sync_with(&paths, &FakeWhiteouts::new(), &mut log)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn refuses_symlinked_overlay_parent() {
    let (_root, paths) = setup();
    let outside = paths.waydroid.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(paths.waydroid.join("overlay")).unwrap();
    fs::create_dir_all(paths.waydroid.join("overlay/system")).unwrap();
    symlink(&outside, paths.waydroid.join("overlay/system/other")).unwrap();
    fs::write(paths.share.join("whiteouts"), "system/other/file\n").unwrap();
    let mut log = Vec::new();
    assert_eq!(
        sync_with(&paths, &FakeWhiteouts::new(), &mut log)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn binary_uses_configurable_paths() {
    let (_root, paths) = setup();
    fs::write(paths.share.join("whiteouts"), "").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_deckard-overlay-sync"))
        .env("DECKARD_SHARE_DIR", &paths.share)
        .env("DECKARD_WAYDROID_DIR", &paths.waydroid)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        paths
            .waydroid
            .join("overlay/system/etc/init/deckard-watch.rc")
            .exists()
    );
}
