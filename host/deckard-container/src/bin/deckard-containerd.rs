use deckard_container::dbus::serve_system_bus;
use deckard_container::lxc::Lxc;
use deckard_container::manager::ContainerManager;
use deckard_container::mounts::MountLayout;
use deckard_container::network::Network;
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    if !std::env::args()
        .skip(1)
        .eq(["--experimental-run"].map(str::to_owned))
    {
        return Err("prototype: explicit --experimental-run required".into());
    }
    if rustix::process::geteuid().as_raw() != 0 {
        return Err("deckard-containerd requires root".into());
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
