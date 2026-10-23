use deckard_container::dbus::{Mode, serve_system_bus};
use deckard_container::drivers::DriverNodes;
use deckard_container::hardware;
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
use std::path::{Path, PathBuf};
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
    let production = args == ["--production-run"];
    if !production && args != ["--experimental-run"] {
        return Err("usage: deckard-containerd --audit-production | --audit-hardware | --production-run | --experimental-run".into());
    }
    let work = PathBuf::from(if production {
        "/var/lib/waydroid"
    } else {
        "/var/lib/deckard"
    });
    // Production reads waydroid.cfg at each Start, so an uninitialized
    // instance still serves Init.
    let (name, bridge, runtime, mode) = if production {
        (
            "waydroid",
            "waydroid0",
            "/run/waydroid-lxc",
            Mode::Production,
        )
    } else {
        (
            "deckard",
            "deckard0",
            "/run/deckard-container",
            Mode::Experimental,
        )
    };
    let manager = ContainerManager {
        lxc: Lxc::new(work.join("lxc"), name.into())?,
        mounts: MountLayout {
            images: work.join("images"),
            work,
            overlays: true,
        },
        network: Network::new(bridge.into(), runtime.into())?,
    };
    let (hardware_sender, hardware_receiver) = mpsc::sync_channel(hardware::QUEUE_DEPTH);
    let terminating = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, terminating.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, terminating.clone())?;
    async_io::block_on(async {
        let (_connection, service) = serve_system_bus(manager, mode, hardware_sender).await?;
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
