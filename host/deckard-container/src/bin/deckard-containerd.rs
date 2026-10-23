use deckard_container::dbus::serve_system_bus;
use deckard_container::initializer::{LocalImageInit, NodeInventory};
use deckard_container::lxc::Lxc;
use deckard_container::manager::ContainerManager;
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if rustix::process::geteuid().as_raw() != 0 {
        return Err("deckard-containerd requires root".into());
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
    if args != ["--experimental-run"] {
        return Err("prototype: explicit --experimental-run required".into());
    }
    let work = PathBuf::from("/var/lib/deckard");
    let manager = ContainerManager {
        lxc: Lxc::new(work.join("lxc"), "deckard".into())?,
        mounts: MountLayout {
            images: work.join("images"),
            work,
            overlays: true,
        },
        network: Network::new("deckard0".into(), "/run/deckard-container".into())?,
    };
    async_io::block_on(async {
        let _connection = serve_system_bus(manager).await?;
        std::future::pending::<()>().await;
        Ok(())
    })
}
