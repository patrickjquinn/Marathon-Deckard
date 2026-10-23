use deckard_container::dbus::serve_system_bus;
use deckard_container::hardware::{self, HardwareConfig};
use deckard_container::initializer::{LocalImageInit, NodeInventory};
use deckard_container::lxc::Lxc;
use deckard_container::manager::ContainerManager;
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use deckard_container::{properties, session_config};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn audit_production() -> Result<(), Box<dyn Error>> {
    let bus = zbus::blocking::Connection::system()?;
    let manager = zbus::blocking::Proxy::new(
        &bus,
        "id.waydro.Container",
        "/ContainerManager",
        "id.waydro.ContainerManager",
    )?;
    let fields: HashMap<String, String> = manager.call("GetSession", &())?;
    let uid: u32 = fields
        .get("user_id")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no active session"))?
        .parse()?;
    let work = Path::new("/var/lib/waydroid");
    let rendered = session_config::render(&fields, uid, Path::new("/run/user"))?;
    let installed = fs::read_to_string(work.join("lxc/waydroid/config_session"))?;
    if rendered != installed {
        return Err("session LXC config differs from the live config".into());
    }
    let base = fs::read_to_string(work.join("waydroid_base.prop"))?;
    let rendered = properties::render(
        &base,
        &fields,
        Path::new("/usr/bin/waydroid-sensord").exists(),
    )?;
    let installed = fs::read_to_string(work.join("waydroid.prop"))?;
    if rendered != installed {
        return Err("Android properties differ from the live config".into());
    }
    println!("production session mounts and Android properties match");
    Ok(())
}

fn production_setting(work: &Path, key: &str) -> Result<String, Box<dyn Error>> {
    let config = fs::read_to_string(work.join("waydroid.cfg"))?;
    let value = config
        .lines()
        .filter_map(|line| line.split_once('='))
        .find_map(|(name, value)| (name.trim() == key).then_some(value.trim()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {key}")))?;
    Ok(value.to_owned())
}

fn production_overlays(work: &Path) -> Result<bool, Box<dyn Error>> {
    match production_setting(work, "mount_overlays")?.as_str() {
        "True" => Ok(true),
        "False" => Ok(false),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid mount_overlays").into()),
    }
}

fn audit_hardware() -> Result<(), Box<dyn Error>> {
    let work = Path::new("/var/lib/waydroid");
    let config = HardwareConfig {
        binder_device: format!("/dev/{}", production_setting(work, "binder")?),
        service_manager_protocol: Some(production_setting(work, "service_manager_protocol")?),
        binder_protocol: Some(production_setting(work, "binder_protocol")?),
    };
    if !hardware::probe(&config)? {
        return Err("Binder service manager is not present".into());
    }
    println!("Binder service manager ABI and connection ready");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if rustix::process::geteuid().as_raw() != 0 {
        return Err("deckard-containerd requires root".into());
    }
    if args == ["--audit-production"] {
        return audit_production();
    }
    if args == ["--audit-hardware"] {
        return audit_hardware();
    }
    if args.first().map(String::as_str) == Some("--experimental-prepare-local") {
        if args.len() != 11 {
            return Err("usage: deckard-containerd --experimental-prepare-local WORK IMAGES TEMPLATES NAME BRIDGE ARCH LXC_MAJOR BINDER VNDBINDER HWBINDER".into());
        }
        let init = LocalImageInit {
            work: PathBuf::from(&args[1]),
            images: PathBuf::from(&args[2]),
            templates: PathBuf::from(&args[3]),
            name: args[4].clone(),
            bridge: args[5].clone(),
            arch: args[6].clone(),
            lxc_major: args[7].parse()?,
            binder: args[8].clone(),
            vndbinder: args[9].clone(),
            hwbinder: args[10].clone(),
        };
        let output = init.prepare_local(&NodeInventory::from_host()?)?;
        println!("{}", output.display());
        return Ok(());
    }
    let production = args == ["--production-run"];
    if !production && args != ["--experimental-run"] {
        return Err("usage: deckard-containerd --audit-production | --audit-hardware | --production-run | --experimental-run | --experimental-prepare-local ...".into());
    }
    let work = PathBuf::from(if production {
        "/var/lib/waydroid"
    } else {
        "/var/lib/deckard"
    });
    let (name, bridge, runtime, overlays) = if production {
        let config = work.join("lxc/waydroid/config");
        if !config.is_file() || !work.join("waydroid_base.prop").is_file() {
            return Err("production Waydroid instance is not initialized".into());
        }
        (
            "waydroid",
            "waydroid0",
            "/run/waydroid-lxc",
            production_overlays(&work)?,
        )
    } else {
        ("deckard", "deckard0", "/run/deckard-container", true)
    };
    let manager = ContainerManager {
        lxc: Lxc::new(work.join("lxc"), name.into())?,
        mounts: MountLayout {
            images: work.join("images"),
            work,
            overlays,
        },
        network: Network::new(bridge.into(), runtime.into())?,
    };
    let terminating = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminating.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, terminating.clone())?;
    async_io::block_on(async {
        let (_connection, service) = serve_system_bus(manager).await?;
        while !terminating.load(Ordering::Relaxed) {
            async_io::Timer::after(Duration::from_millis(100)).await;
        }
        service.shutdown().await?;
        Ok(())
    })
}
