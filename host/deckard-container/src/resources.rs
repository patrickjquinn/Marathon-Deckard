//! Host scheduling for the Android runtime. These are shares under contention,
//! not CPU or bandwidth caps; Android keeps its own per-thread priorities.

use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Profile {
    Foreground,
    #[default]
    Background,
    Normal,
}

impl Profile {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "foreground" => Some(Self::Foreground),
            "background" => Some(Self::Background),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }

    pub(crate) fn apply(self, group: &Path) -> io::Result<()> {
        // The stopped runtime has no payload cgroup. The manager retains the
        // profile and reapplies it after a start or hardware-driven reboot.
        if !group.is_dir() {
            return Ok(());
        }
        let (cpu, io) = match self {
            Self::Foreground => (200, 200),
            Self::Background => (50, 25),
            Self::Normal => (100, 100),
        };
        if group.join("cpu.weight").exists() {
            fs::write(group.join("cpu.weight"), cpu.to_string())?;
        }
        // BFQ has its own controller file. io.weight belongs to iocost and
        // is absent on the phone's BFQ-only kernel.
        let bfq = group.join("io.bfq.weight");
        if bfq.exists() {
            fs::write(bfq, format!("default {io}"))?;
        } else if group.join("io.weight").exists() {
            fs::write(group.join("io.weight"), format!("default {io}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_boost_and_background_restore_work_without_uclamp() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("cpu.weight"), "50")?;
        fs::write(dir.path().join("io.bfq.weight"), "default 25")?;
        Profile::Foreground.apply(dir.path())?;
        assert_eq!(fs::read_to_string(dir.path().join("cpu.weight"))?, "200");
        assert_eq!(
            fs::read_to_string(dir.path().join("io.bfq.weight"))?,
            "default 200"
        );
        Profile::Background.apply(dir.path())?;
        assert_eq!(fs::read_to_string(dir.path().join("cpu.weight"))?, "50");
        assert_eq!(
            fs::read_to_string(dir.path().join("io.bfq.weight"))?,
            "default 25"
        );
        assert!(!dir.path().join("cpu.uclamp.min").exists());
        Ok(())
    }

    #[test]
    fn normal_restores_default_shares_and_optional_io_is_not_created() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("cpu.weight"), "200")?;
        Profile::Normal.apply(dir.path())?;
        assert_eq!(fs::read_to_string(dir.path().join("cpu.weight"))?, "100");
        assert!(!dir.path().join("io.bfq.weight").exists());
        assert!(!dir.path().join("io.weight").exists());
        assert_eq!(Profile::parse("realtime"), None);
        Ok(())
    }
}
