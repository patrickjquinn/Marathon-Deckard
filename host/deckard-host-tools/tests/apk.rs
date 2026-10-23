#![allow(
    clippy::unwrap_used,
    reason = "tests create isolated APK files and parse the constructed request"
)]

use deckard_host_tools::apk;

#[test]
fn constructs_json_arguments_without_shell_interpolation() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("app '\" $().aPk");
    std::fs::write(&source, "fixture").unwrap();
    let arguments = apk::arguments(&source).unwrap();
    assert_eq!(arguments[2], "org.marathonos.store");
    assert_eq!(arguments[7], "apps.openFile");
    let json: serde_json::Value = serde_json::from_str(&arguments[8]).unwrap();
    assert_eq!(
        json["path"].as_str(),
        source.canonicalize().unwrap().to_str()
    );
    assert_eq!(arguments[9], "0");
}

#[test]
fn rejects_non_apks_directories_and_absent_files() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("note.txt");
    std::fs::write(&file, "fixture").unwrap();
    assert!(apk::arguments(&file).is_err());
    let directory = temporary.path().join("directory.apk");
    std::fs::create_dir(&directory).unwrap();
    assert!(apk::arguments(&directory).is_err());
    assert!(apk::arguments(&temporary.path().join("missing.apk")).is_err());
}
