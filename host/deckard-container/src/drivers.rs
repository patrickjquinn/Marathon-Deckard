//! Binder node preflight for the initialized Waydroid instance.

use crate::{Error, Result};
use std::fs::{self, File};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverNodes {
    pub binder: String,
    pub vndbinder: String,
    pub hwbinder: String,
}

impl DriverNodes {
    pub fn from_config(path: &Path) -> Result<Self> {
        let config = fs::read_to_string(path)?;
        Self::parse(&config)
    }

    fn parse(config: &str) -> Result<Self> {
        let setting = |name: &str| {
            config
                .lines()
                .filter_map(|line| line.split_once('='))
                .find_map(|(key, value)| (key.trim() == name).then_some(value.trim().to_owned()))
                .ok_or_else(|| Error::Invalid(format!("missing {name} in waydroid.cfg")))
        };
        let nodes = Self {
            binder: setting("binder")?,
            vndbinder: setting("vndbinder")?,
            hwbinder: setting("hwbinder")?,
        };
        for name in [&nodes.binder, &nodes.vndbinder, &nodes.hwbinder] {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(Error::Invalid("invalid Binder node name".into()));
            }
        }
        Ok(nodes)
    }

    fn open_devices(&self, dev_root: &Path) -> Result<Vec<File>> {
        let mut devices = Vec::with_capacity(3);
        for name in [&self.binder, &self.vndbinder, &self.hwbinder] {
            let path = dev_root.join(name);
            let resolved = fs::canonicalize(&path)?;
            if !resolved.starts_with(dev_root) {
                return Err(Error::Invalid(format!(
                    "Binder node is not a device under {}: {}",
                    dev_root.display(),
                    path.display()
                )));
            }
            let device = File::open(&resolved)?;
            if !device.metadata()?.file_type().is_char_device() {
                return Err(Error::Invalid(format!(
                    "Binder node is not a character device: {}",
                    path.display()
                )));
            }
            devices.push(device);
        }
        Ok(devices)
    }

    pub fn audit(&self, dev_root: &Path) -> Result<()> {
        self.open_devices(dev_root).map(|_| ())
    }

    pub fn prepare(&self, dev_root: &Path) -> Result<()> {
        let devices = self.open_devices(dev_root)?;
        for device in devices {
            device.set_permissions(fs::Permissions::from_mode(0o666))?;
        }
        Ok(())
    }

    pub fn paths(&self, dev_root: &Path) -> [PathBuf; 3] {
        [
            dev_root.join(&self.binder),
            dev_root.join(&self.vndbinder),
            dev_root.join(&self.hwbinder),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installed_binder_names() -> Result<()> {
        let nodes = DriverNodes::parse(
            "[waydroid]\nbinder = binder\nvndbinder = vndbinder\nhwbinder = hwbinder\n",
        )?;
        assert_eq!(nodes.paths(Path::new("/dev"))[0], Path::new("/dev/binder"));
        Ok(())
    }

    #[test]
    fn rejects_escaping_and_missing_names() {
        assert!(
            DriverNodes::parse("binder = ../null\nvndbinder = vndbinder\nhwbinder = hwbinder")
                .is_err()
        );
        assert!(DriverNodes::parse("binder = binder\nvndbinder = vndbinder").is_err());
    }
}
