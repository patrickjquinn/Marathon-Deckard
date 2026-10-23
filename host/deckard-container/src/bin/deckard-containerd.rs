use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use deckard_container::dbus::{self, Mode};
use deckard_container::drivers::DriverNodes;
use deckard_container::lxc::Lxc;
use deckard_container::manager::ContainerManager;
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use deckard_container::session_config::{self, Stage};
use deckard_container::{Result, hardware, properties};
use deckard_launch::binder::{self, BinderConfig};
use deckard_launch::session::ContainerManagerProxyBlocking;

const USAGE: &str = "usage: deckard-containerd --audit-production | --audit-hardware | --production-run | --experimental-run";

fn audit_production() -> Result<()> {
    let fields = ContainerManagerProxyBlocking::new(&zbus::blocking::Connection::system()?)?
        .get_session()?;
    let uid: u32 = fields.get("user_id").ok_or("no active session")?.parse()?;
    let work = Path::new("/var/lib/waydroid");
    DriverNodes::from_config(&work.join("waydroid.cfg"))?.audit(Path::new("/dev"))?;
    let pulse = session_config::check(&fields, uid, Path::new("/run/user"))?;
    let rendered = session_config::render(&Stage::new(Path::new("/run/waydroid-lxc")), pulse);
    if rendered != fs::read_to_string(work.join("lxc/waydroid/config_session"))? {
        return Err("session LXC config differs from the live config".into());
    }
    let base = fs::read_to_string(work.join("waydroid_base.prop"))?;
    let sensors = Path::new("/usr/bin/waydroid-sensord").exists();
    if properties::render(&base, &fields, sensors)?
        != fs::read_to_string(work.join("waydroid.prop"))?
    {
        return Err("Android properties differ from the live config".into());
    }
    println!("production Binder nodes, session mounts, and Android properties match");
    Ok(())
}

fn audit_hardware() -> Result<()> {
    let config = BinderConfig::from_file(Path::new("/var/lib/waydroid/waydroid.cfg"))?;
    if !binder::service_manager_present(&config)? {
        return Err("Binder service manager is not present".into());
    }
    println!("Binder service manager ABI and connection ready");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !rustix::process::geteuid().is_root() {
        return Err("deckard-containerd requires root".into());
    }
    // Production reads waydroid.cfg at each Start, so an uninitialized
    // instance still serves Init.
    let (work, name, bridge, runtime, mode) =
        match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["--audit-production"] => return audit_production(),
            ["--audit-hardware"] => return audit_hardware(),
            ["--production-run"] => (
                "/var/lib/waydroid",
                "waydroid",
                "waydroid0",
                "/run/waydroid-lxc",
                Mode::Production,
            ),
            ["--experimental-run"] => (
                "/var/lib/deckard",
                "deckard",
                "deckard0",
                "/run/deckard-container",
                Mode::Experimental,
            ),
            _ => return Err(USAGE.into()),
        };
    let work = PathBuf::from(work);
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
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&terminating))?;
    }
    async_io::block_on(async {
        let (_connection, service) = dbus::serve_system_bus(manager, mode, hardware_sender).await?;
        while !terminating.load(Ordering::Relaxed) {
            let batch = hardware_receiver.try_iter().take(8);
            for request in batch {
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
