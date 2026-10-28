//! One LXC container, driven through the `lxc-*` tools, and its lifecycle
//! with the rootfs mounts and the network.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::Result;
use crate::command::{self, Command};
use crate::mounts::MountLayout;
use crate::network::Network;

#[derive(Clone, Debug)]
pub struct Lxc {
    pub root: PathBuf,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Stopped,
    Running,
    Frozen,
}

impl Lxc {
    /// # Errors
    /// The root is relative or the name is not a plain identifier.
    pub fn new(root: PathBuf, name: String) -> Result<Self> {
        let plain = name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !root.is_absolute() || name.is_empty() || !plain {
            return Err("invalid LXC root or container name".into());
        }
        Ok(Self { root, name })
    }

    fn command(&self, program: &str, tail: &[&str]) -> Command {
        let root = self.root.display().to_string();
        let head = [program, "-P", &root, "-n", &self.name];
        Command::new(head.iter().chain(tail).copied())
    }

    /// Runs `program` in the container, thawing a frozen one around it.
    fn attach(&self, program: &str, args: &[&str]) -> Result<String> {
        let tail = [&["--clear-env", "--", program], args].concat();
        let command = self
            .command("lxc-attach", &tail)
            .timeout(Duration::from_secs(3));
        let state = self.status()?;
        if state == State::Stopped {
            return Err("container is stopped".into());
        }
        if state == State::Running {
            return command::run(command);
        }
        self.unfreeze()?;
        match (command::run(command), self.freeze()) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(operation), Err(refreeze)) => Err(format!(
                "property operation failed: {operation}; refreeze failed: {refreeze}"
            )
            .into()),
        }
    }

    /// Android's value of `key`, thawing a frozen container around the read.
    ///
    /// # Errors
    /// The key is invalid, the container is stopped or a command fails.
    pub fn get_property(&self, key: &str) -> Result<String> {
        validate_property_key(key)?;
        let value = self.attach("/system/bin/getprop", &[key])?;
        Ok(value.trim_end_matches(['\r', '\n']).to_owned())
    }

    /// # Errors
    /// The key or value is invalid, the container is stopped or a command fails.
    pub fn set_property(&self, key: &str, value: &str) -> Result<()> {
        validate_property_key(key)?;
        validate_property_value(value)?;
        self.attach("/system/bin/setprop", &[key, value]).map(drop)
    }

    /// # Errors
    /// `lxc-info` fails or reports an unknown state.
    pub fn status(&self) -> Result<State> {
        match command::run(self.command("lxc-info", &["-sH"]))?.trim() {
            "STOPPED" => Ok(State::Stopped),
            "RUNNING" => Ok(State::Running),
            "FROZEN" => Ok(State::Frozen),
            other => Err(format!("unrecognized LXC state: {other}").into()),
        }
    }

    /// # Errors
    /// A status check fails or `target` is not reached in time.
    pub fn wait_for(&self, target: State, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.status()? != target {
            if Instant::now() >= deadline {
                return Err(format!("LXC did not reach {target:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// From `from`, runs `program` and waits up to 10 seconds for `to`.
    fn change(&self, from: State, program: &str, to: State) -> Result<()> {
        if self.status()? != from {
            let from = format!("{from:?}").to_lowercase();
            return Err(format!("container is not {from}").into());
        }
        command::run(self.command(program, &[]))?;
        self.wait_for(to, Duration::from_secs(10))
    }

    /// # Errors
    /// The container is not stopped, its config is not a regular file, or
    /// it does not start within 10 seconds.
    pub fn start(&self) -> Result<()> {
        if self.status()? != State::Stopped {
            return Err("container is not stopped".into());
        }
        let config = self.root.join(&self.name).join("config");
        if !fs::symlink_metadata(&config).is_ok_and(|metadata| metadata.is_file()) {
            return Err(format!("LXC config is not a regular file: {}", config.display()).into());
        }
        command::run(self.command("lxc-start", &["-d", "--", "/init"]))?;
        self.wait_for(State::Running, Duration::from_secs(10))
    }

    /// # Errors
    /// The container does not stop within 30 seconds.
    pub fn stop(&self) -> Result<()> {
        if self.status()? == State::Stopped {
            return Ok(());
        }
        command::run(self.command("lxc-stop", &["-k"]))?;
        self.wait_for(State::Stopped, Duration::from_secs(30))
    }

    /// # Errors
    /// The container is not running or does not freeze within 10 seconds.
    pub fn freeze(&self) -> Result<()> {
        self.change(State::Running, "lxc-freeze", State::Frozen)
    }

    /// # Errors
    /// The container is not frozen or does not thaw within 10 seconds.
    pub fn unfreeze(&self) -> Result<()> {
        self.change(State::Frozen, "lxc-unfreeze", State::Running)
    }
}

/// The container with its mounts and network; root-only, no systemd.
pub struct ContainerManager {
    pub lxc: Lxc,
    pub mounts: MountLayout,
    pub network: Network,
}

impl ContainerManager {
    /// Network, mounts, then LXC; undone in reverse on failure.
    ///
    /// # Errors
    /// The container is not stopped or a step fails.
    pub fn start(&self) -> Result<()> {
        if self.lxc.status()? != State::Stopped {
            return Err("container must be stopped before mounting".into());
        }
        self.mounts.preflight()?;
        self.network.start()?;
        if let Err(error) = self.mounts.mount() {
            let _ = self.network.stop();
            return Err(error);
        }
        if let Err(error) = self.lxc.start() {
            // An LXC launch can fail after spawning; do not unmount a live container.
            if self.lxc.status()? == State::Stopped {
                let _ = self.mounts.unmount();
                let _ = self.network.stop();
            }
            return Err(error);
        }
        Ok(())
    }
}

/// # Errors
/// `key` is not a plain dotted Android property name of at most 128 bytes.
pub fn validate_property_key(key: &str) -> Result<()> {
    let plain = key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if key.is_empty()
        || key.len() > 128
        || key.starts_with(['.', '-'])
        || key.ends_with('.')
        || key.contains("..")
        || !plain
    {
        return Err("invalid Android property key".into());
    }
    Ok(())
}

/// # Errors
/// `value` is over 1024 bytes or holds a NUL or line break.
pub fn validate_property_value(value: &str) -> Result<()> {
    if value.len() > 1024 || value.contains(['\0', '\n', '\r']) {
        return Err("invalid property value".into());
    }
    Ok(())
}
