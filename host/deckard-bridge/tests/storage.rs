#![allow(
    clippy::unwrap_used,
    reason = "the test drives fixed temporary folders"
)]

use std::fs;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use deckard_bridge::storage::{Folder, SETTLE, SharedStorage};
use serde_json::{Map, Value, json};

fn paths(message: &Map<String, Value>) -> Vec<Value> {
    let mut paths = message["paths"].as_array().unwrap().clone();
    paths.sort_by_key(|path| path.to_string());
    paths
}

fn names_media_rw(path: &std::path::Path) -> bool {
    let mut buffer = [0u8; 512];
    let length = rustix::fs::lgetxattr(path, "system.posix_acl_access", &mut buffer).unwrap_or(0);
    deckard_core::acl::names_group(&buffer[..length], 1023)
}

#[test]
fn indexes_what_the_host_changes() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let pictures = home.path().join("Pictures");
    let marathon = pictures.join("Marathon");
    fs::create_dir_all(marathon.join("old"))?;
    fs::write(marathon.join("old/a.jpg"), "x")?;
    let stamp = home.path().join("state/shared-acl");
    let folders = vec![
        Folder {
            host: pictures.clone(),
            android: "Pictures",
        },
        Folder {
            host: home.path().join("DCIM"),
            android: "DCIM",
        },
    ];
    let (tx, sent) = mpsc::channel();
    let tx = Mutex::new(tx);
    let send = Arc::new(move |message| tx.lock().unwrap().send(message).is_ok());
    let storage = SharedStorage::start(folders, stamp.clone(), send)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.path().join("DCIM").is_dir() {
        assert!(Instant::now() < deadline, "the folders were never prepared");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    let acls = stamp.exists();
    if acls {
        assert!(names_media_rw(&marathon.join("old/a.jpg")));
    }

    // Changes while Android is away wait for it.
    fs::write(marathon.join("b.jpg"), "x")?;
    assert!(
        sent.recv_timeout(SETTLE + Duration::from_millis(500))
            .is_err()
    );
    storage.set_connected(true);
    let scan = sent.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(scan["t"], "media_scan");
    let expected = json!([
        "/storage/emulated/0/DCIM",
        "/storage/emulated/0/Pictures",
        "/storage/emulated/0/Pictures/Marathon",
    ]);
    assert_eq!(Value::Array(paths(&scan)), expected);

    fs::write(marathon.join("c.jpg"), "x")?;
    let scan = sent.recv_timeout(SETTLE + Duration::from_secs(2))?;
    assert_eq!(
        scan["paths"],
        json!(["/storage/emulated/0/Pictures/Marathon"])
    );
    if acls {
        assert!(names_media_rw(&marathon.join("c.jpg")));
    }
    Ok(())
}
