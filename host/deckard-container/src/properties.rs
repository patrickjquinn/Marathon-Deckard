//! Compose the per-session Android property file from initialized host data.

use crate::{Error, Result};
use rustix::process::geteuid;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BASE_BYTES: u64 = 1024 * 1024;
const SESSION_KEYS: &[&str] = &[
    "waydroid.host.user",
    "waydroid.host.uid",
    "waydroid.host.gid",
    "waydroid.host_data_path",
    "waydroid.background_start",
    "waydroid.xdg_runtime_dir",
    "waydroid.pulse_runtime_path",
    "waydroid.wayland_display",
    "waydroid.stub_sensors_hal",
    "ro.sf.lcd_density",
];

fn value<'a>(fields: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    let value = fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid(format!("missing session field: {key}")))?;
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(|character| character.is_control())
    {
        return Err(Error::Invalid(format!("invalid session property: {key}")));
    }
    Ok(value)
}

pub fn render(
    base: &str,
    fields: &HashMap<String, String>,
    sensors_available: bool,
) -> Result<String> {
    if base.is_empty() || base.len() as u64 > MAX_BASE_BYTES || base.contains('\0') {
        return Err(Error::Invalid("invalid base property file".into()));
    }
    let uid = value(fields, "user_id")?;
    let gid = value(fields, "group_id")?;
    uid.parse::<u32>()
        .map_err(|_| Error::Invalid("invalid user ID".into()))?;
    gid.parse::<u32>()
        .map_err(|_| Error::Invalid("invalid group ID".into()))?;
    let user = value(fields, "user_name")?;
    if user.len() > 64
        || !user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(Error::Invalid("invalid user name".into()));
    }
    let data = value(fields, "waydroid_data")?;
    if !data.starts_with('/') {
        return Err(Error::Invalid("Android data path must be absolute".into()));
    }
    let background = value(fields, "background_start")?;
    if !matches!(background, "true" | "false") {
        return Err(Error::Invalid("invalid background_start".into()));
    }
    let density = value(fields, "lcd_density")?
        .parse::<u16>()
        .map_err(|_| Error::Invalid("invalid LCD density".into()))?;
    if density > 1000 {
        return Err(Error::Invalid("LCD density is out of range".into()));
    }

    let mut result = String::new();
    for line in base.lines() {
        if line.contains('\r') {
            return Err(Error::Invalid("invalid base property line".into()));
        }
        let key = line.split_once('=').map_or(line, |(key, _)| key);
        if !SESSION_KEYS.contains(&key) || (key == "ro.sf.lcd_density" && density == 0) {
            result.push_str(line);
            result.push('\n');
        }
    }
    for (key, entry) in [
        ("waydroid.host.user", user),
        ("waydroid.host.uid", uid),
        ("waydroid.host.gid", gid),
        ("waydroid.host_data_path", data),
        ("waydroid.background_start", background),
        ("waydroid.xdg_runtime_dir", "/run/xdg"),
        ("waydroid.pulse_runtime_path", "/run/xdg/pulse"),
        ("waydroid.wayland_display", "wayland-0"),
    ] {
        result.push_str(&format!("{key}={entry}\n"));
    }
    if !sensors_available {
        result.push_str("waydroid.stub_sensors_hal=1\n");
    }
    if density != 0 {
        result.push_str(&format!("ro.sf.lcd_density={density}\n"));
    }
    Ok(result)
}

pub fn write(work: &Path, fields: &HashMap<String, String>, uid: u32) -> Result<()> {
    let base_path = work.join("waydroid_base.prop");
    let base_metadata = fs::symlink_metadata(&base_path)?;
    if !base_metadata.is_file()
        || base_metadata.file_type().is_symlink()
        || base_metadata.uid() != geteuid().as_raw()
        || base_metadata.len() > MAX_BASE_BYTES
    {
        return Err(Error::Invalid("untrusted base property file".into()));
    }
    if value(fields, "user_id")? != uid.to_string() {
        return Err(Error::Invalid(
            "session UID does not match property UID".into(),
        ));
    }
    let base = fs::read_to_string(&base_path)?;
    let properties = render(
        &base,
        fields,
        Path::new("/usr/bin/waydroid-sensord").exists(),
    )?;
    let work_metadata = fs::symlink_metadata(work)?;
    if !work_metadata.is_dir()
        || work_metadata.file_type().is_symlink()
        || work_metadata.uid() != geteuid().as_raw()
        || work_metadata.mode() & 0o022 != 0
    {
        return Err(Error::Invalid("untrusted Android work directory".into()));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Invalid(error.to_string()))?
        .as_nanos();
    let temporary = work.join(format!(".waydroid.prop.{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let result: Result<()> = (|| {
        file.write_all(properties.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, work.join("waydroid.prop"))?;
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

    #[test]
    fn appends_session_properties_without_duplicate_density() -> Result<()> {
        let fields = HashMap::from([
            ("user_id".into(), "1000".into()),
            ("group_id".into(), "1000".into()),
            ("user_name".into(), "user".into()),
            ("waydroid_data".into(), "/home/user/data".into()),
            ("background_start".into(), "true".into()),
            ("lcd_density".into(), "254".into()),
        ]);
        let result = render(
            "ro.hardware.egl=mesa\nro.sf.lcd_density=200\n",
            &fields,
            false,
        )?;
        assert!(result.contains("ro.hardware.egl=mesa\n"));
        assert!(result.contains("waydroid.host_data_path=/home/user/data\n"));
        assert_eq!(result.matches("ro.sf.lcd_density=").count(), 1);
        assert!(result.contains("waydroid.stub_sensors_hal=1\n"));
        Ok(())
    }

    #[test]
    fn rejects_property_injection() {
        let fields = HashMap::from([
            ("user_id".into(), "1000".into()),
            ("group_id".into(), "1000".into()),
            ("user_name".into(), "user\nro.adb.secure=0".into()),
            ("waydroid_data".into(), "/home/user/data".into()),
            ("background_start".into(), "true".into()),
            ("lcd_density".into(), "254".into()),
        ]);
        assert!(render("ro.adb.secure=1\n", &fields, false).is_err());
    }

    #[test]
    fn zero_density_keeps_image_default() -> Result<()> {
        let fields = HashMap::from([
            ("user_id".into(), "1000".into()),
            ("group_id".into(), "1000".into()),
            ("user_name".into(), "user".into()),
            ("waydroid_data".into(), "/home/user/data".into()),
            ("background_start".into(), "true".into()),
            ("lcd_density".into(), "0".into()),
        ]);
        let result = render("ro.sf.lcd_density=254\n", &fields, false)?;
        assert_eq!(result.matches("ro.sf.lcd_density=254").count(), 1);
        Ok(())
    }
}
