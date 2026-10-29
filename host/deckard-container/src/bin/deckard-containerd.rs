use std::fs;
use std::path::{Path, PathBuf};

use async_signal::{Signal, Signals};
use deckard_container::dbus;
use deckard_container::drivers::DriverNodes;
use deckard_container::lxc::{ContainerManager, Lxc};
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use deckard_container::session_config::{self, Stage};
use deckard_container::{Result, hardware, properties};
use deckard_host_tools::names;
use deckard_launch::binder::{self, BinderConfig};
use deckard_launch::session::ContainerManagerProxy;
use futures_lite::{StreamExt, future};

const USAGE: &str =
    "usage: deckard-containerd --audit-production | --audit-hardware | --production-run";

fn audit_production() -> Result<()> {
    let fields = zbus::block_on(
        zbus::block_on(ContainerManagerProxy::new(&zbus::block_on(
            zbus::Connection::system(),
        )?))?
        .get_session(),
    )?;
    let uid: u32 = fields.get("user_id").ok_or("no active session")?.parse()?;
    let work = Path::new(names::WORK);
    DriverNodes::from_config(&work.join("deckard.cfg"))?.audit(Path::new("/dev"))?;
    let pulse = session_config::check(&fields, uid, Path::new("/run/user"))?;
    let rendered = session_config::render(&Stage::new(Path::new(names::RUN)), pulse);
    if rendered
        != fs::read_to_string(
            work.join("lxc")
                .join(names::CONTAINER)
                .join("config_session"),
        )?
    {
        return Err("session LXC config differs from the live config".into());
    }
    let base = fs::read_to_string(work.join("deckard_base.prop"))?;
    let sensors = Path::new("/usr/bin/waydroid-sensord").exists();
    if properties::render(&base, &fields, sensors)?
        != fs::read_to_string(work.join("deckard.prop"))?
    {
        return Err("Android properties differ from the live config".into());
    }
    println!("production Binder nodes, session mounts, and Android properties match");
    Ok(())
}

fn audit_hardware() -> Result<()> {
    let config = BinderConfig::from_file(Path::new(names::CONFIG))?;
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
        // deckard.cfg is read at each Start, so an uninitialized instance
        // still serves Init.
        ["--production-run"] => {}
        _ => return Err(USAGE.into()),
    }
    let work = PathBuf::from(names::WORK);
    let manager = ContainerManager {
        lxc: Lxc::new(work.join("lxc"), names::CONTAINER.into())?,
        mounts: MountLayout {
            images: work.join("images"),
            work,
            overlays: true,
        },
        network: Network::new(names::BRIDGE.into(), names::RUN.into())?,
    };
    let (hardware_sender, hardware_receiver) = async_channel::bounded(hardware::QUEUE_DEPTH);
    let mut terminating = Signals::new([Signal::Term, Signal::Int])?;
    async_io::block_on(async {
        let (_connection, service) = dbus::serve_system_bus(manager, hardware_sender).await?;
        let requests = async {
            while let Ok(request) = hardware_receiver.recv().await {
                if let Err(error) = service.handle_hardware_request(request).await {
                    eprintln!("deckard-containerd: hardware request failed: {error}");
                }
            }
        };
        future::or(requests, async { drop(terminating.next().await) }).await;
        service.shutdown().await?;
        Ok(())
    })
}
