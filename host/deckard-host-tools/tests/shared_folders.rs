#![allow(
    clippy::unwrap_used,
    reason = "tests use isolated directories and record privileged operations"
)]

use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use deckard_host_tools::shared_folders::{self, Operations};

#[derive(Default)]
struct Recorded {
    mounts: Vec<(PathBuf, PathBuf)>,
    owners: Vec<(u32, u32)>,
}

impl Operations for Recorded {
    fn chown(&mut self, _: &OwnedFd, uid: u32, gid: u32) -> std::io::Result<()> {
        self.owners.push((uid, gid));
        Ok(())
    }

    fn bind(&mut self, source: &OwnedFd, target: &OwnedFd) -> std::io::Result<()> {
        self.mounts.push((
            std::fs::read_link(format!("/proc/self/fd/{}", source.as_raw_fd()))?,
            std::fs::read_link(format!("/proc/self/fd/{}", target.as_raw_fd()))?,
        ));
        Ok(())
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let root = home.join(".local/share/deckard");
    let data = root.join("data");
    std::fs::create_dir_all(data.join("media/0/Pictures")).unwrap();
    std::fs::create_dir_all(home.join(".config")).unwrap();
    std::fs::create_dir_all(home.join("Bilder")).unwrap();
    std::fs::write(home.join(".config/user-dirs.dirs"),
        "XDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_MUSIC_DIR=\"$HOME/../etc\"\nXDG_VIDEOS_DIR=\"/etc\"\n").unwrap();
    (temporary, home, root, data)
}

fn share(
    root: &Path,
    home: &Path,
    data: &Path,
    recorded: &mut Recorded,
) -> std::io::Result<shared_folders::Report> {
    shared_folders::share(
        root,
        home,
        data,
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
        recorded,
    )
}

#[test]
fn maps_all_folders_and_honors_safe_xdg_names() {
    let (_temporary, home, root, data) = fixture();
    let mut recorded = Recorded::default();
    let report = share(&root, &home, &data, &mut recorded).unwrap();
    assert_eq!(
        report.shared,
        [
            "Documents",
            "Download",
            "Pictures",
            "Music",
            "Movies",
            "DCIM"
        ]
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(recorded.mounts.len(), 6);
    assert!(
        recorded
            .mounts
            .contains(&(home.join("Bilder"), data.join("media/0/Pictures")))
    );
    assert!(home.join("Music").is_dir());
    assert!(!data.join("media/0/Android").exists());
}

#[test]
fn migrates_files_without_overwriting_existing_user_files() {
    let (_temporary, home, root, data) = fixture();
    let pictures = data.join("media/0/Pictures");
    std::fs::write(pictures.join("cat.jpg"), "cat").unwrap();
    std::fs::write(pictures.join("dog.jpg"), "Android dog").unwrap();
    std::fs::write(home.join("Bilder/dog.jpg"), "host dog").unwrap();
    std::fs::create_dir(pictures.join("Screenshots")).unwrap();
    let mut recorded = Recorded::default();
    share(&root, &home, &data, &mut recorded).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join("Bilder/cat.jpg")).unwrap(),
        "cat"
    );
    assert_eq!(
        std::fs::read_to_string(home.join("Bilder/dog.jpg")).unwrap(),
        "host dog"
    );
    assert_eq!(
        std::fs::read_to_string(pictures.join("dog.jpg")).unwrap(),
        "Android dog"
    );
    assert!(home.join("Bilder/Screenshots").is_dir());
    assert_eq!(
        std::fs::metadata(home.join("Bilder/cat.jpg"))
            .unwrap()
            .permissions()
            .mode()
            & 0o660,
        0o660
    );
}

#[test]
fn refuses_symlinked_mount_sources_and_targets() {
    let (temporary, home, root, data) = fixture();
    let secret = temporary.path().join("secret");
    std::fs::create_dir(&secret).unwrap();
    symlink(&secret, home.join("Documents")).unwrap();
    std::fs::remove_dir(data.join("media/0/Pictures")).unwrap();
    symlink(&secret, data.join("media/0/Pictures")).unwrap();
    let mut recorded = Recorded::default();
    let report = share(&root, &home, &data, &mut recorded).unwrap();
    assert!(!report.shared.contains(&"Documents".into()));
    assert!(!report.shared.contains(&"Pictures".into()));
    assert!(std::fs::read_dir(secret).unwrap().next().is_none());
}

#[test]
fn refuses_linked_data_and_data_outside_home() {
    let (temporary, home, root, data) = fixture();
    let elsewhere = temporary.path().join("elsewhere");
    std::fs::rename(home.join(".local"), &elsewhere).unwrap();
    symlink(&elsewhere, home.join(".local")).unwrap();
    let mut recorded = Recorded::default();
    assert!(share(&root, &home, &data, &mut recorded).is_err());
    assert!(
        share(
            &root,
            &home,
            Path::new("/var/lib/deckard/data"),
            &mut recorded
        )
        .is_err()
    );
    assert!(recorded.mounts.is_empty());
}

#[test]
fn refuses_container_data_with_a_different_inode() {
    let (temporary, home, _root, data) = fixture();
    let root = temporary.path().join("another-root");
    std::fs::create_dir_all(root.join("data/media/0/Pictures")).unwrap();
    let mut recorded = Recorded::default();
    let report = share(&root, &home, &data, &mut recorded).unwrap();
    assert!(report.shared.is_empty());
    assert!(recorded.mounts.is_empty());
}

#[test]
fn configuration_parser_accepts_only_direct_home_folders() {
    assert_eq!(
        shared_folders::session_data(
            "lxc.mount.entry = /home/user/.local/share/deckard/data data none rbind 0 0"
        ),
        Some("/home/user/.local/share/deckard/data")
    );
    let directories = shared_folders::user_directories(
        "XDG_MUSIC_DIR=\"$HOME/../etc\"\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n",
    );
    assert_eq!(directories.len(), 1);
    assert_eq!(
        directories.get("XDG_PICTURES_DIR").map(String::as_str),
        Some("Bilder")
    );
}
