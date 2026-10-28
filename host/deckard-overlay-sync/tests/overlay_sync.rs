#![allow(clippy::unwrap_used)]

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::path::Path;

use deckard_overlay_sync::{Paths, sync};
use tempfile::TempDir;

fn setup() -> (TempDir, Paths) {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths {
        share: root.path().join("share"),
        work: root.path().join("deckard"),
    };
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::create_dir_all(paths.share.join("overlay/system/etc/init")).unwrap();
    fs::create_dir_all(&paths.work).unwrap();
    for path in ["deckard.prop", "whiteouts"] {
        fs::copy(fixture.join(path), paths.share.join(path)).unwrap();
    }
    for path in ["deckard.cfg", "deckard_base.prop"] {
        fs::copy(fixture.join(path), paths.work.join(path)).unwrap();
    }
    let watch = "system/etc/init/deckard-watch.rc";
    fs::copy(
        fixture.join("overlay").join(watch),
        paths.share.join("overlay").join(watch),
    )
    .unwrap();
    (root, paths)
}

#[test]
fn reconciles_assets_whiteouts_and_properties() {
    let (_root, paths) = setup();
    let dst = paths.work.join("overlay");
    fs::create_dir_all(dst.join("system")).unwrap();
    fs::write(dst.join("system/conflict.apk"), "keep").unwrap();
    fs::write(dst.join("system/obsolete.apk"), "stale").unwrap();
    fs::write(
        paths.work.join("deckard-overlay.list"),
        "./system/obsolete.apk\n",
    )
    .unwrap();
    fs::write(paths.work.join("deckard-props.list"), "old.property\n").unwrap();
    fs::write(
        paths.work.join("deckard_base.prop"),
        "old.property=x\nro.product.cpu.abilist=obsolete\nkeep.me=yes\n",
    )
    .unwrap();
    fs::write(
        paths.work.join("deckard.cfg"),
        format!("{}old.property = x\n", include_str!("fixtures/deckard.cfg")),
    )
    .unwrap();
    let source = paths.share.join("overlay/system/etc/init/deckard-watch.rc");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
    symlink(
        "deckard-watch.rc",
        paths.share.join("overlay/system/etc/init/watch-link.rc"),
    )
    .unwrap();

    sync(&paths).unwrap();
    assert!(!dst.join("system/obsolete.apk").exists());
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
    let cfg = fs::read_to_string(paths.work.join("deckard.cfg")).unwrap();
    assert!(cfg.contains("ro.product.cpu.abilist = arm64-v8a"));
    assert!(!cfg.contains("obsolete"));
    assert!(!cfg.contains("old.property"));
    assert!(cfg.contains("keep.me = yes"));
    let base = fs::read_to_string(paths.work.join("deckard_base.prop")).unwrap();
    assert!(base.contains("ro.product.cpu.abilist=arm64-v8a"));
    assert!(!base.contains("old.property"));
    assert_eq!(
        fs::read_to_string(paths.work.join("deckard-props.list")).unwrap(),
        "ro.product.cpu.abilist\nro.lmk.use_new_strategy\n"
    );

    sync(&paths).unwrap();
    let whiteout = fs::symlink_metadata(dst.join("system/removed.apk")).unwrap();
    assert!(whiteout.file_type().is_char_device());
    assert_eq!(
        fs::read_to_string(paths.work.join("deckard-whiteouts.list")).unwrap(),
        "system/removed.apk\n"
    );

    fs::write(paths.share.join("whiteouts"), "").unwrap();
    fs::remove_file(paths.share.join("overlay/system/etc/init/deckard-watch.rc")).unwrap();
    sync(&paths).unwrap();
    assert!(!dst.join("system/etc/init/deckard-watch.rc").exists());
    assert!(!dst.join("system/removed.apk").exists());
    assert!(dst.join("system/conflict.apk").exists());
}

#[test]
fn removed_property_is_retracted_and_hardlinks_are_preserved() {
    let (_root, paths) = setup();
    let source = paths.share.join("overlay/system/etc/init/deckard-watch.rc");
    let link = paths.share.join("overlay/system/etc/init/second.rc");
    fs::hard_link(&source, &link).unwrap();
    sync(&paths).unwrap();
    let dst = paths.work.join("overlay/system/etc/init");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(dst.join("deckard-watch.rc")).unwrap().ino(),
        fs::metadata(dst.join("second.rc")).unwrap().ino()
    );
    fs::write(paths.share.join("deckard.prop"), "").unwrap();
    fs::write(paths.share.join("whiteouts"), "").unwrap();
    sync(&paths).unwrap();
    let cfg = fs::read_to_string(paths.work.join("deckard.cfg")).unwrap();
    assert!(!cfg.contains("ro.product.cpu.abilist"));
    assert!(!cfg.contains("ro.lmk.use_new_strategy"));
    assert!(cfg.contains("keep.me = yes"));
    assert_eq!(
        fs::read_to_string(paths.work.join("deckard-props.list")).unwrap(),
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
    sync(&paths).unwrap();
    assert!(!paths.work.join("overlay").exists());
    fs::rename(
        paths.share.join("removed-overlay"),
        paths.share.join("overlay"),
    )
    .unwrap();
    fs::write(paths.work.join("deckard-overlay.list"), "../../escape\n").unwrap();
    assert_eq!(sync(&paths).unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn refuses_symlinked_overlay_parent() {
    let (_root, paths) = setup();
    let outside = paths.work.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(paths.work.join("overlay")).unwrap();
    fs::create_dir_all(paths.work.join("overlay/system")).unwrap();
    symlink(&outside, paths.work.join("overlay/system/other")).unwrap();
    fs::write(paths.share.join("whiteouts"), "system/other/file\n").unwrap();
    assert_eq!(sync(&paths).unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}
