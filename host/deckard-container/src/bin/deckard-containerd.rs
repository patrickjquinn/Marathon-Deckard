use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use deckard_container::dbus;
use deckard_container::drivers::DriverNodes;
use deckard_container::lxc::{ContainerManager, Lxc};
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use deckard_container::session_config::{self, Stage};
use deckard_container::{Result, hardware, properties};
use deckard_launch::binder::{self, BinderConfig};
use deckard_launch::session::ContainerManagerProxyBlocking;

const USAGE: &str =
    "usage: deckard-containerd --audit-production | --audit-hardware | --production-run";

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
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["--audit-production"] => return audit_production(),
        ["--audit-hardware"] => return audit_hardware(),
        // waydroid.cfg is read at each Start, so an uninitialized instance
        // still serves Init.
        ["--production-run"] => {}
        _ => return Err(USAGE.into()),
    }
    let work = PathBuf::from("/var/lib/waydroid");
    let manager = ContainerManager {
        lxc: Lxc::new(work.join("lxc"), "waydroid".into())?,
        mounts: MountLayout {
            images: work.join("images"),
            work,
            overlays: true,
        },
        network: Network::new("waydroid0".into(), "/run/waydroid-lxc".into())?,
    };
    let (hardware_sender, hardware_receiver) = mpsc::sync_channel(hardware::QUEUE_DEPTH);
    let terminating = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&terminating))?;
    }
    async_io::block_on(async {
        let (_connection, service) = dbus::serve_system_bus(manager, hardware_sender).await?;
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
