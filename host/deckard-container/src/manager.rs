use crate::command::Runner;
use crate::lxc::{Lxc, State};
use crate::mounts::MountLayout;
use crate::network::Network;
use crate::{Error, Result};

/// Root-only lifecycle coordinator. It does not expose a D-Bus API or touch systemd.
pub struct ContainerManager {
    pub lxc: Lxc,
    pub mounts: MountLayout,
    pub network: Network,
}

impl ContainerManager {
    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.lxc.status(runner)? != State::Stopped {
            return Err(Error::Invalid(
                "container must be stopped before mounting".into(),
            ));
        }
        self.mounts.preflight()?;
        self.network.start(runner)?;
        if let Err(error) = self.mounts.mount(runner) {
            let _ = self.network.stop(runner);
            return Err(error);
        }
        if let Err(error) = self.lxc.start(runner) {
            // An LXC launch can fail after spawning; do not unmount a live container.
            if self.lxc.status(runner)? == State::Stopped {
                let _ = self.mounts.unmount(runner);
                let _ = self.network.stop(runner);
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        self.lxc.stop(runner)?;
        if self.lxc.status(runner)? != State::Stopped {
            return Err(Error::Invalid(
                "refusing to unmount a live container".into(),
            ));
        }
        self.mounts.unmount(runner)?;
        self.network.stop(runner)
    }
}
