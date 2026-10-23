//! Android launchers as desktop entries (upstream `tools/helpers/apps.py`),
//! merged into any existing entry so user edits survive.

use std::collections::HashSet;
use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use deckard_host_tools::files;
use deckard_launch::binder::AppInfo;

use crate::Result;

const LAUNCHER: &str = "android.intent.category.LAUNCHER";
const DECKARD_LAUNCH: &str = "/usr/libexec/deckard-launch";
const ENTRY: &str = "Desktop Entry";
const SETTINGS: &str = "Desktop Action app-settings";
const SYSTEM_APPS: &[&str] = &[
    "com.android.calculator2",
    "com.android.camera2",
    "com.android.contacts",
    "com.android.deskclock",
    "com.android.documentsui",
    "com.android.email",
    "com.android.gallery3d",
    "com.android.inputmethod.latin",
    "com.android.settings",
    "com.google.android.gms",
    "org.lineageos.aperture",
    "org.lineageos.eleven",
    "org.lineageos.etar",
    "org.lineageos.jelly",
    "org.lineageos.recorder",
];

fn valid_package(package: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    package.contains('.') && package.split('.').all(part)
}

/// A desktop entry's lines, edited in place.
struct DesktopFile(Vec<String>);

impl DesktopFile {
    /// The line range of `group`'s keys.
    fn group(&self, group: &str) -> Option<(usize, usize)> {
        let header = format!("[{group}]");
        let start = self.0.iter().position(|line| line.trim() == header)? + 1;
        let next = self.0[start..]
            .iter()
            .position(|line| line.starts_with('[') && line.ends_with(']'));
        Some((start, next.map_or(self.0.len(), |index| start + index)))
    }

    fn find(&self, group: &str, key: &str) -> Option<usize> {
        let (start, end) = self.group(group)?;
        (start..end).find(|&index| {
            self.0[index]
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('='))
        })
    }

    fn get(&self, group: &str, key: &str) -> Option<&str> {
        let index = self.find(group, key)?;
        Some(&self.0[index][key.len() + 1..])
    }

    fn set(&mut self, group: &str, key: &str, value: &str) {
        let line = format!("{key}={value}");
        match (self.find(group, key), self.group(group)) {
            (Some(index), _) => self.0[index] = line,
            (None, Some((_, end))) => self.0.insert(end, line),
            (None, None) => {
                if !self.0.is_empty() {
                    self.0.push(String::new());
                }
                self.0.push(format!("[{group}]"));
                self.0.push(line);
            }
        }
    }

    /// Puts `first` at the head of the `;` list `key`.
    fn prepend(&mut self, group: &str, key: &str, first: &str) {
        let mut values = vec![first];
        let current = self.get(group, key).unwrap_or_default().to_owned();
        values.extend(
            current
                .split(';')
                .filter(|value| !value.is_empty() && *value != first),
        );
        self.set(group, key, &format!("{};", values.join(";")));
    }
}

fn render_app(existing: &str, app: &AppInfo, icon_dir: &Path) -> String {
    let package = &app.package;
    let icon = |name: &str| {
        icon_dir
            .join(format!("{name}.png"))
            .to_string_lossy()
            .into_owned()
    };
    let name = app
        .name
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    let mut desktop = DesktopFile(existing.lines().map(str::to_owned).collect());
    desktop.set(ENTRY, "Type", "Application");
    desktop.set(ENTRY, "Name", &name);
    desktop.set(
        ENTRY,
        "Exec",
        &format!("{DECKARD_LAUNCH} app launch {package}"),
    );
    desktop.set(ENTRY, "Icon", &icon(package));
    desktop.prepend(ENTRY, "Categories", "X-WayDroid-App");
    desktop.set(ENTRY, "X-Purism-FormFactor", "Workstation;Mobile;");
    desktop.prepend(ENTRY, "Actions", "app-settings");
    if SYSTEM_APPS.contains(&package.as_str()) && desktop.get(ENTRY, "NoDisplay").is_none() {
        desktop.set(ENTRY, "NoDisplay", "true");
    }
    desktop.set(SETTINGS, "Name", "App Settings");
    let settings = format!(
        "{DECKARD_LAUNCH} app intent android.settings.APPLICATION_DETAILS_SETTINGS package:{package}"
    );
    desktop.set(SETTINGS, "Exec", &settings);
    desktop.set(SETTINGS, "Icon", &icon("com.android.settings"));
    let mut content = desktop.0.join("\n");
    content.push('\n');
    content
}

fn is_launcher(app: &AppInfo) -> bool {
    app.categories
        .iter()
        .any(|category| category.trim() == LAUNCHER)
}

fn valid_app(app: &AppInfo) -> bool {
    valid_package(&app.package) && !app.name.is_empty() && !app.name.contains('\0')
}

fn applications_dir(data_home: &Path) -> Result<PathBuf> {
    let applications = data_home.join("applications");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&applications)?;
    Ok(applications)
}

fn launcher_path(applications: &Path, package: &str) -> PathBuf {
    applications.join(format!("waydroid.{package}.desktop"))
}

fn write_app(applications: &Path, data_home: &Path, app: &AppInfo) -> Result<()> {
    let path = launcher_path(applications, &app.package);
    let existing = files::read_optional(&path)?;
    let next = render_app(&existing, app, &data_home.join("waydroid/data/icons"));
    if next != existing {
        files::replace(&path, next.as_bytes(), 0o644)?;
    }
    Ok(())
}

/// Rewrites every launcher from a full Android snapshot and removes the
/// launchers of packages it no longer lists. Returns the launcher count.
///
/// # Errors
/// The snapshot is empty or malformed (nothing is changed), or a write fails.
pub fn export(data_home: &Path, apps: &[AppInfo]) -> Result<usize> {
    // An empty or malformed Binder snapshot must never delete existing launchers.
    if apps.is_empty() {
        return Err("empty Android app snapshot".into());
    }
    let mut packages = HashSet::new();
    for app in apps {
        if !valid_app(app) {
            return Err("invalid Android app identity".into());
        }
        if !packages.insert(app.package.as_str()) {
            return Err("duplicate Android app identity".into());
        }
    }
    let applications = applications_dir(data_home)?;
    let mut visible = HashSet::new();
    let launchers = apps.iter().filter(|app| is_launcher(app));
    for app in launchers {
        write_app(&applications, data_home, app)?;
        visible.insert(app.package.as_str());
    }
    for entry in fs::read_dir(&applications)? {
        let entry = entry?;
        let name = entry.file_name();
        let package = name
            .to_str()
            .and_then(|name| name.strip_prefix("waydroid.")?.strip_suffix(".desktop"));
        if let Some(package) = package
            && valid_package(package)
            && !visible.contains(package)
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    File::open(&applications)?.sync_all()?;
    Ok(visible.len())
}

/// Applies one package change from Android: `app` is its current entry, or
/// `None` once it is removed. Only launcher apps keep a launcher.
///
/// # Errors
/// The package name or entry is invalid, or a write fails.
pub fn update(data_home: &Path, package: &str, app: Option<&AppInfo>) -> Result<()> {
    if !valid_package(package) || app.is_some_and(|app| app.package != package || !valid_app(app)) {
        return Err("invalid Android package change".into());
    }
    let applications = applications_dir(data_home)?;
    match app.filter(|app| is_launcher(app)) {
        Some(app) => write_app(&applications, data_home, app)?,
        None => {
            if files::found(fs::remove_file(launcher_path(&applications, package)))?.is_none() {
                return Ok(());
            }
        }
    }
    File::open(&applications)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, package: &str, visible: bool) -> AppInfo {
        AppInfo {
            name: name.to_owned(),
            package: package.to_owned(),
            categories: if visible {
                vec![LAUNCHER.to_owned()]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn matches_live_desktop_identity_icon_and_action() -> Result<()> {
        let root = tempfile::tempdir()?;
        export(
            root.path(),
            &[app("Feeder", "com.nononsenseapps.feeder", true)],
        )?;
        let entry = fs::read_to_string(
            root.path()
                .join("applications/waydroid.com.nononsenseapps.feeder.desktop"),
        )?;
        assert!(
            entry.contains(
                "Exec=/usr/libexec/deckard-launch app launch com.nononsenseapps.feeder\n"
            )
        );
        assert!(entry.contains(&format!(
                "Icon={}\n",
                root.path()
                    .join("waydroid/data/icons/com.nononsenseapps.feeder.png")
                    .display()
            )));
        assert!(entry.contains("Categories=X-WayDroid-App;\n"));
        assert!(entry.contains("Actions=app-settings;\n"));
        assert!(entry.contains("Exec=/usr/libexec/deckard-launch app intent android.settings.APPLICATION_DETAILS_SETTINGS package:com.nononsenseapps.feeder\n"));
        Ok(())
    }

    #[test]
    fn preserves_custom_fields_and_nodisplay_and_is_idempotent() -> Result<()> {
        let root = tempfile::tempdir()?;
        let apps_dir = root.path().join("applications");
        fs::create_dir(&apps_dir)?;
        let path = apps_dir.join("waydroid.com.android.settings.desktop");
        fs::write(
            &path,
            "# custom\n[Desktop Entry]\nName=Old\nName[fr]=Ancien\nCategories=Utility;\nActions=custom;\nNoDisplay=false\nX-Custom=yes\n\n[Desktop Action custom]\nName=Custom\n",
        )?;
        let apps = [app("Settings", "com.android.settings", true)];
        export(root.path(), &apps)?;
        let first = fs::read_to_string(&path)?;
        assert!(first.contains("Name[fr]=Ancien"));
        assert!(first.contains("Categories=X-WayDroid-App;Utility;"));
        assert!(first.contains("Actions=app-settings;custom;"));
        assert!(first.contains("NoDisplay=false"));
        assert!(first.contains("[Desktop Action custom]\nName=Custom"));
        export(root.path(), &apps)?;
        assert_eq!(first, fs::read_to_string(&path)?);
        assert_eq!(fs::read_dir(&apps_dir)?.count(), 1);
        Ok(())
    }

    #[test]
    fn reconciles_only_own_stale_entries_after_valid_snapshot() -> Result<()> {
        let root = tempfile::tempdir()?;
        let apps_dir = root.path().join("applications");
        fs::create_dir(&apps_dir)?;
        let stale = apps_dir.join("waydroid.org.example.stale.desktop");
        let hidden = apps_dir.join("waydroid.org.example.hidden.desktop");
        let foreign = apps_dir.join("native.desktop");
        for path in [&stale, &hidden, &foreign] {
            fs::write(path, "old")?;
        }
        let apps = [
            app("Hidden", "org.example.hidden", false),
            app("Visible", "org.example.visible", true),
        ];
        assert_eq!(export(root.path(), &apps)?, 1);
        assert!(!stale.exists());
        assert!(!hidden.exists());
        assert!(foreign.exists());
        assert!(
            apps_dir
                .join("waydroid.org.example.visible.desktop")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn rejects_bad_snapshots_without_deleting_existing_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let apps_dir = root.path().join("applications");
        fs::create_dir(&apps_dir)?;
        let old = apps_dir.join("waydroid.org.example.old.desktop");
        fs::write(&old, "old")?;
        assert!(export(root.path(), &[]).is_err());
        assert!(export(root.path(), &[app("Bad", "../escape", true)]).is_err());
        assert!(
            export(
                root.path(),
                &[
                    app("A", "org.example.a", true),
                    app("B", "org.example.a", true)
                ]
            )
            .is_err()
        );
        assert!(old.exists());
        Ok(())
    }

    #[test]
    fn escapes_untrusted_android_label() {
        let entry = render_app(
            "",
            &app("A\nB\\C", "org.example.app", true),
            Path::new("/icons"),
        );
        assert!(entry.contains("Name=A\\nB\\\\C\n"));
        assert!(!entry.contains("Name=A\nB"));
    }

    #[test]
    fn package_changes_add_update_and_remove_one_launcher() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root
            .path()
            .join("applications/waydroid.org.example.app.desktop");
        let other = root
            .path()
            .join("applications/waydroid.org.example.other.desktop");
        export(root.path(), &[app("Other", "org.example.other", true)])?;
        update(
            root.path(),
            "org.example.app",
            Some(&app("App", "org.example.app", true)),
        )?;
        assert!(fs::read_to_string(&path)?.contains("Name=App\n"));
        update(
            root.path(),
            "org.example.app",
            Some(&app("App", "org.example.app", false)),
        )?;
        assert!(!path.exists());
        update(
            root.path(),
            "org.example.app",
            Some(&app("App", "org.example.app", true)),
        )?;
        update(root.path(), "org.example.app", None)?;
        assert!(!path.exists());
        update(root.path(), "org.example.app", None)?;
        assert!(
            update(
                root.path(),
                "org.example.app",
                Some(&app("Spoof", "org.example.other", true))
            )
            .is_err()
        );
        assert!(update(root.path(), "../escape", None).is_err());
        assert!(other.exists());
        Ok(())
    }
}
