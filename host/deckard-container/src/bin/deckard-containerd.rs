use deckard_container::dbus::serve_system_bus;
use deckard_container::drivers::DriverNodes;
use deckard_container::hardware;
use deckard_container::initializer::{LocalImageInit, NodeInventory};
use deckard_container::lxc::Lxc;
use deckard_container::manager::ContainerManager;
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use deckard_container::{properties, session_config};
use deckard_launch::binder::{self, BinderConfig};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn audit_production() -> Result<(), Box<dyn Error + Send + Sync>> {
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
    DriverNodes::from_config(&work.join("waydroid.cfg"))?.audit(Path::new("/dev"))?;
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
    println!("production Binder nodes, session mounts, and Android properties match");
    Ok(())
}

fn production_setting(work: &Path, key: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let config = fs::read_to_string(work.join("waydroid.cfg"))?;
    let value = config
        .lines()
        .filter_map(|line| line.split_once('='))
        .find_map(|(name, value)| (name.trim() == key).then_some(value.trim()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {key}")))?;
    Ok(value.to_owned())
}

fn production_overlays(work: &Path) -> Result<bool, Box<dyn Error + Send + Sync>> {
    match production_setting(work, "mount_overlays")?.as_str() {
        "True" => Ok(true),
        "False" => Ok(false),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid mount_overlays").into()),
    }
}

fn audit_hardware() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = BinderConfig::from_file(Path::new("/var/lib/waydroid/waydroid.cfg"))?;
    if !binder::service_manager_present(&config)? {
        return Err("Binder service manager is not present".into());
    }
    println!("Binder service manager ABI and connection ready");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
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
    let hardware_config = if production {
        Some(BinderConfig::from_file(
            &manager.mounts.work.join("waydroid.cfg"),
        )?)
    } else {
        None
    };
    let suspend_stops =
        production && production_setting(&manager.mounts.work, "suspend_action")? == "stop";
    let (hardware_sender, hardware_receiver) = mpsc::sync_channel(hardware::QUEUE_DEPTH);
    let terminating = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminating.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, terminating.clone())?;
    async_io::block_on(async {
        let (_connection, service) =
            serve_system_bus(manager, hardware_config, hardware_sender, suspend_stops).await?;
        while !terminating.load(Ordering::Relaxed) {
            for _ in 0..8 {
                let Ok(request) = hardware_receiver.try_recv() else {
                    break;
                };
                if let Err(error) = service.handle_hardware_request(request).await {
                    eprintln!("deckard-containerd: hardware request failed: {error}");
                }
            }
            async_io::Timer::after(Duration::from_millis(100)).await;
        }
        service.shutdown().await?;
        Ok(())
    })
}
