//! Session-owned mounts included by an already initialized LXC instance.

use crate::{Error, Result};
use rustix::process::geteuid;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn field<'a>(fields: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing session field: {key}")))
}

fn safe_path(path: &Path) -> Result<()> {
    let value = path.to_string_lossy();
    if !path.is_absolute() || value.chars().any(char::is_whitespace) || value.contains('\0') {
        return Err(Error::Invalid(format!("unsafe LXC mount path: {value}")));
    }
    Ok(())
}

fn owned_path(path: &Path, uid: u32) -> Result<PathBuf> {
    safe_path(path)?;
    let canonical = fs::canonicalize(path)?;
    safe_path(&canonical)?;
    if canonical.metadata()?.uid() != uid {
        return Err(Error::Invalid(format!(
            "session path is not owned by UID {uid}: {}",
            path.display()
        )));
    }
    Ok(canonical)
}

pub fn render(fields: &HashMap<String, String>, uid: u32, runtime_root: &Path) -> Result<String> {
    let runtime = runtime_root.join(uid.to_string());
    safe_path(&runtime)?;
    if Path::new(field(fields, "xdg_runtime_dir")?) != runtime {
        return Err(Error::Invalid("unexpected XDG runtime directory".into()));
    }
    if runtime.metadata()?.uid() != uid {
        return Err(Error::Invalid(
            "XDG runtime directory has wrong owner".into(),
        ));
    }
    let display = field(fields, "wayland_display")?;
    if display.is_empty() || display.contains('/') || display.chars().any(char::is_whitespace) {
        return Err(Error::Invalid("invalid Wayland display name".into()));
    }
    let wayland = owned_path(&runtime.join(display), uid)?;
    if !wayland.starts_with(&runtime) || !wayland.metadata()?.file_type().is_socket() {
        return Err(Error::Invalid(
            "Wayland socket escapes runtime directory".into(),
        ));
    }
    let data = owned_path(Path::new(field(fields, "waydroid_data")?), uid)?;
    if !data.is_dir() {
        return Err(Error::Invalid(
            "Android data path is not a directory".into(),
        ));
    }
    let mut config = format!(
        "lxc.mount.entry = tmpfs /run/xdg none create=dir 0 0\n\
         lxc.mount.entry = {} run/xdg/wayland-0 none rbind,create=file 0 0\n",
        wayland.display()
    );
    let pulse = Path::new(field(fields, "pulse_runtime_path")?).join("native");
    if pulse.exists() {
        let pulse = owned_path(&pulse, uid)?;
        if !pulse.starts_with(&runtime) || !pulse.metadata()?.file_type().is_socket() {
            return Err(Error::Invalid(
                "Pulse socket escapes runtime directory".into(),
            ));
        }
        config.push_str(&format!(
            "lxc.mount.entry = {} run/xdg/pulse/native none rbind,create=file 0 0\n",
            pulse.display()
        ));
    }
    config.push_str(&format!(
        "lxc.mount.entry = {} data none rbind 0 0\n",
        data.display()
    ));
    Ok(config)
}

pub fn write(
    lxc_instance: &Path,
    fields: &HashMap<String, String>,
    uid: u32,
    runtime_root: &Path,
) -> Result<()> {
    let config = render(fields, uid, runtime_root)?;
    let directory = fs::symlink_metadata(lxc_instance)?;
    if !directory.is_dir()
        || directory.file_type().is_symlink()
        || directory.uid() != geteuid().as_raw()
        || directory.mode() & 0o022 != 0
    {
        return Err(Error::Invalid("untrusted LXC instance directory".into()));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Invalid(error.to_string()))?
        .as_nanos();
    let temporary = lxc_instance.join(format!(".config_session.{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let result: Result<()> = (|| {
        file.write_all(config.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, lxc_instance.join("config_session"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::os::unix::net::UnixListener;

    #[test]
    fn renders_and_publishes_session_mounts() -> std::result::Result<(), Box<dyn StdError>> {
        let root = tempfile::tempdir()?;
        let uid = geteuid().as_raw();
        let runtime_root = root.path().join("run");
        let runtime = runtime_root.join(uid.to_string());
        let pulse_dir = runtime.join("pulse");
        let data = root.path().join("data");
        let lxc = root.path().join("lxc");
        fs::create_dir_all(&pulse_dir)?;
        fs::create_dir_all(&data)?;
        fs::create_dir_all(&lxc)?;
        let _wayland = UnixListener::bind(runtime.join("marathon-wayland-0"))?;
        let _pulse = UnixListener::bind(pulse_dir.join("native"))?;
        let fields = HashMap::from([
            ("xdg_runtime_dir".into(), runtime.display().to_string()),
            ("wayland_display".into(), "marathon-wayland-0".into()),
            ("pulse_runtime_path".into(), pulse_dir.display().to_string()),
            ("waydroid_data".into(), data.display().to_string()),
        ]);
        let config = render(&fields, uid, &runtime_root)?;
        assert!(config.contains("run/xdg/wayland-0"));
        assert!(config.contains("run/xdg/pulse/native"));
        write(&lxc, &fields, uid, &runtime_root)?;
        assert_eq!(fs::read_to_string(lxc.join("config_session"))?, config);
        Ok(())
    }

    #[test]
    fn rejects_foreign_paths_and_display_injection() -> std::result::Result<(), Box<dyn StdError>> {
        let root = tempfile::tempdir()?;
        let uid = geteuid().as_raw();
        let runtime_root = root.path().join("run");
        let runtime = runtime_root.join(uid.to_string());
        fs::create_dir_all(&runtime)?;
        let _wayland = UnixListener::bind(runtime.join("wayland-0"))?;
        let data = root.path().join("data");
        fs::create_dir_all(&data)?;
        let mut fields = HashMap::from([
            ("xdg_runtime_dir".into(), runtime.display().to_string()),
            ("wayland_display".into(), "wayland-0\nmount".into()),
            ("pulse_runtime_path".into(), runtime.display().to_string()),
            ("waydroid_data".into(), data.display().to_string()),
        ]);
        assert!(render(&fields, uid, &runtime_root).is_err());
        fields.insert("wayland_display".into(), "wayland-0".into());
        assert!(render(&fields, uid + 1, &runtime_root).is_err());
        Ok(())
    }
}
