//! One LXC container, driven through the `lxc-*` tools, and its lifecycle
//! with the rootfs mounts and the network.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::Result;
use crate::command::{Command, Runner};
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

    fn property(&self, program: &str, args: &[&str]) -> Command {
        let mut command = self.command("lxc-attach", &["--clear-env", "--", program]);
        command
            .args
            .extend(args.iter().map(|arg| (*arg).to_owned()));
        command.timeout(Duration::from_secs(3))
    }

    /// Android's value of `key`, thawing a frozen container around the read.
    ///
    /// # Errors
    /// The key is invalid, the container is stopped or a command fails.
    pub fn get_property(&self, runner: &mut impl Runner, key: &str) -> Result<String> {
        validate_property_key(key)?;
        let command = self.property("/system/bin/getprop", &[key]);
        let value = self.with_running(runner, |runner| runner.run(command))?;
        Ok(value.trim_end_matches(['\r', '\n']).to_owned())
    }

    /// # Errors
    /// The key or value is invalid, the container is stopped or a command fails.
    pub fn set_property(&self, runner: &mut impl Runner, key: &str, value: &str) -> Result<()> {
        validate_property_key(key)?;
        validate_property_value(value)?;
        let command = self.property("/system/bin/setprop", &[key, value]);
        self.with_running(runner, |runner| runner.run(command))
            .map(drop)
    }

    fn with_running<R: Runner, T>(
        &self,
        runner: &mut R,
        action: impl FnOnce(&mut R) -> Result<T>,
    ) -> Result<T> {
        let state = self.status(runner)?;
        if state == State::Stopped {
            return Err("container is stopped".into());
        }
        if state == State::Running {
            return action(runner);
        }
        self.unfreeze(runner)?;
        match (action(runner), self.freeze(runner)) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(operation), Err(refreeze)) => Err(format!(
                "property operation failed: {operation}; refreeze failed: {refreeze}"
            )
            .into()),
        }
    }

    /// # Errors
    /// `lxc-info` fails or reports an unknown state.
    pub fn status(&self, runner: &mut impl Runner) -> Result<State> {
        match runner.run(self.command("lxc-info", &["-sH"]))?.trim() {
            "STOPPED" => Ok(State::Stopped),
            "RUNNING" => Ok(State::Running),
            "FROZEN" => Ok(State::Frozen),
            other => Err(format!("unrecognized LXC state: {other}").into()),
        }
    }

    /// # Errors
    /// A status check fails or `target` is not reached in time.
    pub fn wait_for(
        &self,
        runner: &mut impl Runner,
        target: State,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.status(runner)? != target {
            if Instant::now() >= deadline {
                return Err(format!("LXC did not reach {target:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// From `from`, runs `program` and waits up to 10 seconds for `to`.
    fn change(
        &self,
        runner: &mut impl Runner,
        from: State,
        program: &str,
        to: State,
    ) -> Result<()> {
        if self.status(runner)? != from {
            let from = format!("{from:?}").to_lowercase();
            return Err(format!("container is not {from}").into());
        }
        runner.run(self.command(program, &[]))?;
        self.wait_for(runner, to, Duration::from_secs(10))
    }

    /// # Errors
    /// The container is not stopped, its config is not a regular file, or
    /// it does not start within 10 seconds.
    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? != State::Stopped {
            return Err("container is not stopped".into());
        }
        let config = self.root.join(&self.name).join("config");
        if !fs::symlink_metadata(&config).is_ok_and(|metadata| metadata.is_file()) {
            return Err(format!("LXC config is not a regular file: {}", config.display()).into());
        }
        runner.run(self.command("lxc-start", &["-d", "--", "/init"]))?;
        self.wait_for(runner, State::Running, Duration::from_secs(10))
    }

    /// # Errors
    /// The container does not stop within 30 seconds.
    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? == State::Stopped {
            return Ok(());
        }
        runner.run(self.command("lxc-stop", &["-k"]))?;
        self.wait_for(runner, State::Stopped, Duration::from_secs(30))
    }

    /// # Errors
    /// The container is not running or does not freeze within 10 seconds.
    pub fn freeze(&self, runner: &mut impl Runner) -> Result<()> {
        self.change(runner, State::Running, "lxc-freeze", State::Frozen)
    }

    /// # Errors
    /// The container is not frozen or does not thaw within 10 seconds.
    pub fn unfreeze(&self, runner: &mut impl Runner) -> Result<()> {
        self.change(runner, State::Frozen, "lxc-unfreeze", State::Running)
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
    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.lxc.status(runner)? != State::Stopped {
            return Err("container must be stopped before mounting".into());
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

    /// # Errors
    /// LXC does not stop or an unmount or network step fails.
    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        self.lxc.stop(runner)?;
        if self.lxc.status(runner)? != State::Stopped {
            return Err("refusing to unmount a live container".into());
        }
        self.mounts.unmount(runner)?;
        self.network.stop(runner)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeRunner {
        expected: VecDeque<(&'static str, &'static str)>,
        commands: Vec<Command>,
    }

    impl FakeRunner {
        fn new(expected: &[(&'static str, &'static str)]) -> Self {
            Self {
                expected: expected.iter().copied().collect(),
                commands: Vec::new(),
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, command: Command) -> Result<String> {
            let (program, output) = self.expected.pop_front().ok_or("unexpected command")?;
            if command.program() != program {
                return Err(format!("expected {program}, got {}", command.program()).into());
            }
            self.commands.push(command);
            Ok(output.into())
        }
    }

    #[test]
    fn start_and_stop_wait_for_real_lxc_states() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("deckard"))?;
        fs::write(root.path().join("deckard/config"), "lxc.rootfs.path = /x\n")?;
        let lxc = Lxc::new(root.path().into(), "deckard".into())?;
        let mut runner = FakeRunner::new(&[
            ("lxc-info", "STOPPED"),
            ("lxc-start", ""),
            ("lxc-info", "RUNNING"),
            ("lxc-info", "RUNNING"),
            ("lxc-stop", ""),
            ("lxc-info", "STOPPED"),
        ]);
        lxc.start(&mut runner)?;
        lxc.stop(&mut runner)?;
        assert!(runner.expected.is_empty());
        let root = root.path().display().to_string();
        let start = [
            "lxc-start",
            "-P",
            &root,
            "-n",
            "deckard",
            "-d",
            "--",
            "/init",
        ];
        assert_eq!(runner.commands[1].args, start);
        assert!(Lxc::new("/tmp".into(), "../waydroid".into()).is_err());
        Ok(())
    }

    #[test]
    fn property_read_is_a_bounded_attach_that_restores_frozen_state() -> Result<()> {
        let lxc = Lxc::new("/var/lib/deckard/lxc".into(), "deckard".into())?;
        let mut runner = FakeRunner::new(&[
            ("lxc-info", "FROZEN"),
            ("lxc-info", "FROZEN"),
            ("lxc-unfreeze", ""),
            ("lxc-info", "RUNNING"),
            ("lxc-attach", "1\n"),
            ("lxc-info", "RUNNING"),
            ("lxc-freeze", ""),
            ("lxc-info", "FROZEN"),
        ]);
        assert_eq!(lxc.get_property(&mut runner, "sys.boot_completed")?, "1");
        assert!(runner.expected.is_empty());
        let attach = &runner.commands[4];
        let expected = "lxc-attach -P /var/lib/deckard/lxc -n deckard --clear-env -- /system/bin/getprop sys.boot_completed";
        assert_eq!(attach.args.join(" "), expected);
        assert_eq!(attach.timeout, Some(Duration::from_secs(3)));
        assert!(lxc.get_property(&mut runner, "bad;key").is_err());
        assert!(
            lxc.set_property(&mut runner, "persist.x", "bad\nvalue")
                .is_err()
        );
        Ok(())
    }
}
