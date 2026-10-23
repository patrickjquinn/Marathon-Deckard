//! Writing `waydroid.cfg`; reading is `deckard_host_tools::cfg`.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use deckard_host_tools::{cfg, files};

use crate::Result;

const MAX_BYTES: u64 = 1024 * 1024;

/// Keys upstream fills in when the file lacks them.
pub const DEFAULTS: [(&str, &str); 3] = [
    ("suspend_action", "freeze"),
    ("mount_overlays", "True"),
    ("auto_adb", "False"),
];

/// Like [`cfg::get`], falling back to upstream's default.
#[must_use]
pub fn get_or_default<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let default = DEFAULTS.iter().find(|(name, _)| *name == key);
    cfg::get(text, key).or(default.map(|(_, value)| *value))
}

/// `text` with `updates` written into `[waydroid]` (created if missing);
/// other lines and sections are kept.
///
/// # Errors
/// The text has control characters, a duplicate key or section, or a value
/// would break the line format.
pub fn set(text: &str, updates: &[(&str, &str)]) -> Result<String> {
    if text.contains(['\0', '\r']) {
        return Err("invalid waydroid.cfg".into());
    }
    for (key, value) in updates {
        if key.is_empty() || value.contains(['\n', '\r', '\0']) || value.len() > 4096 {
            return Err(format!("invalid waydroid.cfg value for {key}").into());
        }
    }
    let mut result = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut in_waydroid = false;
    let mut found = false;
    let append_missing = |result: &mut String, seen: &[String]| {
        for (key, value) in updates {
            if !seen.iter().any(|name| name == key) {
                let _ = writeln!(result, "{key} = {value}");
            }
        }
    };
    for line in text.lines() {
        if let Some(name) = cfg::section(line) {
            if in_waydroid {
                append_missing(&mut result, &seen);
            }
            in_waydroid = name.eq_ignore_ascii_case("waydroid");
            if in_waydroid && found {
                return Err("duplicate [waydroid] section".into());
            }
            found |= in_waydroid;
        } else if in_waydroid
            && let Some((key, _)) = line.split_once('=')
            && let key = key.trim().to_ascii_lowercase()
            && let Some((name, value)) = updates.iter().find(|(name, _)| *name == key)
        {
            if seen.contains(&key) {
                return Err(format!("duplicate config key: {key}").into());
            }
            seen.push(key);
            let _ = writeln!(result, "{name} = {value}");
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    if in_waydroid {
        append_missing(&mut result, &seen);
    } else if !found {
        result.push_str("[waydroid]\n");
        append_missing(&mut result, &[]);
    }
    Ok(result)
}

/// Reads a root-owned config of at most 1 MiB; a missing file reads as empty.
///
/// # Errors
/// The file is a link, not owned by the daemon, writable by others or too big.
pub fn read(path: &Path) -> Result<String> {
    let Some(metadata) = files::found(fs::symlink_metadata(path))? else {
        return Ok(String::new());
    };
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() > MAX_BYTES
    {
        return Err(format!("untrusted file: {}", path.display()).into());
    }
    let mut text = String::new();
    File::open(path)?
        .take(MAX_BYTES)
        .read_to_string(&mut text)?;
    Ok(text)
}

/// Replaces `path` atomically with `text`, mode 0644.
///
/// # Errors
/// The file cannot be written.
pub fn write(path: &Path, text: &str) -> Result<()> {
    Ok(files::replace(path, text.as_bytes(), 0o644)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_updates_appends_and_keeps_other_sections() -> Result<()> {
        let text = "[waydroid]\nArch = old\nmount_overlays = False\n[properties]\narch = keep\n";
        let result = set(text, &[("arch", "arm64"), ("binder", "binder")])?;
        assert_eq!(
            result,
            "[waydroid]\narch = arm64\nmount_overlays = False\nbinder = binder\n[properties]\narch = keep\n"
        );
        assert_eq!(cfg::get(&result, "binder"), Some("binder"));
        assert_eq!(cfg::get(&result, "missing"), None);
        assert_eq!(get_or_default(&result, "suspend_action"), Some("freeze"));
        assert_eq!(get_or_default(&result, "mount_overlays"), Some("False"));
        assert_eq!(
            set("[properties]\nro.x = 1\n", &[("arch", "arm64")])?,
            "[properties]\nro.x = 1\n[waydroid]\narch = arm64\n"
        );
        Ok(())
    }

    #[test]
    fn set_rejects_duplicates_and_injection() {
        assert!(set("[waydroid]\narch = a\narch = b\n", &[("arch", "c")]).is_err());
        assert!(set("[waydroid]\n[waydroid]\n", &[("arch", "c")]).is_err());
        assert!(set("", &[("arch", "c\nbinder = ../mem")]).is_err());
    }
}
