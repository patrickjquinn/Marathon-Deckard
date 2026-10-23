use crate::command::{CommandSpec, Runner};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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
    pub fn new(root: PathBuf, name: String) -> Result<Self> {
        if !root.is_absolute()
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::Invalid("invalid LXC root or container name".into()));
        }
        Ok(Self { root, name })
    }

    fn command(&self, program: &str, tail: &[&str]) -> CommandSpec {
        let mut args = vec![
            "-P".into(),
            self.root.display().to_string(),
            "-n".into(),
            self.name.clone(),
        ];
        args.extend(tail.iter().map(|s| (*s).into()));
        CommandSpec::new(program, args)
    }

    pub fn status_command(&self) -> CommandSpec {
        self.command("lxc-info", &["-sH"])
    }

    pub fn start_command(&self) -> CommandSpec {
        self.command("lxc-start", &["-d", "--", "/init"])
    }

    pub fn stop_command(&self) -> CommandSpec {
        self.command("lxc-stop", &["-k"])
    }

    pub fn freeze_command(&self) -> CommandSpec {
        self.command("lxc-freeze", &[])
    }

    pub fn unfreeze_command(&self) -> CommandSpec {
        self.command("lxc-unfreeze", &[])
    }

    pub fn status(&self, runner: &mut impl Runner) -> Result<State> {
        match runner.run(self.status_command())?.trim() {
            "STOPPED" => Ok(State::Stopped),
            "RUNNING" => Ok(State::Running),
            "FROZEN" => Ok(State::Frozen),
            other => Err(Error::Invalid(format!("unrecognized LXC state: {other}"))),
        }
    }

    pub fn wait_for(
        &self,
        runner: &mut impl Runner,
        target: State,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.status(runner)? == target {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout(format!("LXC did not reach {target:?}")));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn start(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? != State::Stopped {
            return Err(Error::Invalid("container is not stopped".into()));
        }
        require_config(&self.root.join(&self.name).join("config"))?;
        runner.run(self.start_command())?;
        self.wait_for(runner, State::Running, Duration::from_secs(10))
    }

    pub fn stop(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? == State::Stopped {
            return Ok(());
        }
        runner.run(self.stop_command())?;
        self.wait_for(runner, State::Stopped, Duration::from_secs(30))
    }

    pub fn freeze(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? != State::Running {
            return Err(Error::Invalid("container is not running".into()));
        }
        runner.run(self.freeze_command())?;
        self.wait_for(runner, State::Frozen, Duration::from_secs(10))
    }

    pub fn unfreeze(&self, runner: &mut impl Runner) -> Result<()> {
        if self.status(runner)? != State::Frozen {
            return Err(Error::Invalid("container is not frozen".into()));
        }
        runner.run(self.unfreeze_command())?;
        self.wait_for(runner, State::Running, Duration::from_secs(10))
    }
}

fn require_config(path: &Path) -> Result<()> {
    if !path.is_file() || path.symlink_metadata()?.file_type().is_symlink() {
        return Err(Error::Invalid(format!(
            "LXC config is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeRunner {
        expected: VecDeque<(&'static str, &'static str)>,
    }

    impl Runner for FakeRunner {
        fn run(&mut self, command: CommandSpec) -> Result<String> {
            let (program, output) = self
                .expected
                .pop_front()
                .ok_or_else(|| Error::Invalid("unexpected command".into()))?;
            if command.program != program {
                return Err(Error::Invalid(format!(
                    "expected {program}, got {}",
                    command.program
                )));
            }
            Ok(output.into())
        }
    }

    #[test]
    fn commands_match_installed_lxc_interface() -> Result<()> {
        let lxc = Lxc::new("/var/lib/deckard/lxc".into(), "deckard".into())?;
        assert_eq!(
            lxc.start_command().args,
            [
                "-P",
                "/var/lib/deckard/lxc",
                "-n",
                "deckard",
                "-d",
                "--",
                "/init"
            ]
        );
        assert_eq!(
            lxc.stop_command().args.last().map(String::as_str),
            Some("-k")
        );
        assert_eq!(
            lxc.status_command().args.last().map(String::as_str),
            Some("-sH")
        );
        assert!(Lxc::new("/tmp".into(), "../waydroid".into()).is_err());
        Ok(())
    }

    #[test]
    fn start_and_stop_wait_for_real_lxc_states() -> Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Invalid(e.to_string()))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("deckard-lxc-{nonce}"));
        fs::create_dir_all(root.join("deckard"))?;
        fs::write(
            root.join("deckard/config"),
            b"lxc.rootfs.path = /tmp/test\n",
        )?;
        let lxc = Lxc::new(root.clone(), "deckard".into())?;
        let mut runner = FakeRunner {
            expected: VecDeque::from([
                ("lxc-info", "STOPPED"),
                ("lxc-start", ""),
                ("lxc-info", "RUNNING"),
                ("lxc-info", "RUNNING"),
                ("lxc-stop", ""),
                ("lxc-info", "STOPPED"),
            ]),
        };
        lxc.start(&mut runner)?;
        lxc.stop(&mut runner)?;
        assert!(runner.expected.is_empty());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
