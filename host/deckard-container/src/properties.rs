//! The per-session Android property file, from the initialized base file.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use deckard_host_tools::{files, prop};

use crate::Result;

const MAX_BASE_BYTES: u64 = 1024 * 1024;
const SESSION_KEYS: &[&str] = &[
    prop!("host.user"),
    prop!("host.uid"),
    prop!("host.gid"),
    prop!("host_data_path"),
    prop!("background_start"),
    prop!("xdg_runtime_dir"),
    prop!("pulse_runtime_path"),
    prop!("wayland_display"),
    prop!("stub_sensors_hal"),
    "ro.sf.lcd_density",
];

fn value<'a>(fields: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    let value = fields
        .get(key)
        .ok_or(format!("missing session field: {key}"))?;
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(format!("invalid session property: {key}").into());
    }
    Ok(value)
}

/// `base` without the session keys, then the session's values.
///
/// # Errors
/// The base file or a session field is invalid.
pub fn render(
    base: &str,
    fields: &HashMap<String, String>,
    sensors_available: bool,
) -> Result<String> {
    let base_ok = !base.is_empty() && base.len() as u64 <= MAX_BASE_BYTES && !base.contains('\0');
    crate::ensure(base_ok, "invalid base property file")?;
    let uid = value(fields, "user_id")?;
    let gid = value(fields, "group_id")?;
    crate::ensure(uid.parse::<u32>().is_ok(), "invalid user ID")?;
    crate::ensure(gid.parse::<u32>().is_ok(), "invalid group ID")?;
    let user = value(fields, "user_name")?;
    let plain = user
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    crate::ensure(user.len() <= 64 && plain, "invalid user name")?;
    let data = value(fields, "deckard_data")?;
    crate::ensure(data.starts_with('/'), "Android data path must be absolute")?;
    let background = value(fields, "background_start")?;
    crate::ensure(
        matches!(background, "true" | "false"),
        "invalid background_start",
    )?;
    let density: u16 = value(fields, "lcd_density")?
        .parse()
        .map_err(|_| "invalid LCD density")?;
    crate::ensure(density <= 1000, "LCD density is out of range")?;

    let mut result = String::new();
    for line in base.lines() {
        crate::ensure(!line.contains('\r'), "invalid base property line")?;
        let key = line.split_once('=').map_or(line, |(key, _)| key);
        if !SESSION_KEYS.contains(&key) || (key == "ro.sf.lcd_density" && density == 0) {
            result.push_str(line);
            result.push('\n');
        }
    }
    for (key, entry) in [
        (prop!("host.user"), user),
        (prop!("host.uid"), uid),
        (prop!("host.gid"), gid),
        (prop!("host_data_path"), data),
        (prop!("background_start"), background),
        (prop!("xdg_runtime_dir"), "/run/xdg"),
        (prop!("pulse_runtime_path"), "/run/xdg/pulse"),
        (prop!("wayland_display"), "wayland-0"),
    ] {
        let _ = writeln!(result, "{key}={entry}");
    }
    if !sensors_available {
        let _ = writeln!(result, "{}=1", prop!("stub_sensors_hal"));
    }
    if density != 0 {
        let _ = writeln!(result, "ro.sf.lcd_density={density}");
    }
    Ok(result)
}

fn owned(path: &Path, message: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    let mine = metadata.uid() == rustix::process::geteuid().as_raw();
    crate::ensure(mine && !metadata.is_symlink(), message)?;
    Ok(metadata)
}

/// Writes `deckard.prop` for the session of `uid`.
///
/// # Errors
/// The base file or work directory is untrusted, the session does not
/// belong to `uid`, or the file cannot be written.
pub fn write(work: &Path, fields: &HashMap<String, String>, uid: u32) -> Result<()> {
    let base_path = work.join("deckard_base.prop");
    let untrusted = "untrusted base property file";
    let base = owned(&base_path, untrusted)?;
    crate::ensure(base.is_file() && base.len() <= MAX_BASE_BYTES, untrusted)?;
    crate::ensure(
        value(fields, "user_id")? == uid.to_string(),
        "session UID does not match property UID",
    )?;
    let base = fs::read_to_string(&base_path)?;
    let sensors = Path::new("/usr/bin/waydroid-sensord").exists();
    let properties = render(&base, fields, sensors)?;
    let untrusted = "untrusted Android work directory";
    let directory = owned(work, untrusted)?;
    crate::ensure(
        directory.is_dir() && directory.mode() & 0o022 == 0,
        untrusted,
    )?;
    Ok(files::replace(
        &work.join("deckard.prop"),
        properties.as_bytes(),
        0o644,
    )?)
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
            ("deckard_data".into(), "/home/user/data".into()),
            ("background_start".into(), "true".into()),
            ("lcd_density".into(), "254".into()),
        ]);
        let result = render(
            "ro.hardware.egl=mesa\nro.sf.lcd_density=200\n",
            &fields,
            false,
        )?;
        assert!(result.contains("ro.hardware.egl=mesa\n"));
        assert!(result.contains("deckard.host_data_path=/home/user/data\n"));
        assert_eq!(result.matches("ro.sf.lcd_density=").count(), 1);
        assert!(result.contains("deckard.stub_sensors_hal=1\n"));
        Ok(())
    }

    #[test]
    fn rejects_property_injection() {
        let fields = HashMap::from([
            ("user_id".into(), "1000".into()),
            ("group_id".into(), "1000".into()),
            ("user_name".into(), "user\nro.adb.secure=0".into()),
            ("deckard_data".into(), "/home/user/data".into()),
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
            ("deckard_data".into(), "/home/user/data".into()),
            ("background_start".into(), "true".into()),
            ("lcd_density".into(), "0".into()),
        ]);
        let result = render("ro.sf.lcd_density=254\n", &fields, false)?;
        assert_eq!(result.matches("ro.sf.lcd_density=254").count(), 1);
        Ok(())
    }
}
