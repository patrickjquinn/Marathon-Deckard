#![allow(
    clippy::unwrap_used,
    reason = "integration tests create isolated CLI fixtures"
)]

use std::path::Path;
use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_deckard-image-tools"))
}

#[test]
fn fonts_command_writes_all_four_matching_outputs() {
    let output = tempfile::tempdir().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor_deckard/fonts");
    let result = command()
        .arg("fonts")
        .arg("--input")
        .arg(&input)
        .arg("--out")
        .arg(output.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for name in [
        "fonts.xml",
        "host-fonts.txt",
        "android-fonts.txt",
        "hidden-fonts.txt",
    ] {
        assert_eq!(
            std::fs::read(output.path().join(name)).unwrap(),
            std::fs::read(input.join(name)).unwrap()
        );
    }
}

#[test]
fn palette_command_validates_options_before_changing_files() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("colors.xml");
    let result = command()
        .args(["palette", "--highlight", "#ff8a3d", "--out"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(result.status.success());
    let xml = std::fs::read_to_string(&output).unwrap();
    assert_eq!(xml.matches("<color name=").count(), 65);
    assert!(xml.contains("system_accent1_100\">#ff8a3d"));
    for options in [
        vec!["--highlight", "bad"],
        vec!["--unknown", "value"],
        vec!["--out", "duplicate"],
    ] {
        let result = command()
            .arg("palette")
            .arg("--out")
            .arg(&output)
            .args(options)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), xml);
    }
}

#[test]
fn failed_permission_patch_preserves_original_file() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("GrantPermissionsActivity.smali");
    std::fs::write(&source, "unknown image\n").unwrap();
    let result = command()
        .arg("permission-view")
        .arg("--input")
        .arg(&source)
        .arg("--out")
        .arg(&source)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read_to_string(source).unwrap(), "unknown image\n");
}

#[test]
fn accepts_equals_options_for_existing_build_script_callers() {
    let temporary = tempfile::tempdir().unwrap();
    let output = temporary.path().join("colors.xml");
    let result = command()
        .args(["palette", "--highlight=#ff8a3d"])
        .arg(format!("--out={}", output.display()))
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        std::fs::read_to_string(output)
            .unwrap()
            .contains("system_accent1_100\">#ff8a3d")
    );
}

#[test]
fn hwcomposer_command_matches_golden_output_and_refuses_unknown_input() {
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("stock.so");
    let output = temporary.path().join("patched.so");
    let golden = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../overlay/vendor/lib64/hw/hwcomposer.waydroid.so"),
    )
    .unwrap();
    let mut stock = golden.clone();
    stock[0x3eee0..0x3eee4].copy_from_slice(&[0x20, 0x00, 0x80, 0x52]);
    stock[0x429b4..0x429b8].copy_from_slice(&[0xe7, 0xa3, 0x00, 0x94]);
    stock[0x52db4..0x52dc0].copy_from_slice(&[0x1f, 0x20, 0x03, 0xd5].repeat(3));
    std::fs::write(&input, &stock).unwrap();

    let result = command()
        .arg("hwcomposer")
        .arg("--input")
        .arg(&input)
        .arg("--out")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&output).unwrap(), golden);

    stock[0] ^= 1;
    std::fs::write(&input, &stock).unwrap();
    let result = command()
        .arg("hwcomposer")
        .arg("--input")
        .arg(&input)
        .arg("--out")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), golden);
}
