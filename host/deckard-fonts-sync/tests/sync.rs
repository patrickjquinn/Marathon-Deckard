#![allow(clippy::unwrap_used, reason = "isolated test fixtures")]

use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use deckard_fonts_sync::{Outcome, Paths, Whiteouts, sync};
use rustix::fs::{self, Mode, OFlags};

#[derive(Default)]
struct Markers {
    fail: bool,
    names: Vec<String>,
}

impl Whiteouts for Markers {
    fn create(&mut self, directory: &OwnedFd, name: &OsStr) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::other("injected whiteout failure"));
        }
        self.names.push(name.to_string_lossy().into_owned());
        fs::openat(
            directory,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )?;
        Ok(())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let share = root.join("share");
        let host_fonts = root.join("host-fonts");
        let overlay_system = root.join("waydroid/overlay/system");
        let state = root.join("waydroid/deckard-fonts.list");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&host_fonts).unwrap();
        std::fs::create_dir_all(&overlay_system).unwrap();
        let fixture = Self {
            _temp: temp,
            root,
            paths: Paths {
                share,
                host_fonts,
                overlay_system,
                state,
            },
        };
        fixture.share("fonts.xml", "<familyset/>");
        fixture.share("fonts_customization.xml", "<fonts-modification/>");
        fixture.share("host-fonts.txt", "Required.ttf\n");
        fixture.share("android-fonts.txt", "Optional.ttf\nMissingOptional.ttf\n");
        fixture.share("hidden-fonts.txt", "Old.ttf\nproduct/Lato.ttf\n");
        fixture.host("nested/Required.ttf", "required");
        fixture.host("Optional.ttf", "optional");
        fixture
    }

    fn share(&self, name: &str, contents: &str) {
        std::fs::write(self.paths.share.join(name), contents).unwrap();
    }

    fn host(&self, name: &str, contents: &str) {
        let path = self.paths.host_fonts.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn overlay(&self, name: &str) -> PathBuf {
        self.paths.overlay_system.join(name)
    }

    fn run(&self, whiteouts: &mut Markers) -> io::Result<Outcome> {
        sync(&self.paths, whiteouts)
    }
}

#[test]
fn installs_fonts_xml_whiteouts_and_state_then_reconciles() {
    let fixture = Fixture::new();
    let mut whiteouts = Markers::default();
    assert_eq!(fixture.run(&mut whiteouts).unwrap(), Outcome::Applied);
    assert_eq!(
        std::fs::read_to_string(fixture.overlay("fonts/Required.ttf")).unwrap(),
        "required"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.overlay("fonts/Optional.ttf")).unwrap(),
        "optional"
    );
    assert!(!fixture.overlay("fonts/MissingOptional.ttf").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.overlay("etc/fonts.xml")).unwrap(),
        "<familyset/>"
    );
    assert_eq!(whiteouts.names, ["Old.ttf", "Lato.ttf"]);
    assert_eq!(
        std::fs::read_to_string(&fixture.paths.state).unwrap(),
        "fonts/Required.ttf\nfonts/Optional.ttf\nfonts/Old.ttf\nproduct/fonts/Lato.ttf\netc/fonts.xml\nproduct/etc/fonts_customization.xml\n"
    );
    assert_eq!(
        std::fs::metadata(fixture.overlay("fonts/Required.ttf"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );

    fixture.share("hidden-fonts.txt", "");
    fixture.share("android-fonts.txt", "");
    fixture.host("nested/Required.ttf", "updated");
    assert_eq!(fixture.run(&mut whiteouts).unwrap(), Outcome::Applied);
    assert_eq!(
        std::fs::read_to_string(fixture.overlay("fonts/Required.ttf")).unwrap(),
        "updated"
    );
    assert!(!fixture.overlay("fonts/Old.ttf").exists());
    assert!(!fixture.overlay("product/fonts/Lato.ttf").exists());
    assert!(!fixture.overlay("fonts/Optional.ttf").exists());
}

#[test]
fn missing_required_font_removes_previous_overlay_without_optional_inputs() {
    let fixture = Fixture::new();
    fixture.run(&mut Markers::default()).unwrap();
    std::fs::remove_file(fixture.paths.host_fonts.join("nested/Required.ttf")).unwrap();
    std::fs::remove_file(fixture.paths.share.join("android-fonts.txt")).unwrap();
    let outcome = fixture.run(&mut Markers::default()).unwrap();
    assert_eq!(
        outcome,
        Outcome::MissingRequiredFonts(vec!["Required.ttf".into()])
    );
    assert!(!fixture.overlay("fonts/Required.ttf").exists());
    assert!(!fixture.overlay("etc/fonts.xml").exists());
    assert!(!fixture.paths.state.exists());
}

#[test]
fn missing_host_font_root_uses_fallback() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(&fixture.paths.host_fonts).unwrap();
    assert_eq!(
        fixture.run(&mut Markers::default()).unwrap(),
        Outcome::MissingRequiredFonts(vec!["Required.ttf".into()])
    );
    assert!(!fixture.paths.state.exists());
}

#[test]
fn missing_share_or_manifest_cleans_prior_installation() {
    let fixture = Fixture::new();
    fixture.run(&mut Markers::default()).unwrap();
    std::fs::remove_dir_all(&fixture.paths.share).unwrap();
    assert_eq!(
        fixture.run(&mut Markers::default()).unwrap(),
        Outcome::MissingManifest
    );
    assert!(!fixture.overlay("fonts/Required.ttf").exists());
    assert!(!fixture.paths.state.exists());
}

#[test]
fn rejects_manifest_traversal_before_cleanup() {
    let fixture = Fixture::new();
    fixture.run(&mut Markers::default()).unwrap();
    fixture.share("hidden-fonts.txt", "product/../../outside\n");
    assert!(fixture.run(&mut Markers::default()).is_err());
    assert!(fixture.overlay("fonts/Required.ttf").exists());
    assert!(fixture.paths.state.exists());
}

#[test]
fn rejects_untrusted_state_without_deleting_outside_files() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    std::fs::write(&outside, "keep").unwrap();
    std::fs::write(&fixture.paths.state, "../../outside\n").unwrap();
    assert!(fixture.run(&mut Markers::default()).is_err());
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "keep");
}

#[test]
fn rejects_symlinked_overlay_directory_and_ignores_symlinked_font() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, fixture.overlay("fonts")).unwrap();
    assert!(fixture.run(&mut Markers::default()).is_err());
    assert!(!outside.join("Required.ttf").exists());

    std::fs::remove_file(fixture.overlay("fonts")).unwrap();
    std::fs::remove_file(fixture.paths.host_fonts.join("nested/Required.ttf")).unwrap();
    symlink(
        fixture.root.join("outside/Required.ttf"),
        fixture.paths.host_fonts.join("nested/Required.ttf"),
    )
    .unwrap();
    assert_eq!(
        fixture.run(&mut Markers::default()).unwrap(),
        Outcome::MissingRequiredFonts(vec!["Required.ttf".into()])
    );
}

#[test]
fn failed_whiteout_rolls_back_new_files_and_keeps_old_state() {
    let fixture = Fixture::new();
    let mut whiteouts = Markers {
        fail: true,
        names: Vec::new(),
    };
    assert!(fixture.run(&mut whiteouts).is_err());
    assert!(!fixture.overlay("fonts/Required.ttf").exists());
    assert!(!fixture.paths.state.exists());
    assert_eq!(
        std::fs::read_dir(fixture.root.join("waydroid"))
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        1
    );
}
