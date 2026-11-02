//! The container's device cgroup: Android may open and create only the
//! nodes of Deckard's node list for the host as it is at start
//! ([`initializer::node_entries`], which also renders `config_nodes`), the
//! pseudo-devices every Linux system has, and the loop devices the host has
//! not attached, which apexd takes for APEXes. Host disks, device-mapper, the
//! modem and every other node stay closed to it, even to its root.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use rustix::fs::{major, minor};

use crate::Result;
use crate::initializer::{self, Host};

/// null, zero, full, random, urandom, kmsg, tty, ptmx, the pty slaves and
/// loop-control.
const PSEUDO: [&str; 10] = [
    "c 1:3", "c 1:5", "c 1:7", "c 1:8", "c 1:9", "c 1:11", "c 5:0", "c 5:2", "c 136:*", "c 10:237",
];
const LOOP_MAJOR: u32 = 7;
const LOOP_MINORS: u32 = 256;

/// Writes the instance's `config_devices` for the host as it is now and
/// returns its path.
///
/// # Errors
/// The host cannot be probed, `/sys/block` cannot be read, or the file
/// cannot be written.
pub fn write(instance: &Path) -> Result<PathBuf> {
    let work = instance
        .parent()
        .and_then(Path::parent)
        .ok_or("LXC instance outside a work directory")?;
    let entries = initializer::node_entries(&Host::probe()?, work);
    let bound = device_nodes(&entries, host_node);
    let path = instance.join("config_devices");
    fs::write(
        &path,
        render(&bound, &attached_loops(Path::new("/sys/block"))?),
    )?;
    Ok(path)
}

/// The type and number of each device node among mount `entries`, as
/// `stat` finds their sources.
fn device_nodes(
    entries: &[String],
    stat: impl Fn(&str) -> Option<(char, u32, u32)>,
) -> Vec<(char, u32, u32)> {
    entries
        .iter()
        .filter_map(|entry| entry.split(' ').next())
        .filter(|source| source.starts_with("/dev/"))
        .filter_map(stat)
        .collect()
}

/// A host device node's type and number.
fn host_node(path: &str) -> Option<(char, u32, u32)> {
    let metadata = fs::metadata(path).ok()?;
    let kind = match metadata.file_type() {
        kind if kind.is_char_device() => 'c',
        kind if kind.is_block_device() => 'b',
        _ => return None,
    };
    Some((kind, major(metadata.rdev()), minor(metadata.rdev())))
}

/// The minors of the loop devices bound to a file.
fn attached_loops(sys_block: &Path) -> Result<BTreeSet<u32>> {
    let mut minors = BTreeSet::new();
    for entry in fs::read_dir(sys_block)? {
        let path = entry?.path();
        if !path.join("loop/backing_file").exists() {
            continue;
        }
        let dev = fs::read_to_string(path.join("dev"))?;
        if let Some(("7", minor)) = dev.trim().split_once(':') {
            minors.insert(minor.parse()?);
        }
    }
    Ok(minors)
}

fn render(bound: &[(char, u32, u32)], attached: &BTreeSet<u32>) -> String {
    let mut text = String::from("lxc.cgroup2.devices.deny = a\n");
    let mut allow = |rule: &str| {
        let _ = writeln!(text, "lxc.cgroup2.devices.allow = {rule} rwm");
    };
    for rule in PSEUDO {
        allow(rule);
    }
    for (kind, major, minor) in bound {
        allow(&format!("{kind} {major}:{minor}"));
    }
    for minor in (0..LOOP_MINORS).filter(|minor| !attached.contains(minor)) {
        allow(&format!("b {LOOP_MAJOR}:{minor}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::drivers::DriverNodes;

    #[test]
    fn every_node_the_initializer_binds_is_allowed() {
        let devices = [
            "/dev/zero",
            "/dev/fuse",
            "/dev/uhid",
            "/dev/net/tun",
            "/dev/video0",
            "/dev/dma_heap/system",
            "/dev/sw_sync",
        ];
        let host = Host {
            arch: "arm64",
            lxc_arch: "aarch64",
            lxc_major: 7,
            nodes: DriverNodes {
                binder: "binder".into(),
                vndbinder: "vndbinder".into(),
                hwbinder: "hwbinder".into(),
            },
            devices: devices.iter().map(|path| (*path).to_owned()).collect(),
            render_node: Some("/dev/dri/renderD128".into()),
            vulkan: None,
            products: Vec::new(),
            ashmem: false,
        };
        let entries = initializer::node_entries(&host, Path::new("/var/lib/deckard"));
        let sources: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.split(' ').next())
            .filter(|source| source.starts_with("/dev/") && *source != "/dev/char")
            .collect();
        let numbers: HashMap<&str, u32> = sources.iter().zip(1..).map(|(s, n)| (*s, n)).collect();
        let nodes = device_nodes(&entries, |path| {
            let minor = match path {
                "/dev/fuse" => 229,
                _ => *numbers.get(path)?,
            };
            Some(('c', 10, minor))
        });
        assert_eq!(nodes.len(), sources.len(), "{sources:?}");
        let rules = render(&nodes, &BTreeSet::new());
        for (source, (_, _, minor)) in sources.iter().zip(&nodes) {
            let rule = format!("allow = c 10:{minor} rwm\n");
            assert!(rules.contains(&rule), "{source} has no rule");
        }
        assert!(rules.contains("allow = c 10:229 rwm\n"), "/dev/fuse");
    }
}
