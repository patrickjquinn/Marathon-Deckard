//! `waydroid.cfg`, the instance's INI file (upstream `tools/config`).

use crate::{Error, Result};
use rustix::process::geteuid;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1024 * 1024;

/// Keys upstream fills in when the file lacks them.
pub const DEFAULTS: [(&str, &str); 3] = [
    ("suspend_action", "freeze"),
    ("mount_overlays", "True"),
    ("auto_adb", "False"),
];

fn section(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

/// The value of `key` in `[waydroid]`.
pub fn get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_waydroid = false;
    for line in text.lines() {
        if let Some(name) = section(line) {
            in_waydroid = name.eq_ignore_ascii_case("waydroid");
        } else if in_waydroid
            && let Some((candidate, value)) = line.split_once('=')
            && candidate.trim().eq_ignore_ascii_case(key)
        {
            return Some(value.trim());
        }
    }
    None
}

/// Like [`get`], falling back to upstream's default.
pub fn get_or_default<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    get(text, key).or_else(|| {
        DEFAULTS
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(*value))
    })
}

/// `text` with `updates` written into `[waydroid]` (created if missing);
/// other lines and sections are kept.
///
/// # Errors
/// The text has control characters, a duplicate key or section, or a value
/// would break the line format.
pub fn set(text: &str, updates: &[(&str, &str)]) -> Result<String> {
    if text.contains(['\0', '\r']) {
        return Err(Error::Invalid("invalid waydroid.cfg".into()));
    }
    for (key, value) in updates {
        if key.is_empty() || value.contains(['\n', '\r', '\0']) || value.len() > 4096 {
            return Err(Error::Invalid(format!(
                "invalid waydroid.cfg value for {key}"
            )));
        }
    }
    let mut result = String::new();
    let mut seen = Vec::new();
    let mut in_waydroid = false;
    let mut found = false;
    let append_missing = |result: &mut String, seen: &[String]| {
        for (key, value) in updates {
            if !seen.iter().any(|name| name == key) {
                result.push_str(&format!("{key} = {value}\n"));
            }
        }
    };
    for line in text.lines() {
        if let Some(name) = section(line) {
            if in_waydroid {
                append_missing(&mut result, &seen);
            }
            in_waydroid = name.eq_ignore_ascii_case("waydroid");
            if in_waydroid && found {
                return Err(Error::Invalid("duplicate [waydroid] section".into()));
            }
            found |= in_waydroid;
        } else if in_waydroid && let Some((key, _)) = line.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            if let Some((name, value)) = updates.iter().find(|(name, _)| *name == key) {
                if seen.contains(&key) {
                    return Err(Error::Invalid(format!("duplicate config key: {key}")));
                }
                seen.push(key);
                result.push_str(&format!("{name} = {value}\n"));
                continue;
            }
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
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
        || metadata.len() > MAX_BYTES
    {
        return Err(Error::Invalid(format!(
            "untrusted file: {}",
            path.display()
        )));
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
/// The temporary file cannot be written or renamed.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("config path has no parent".into()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Invalid(error.to_string()))?
        .as_nanos();
    let temporary = parent.join(format!(".deckard-cfg-{}-{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
        assert_eq!(get(&result, "binder"), Some("binder"));
        assert_eq!(get(&result, "missing"), None);
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
