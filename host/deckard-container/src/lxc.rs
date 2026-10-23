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

    pub fn get_property_command(&self, key: &str) -> Result<CommandSpec> {
        validate_property_key(key)?;
        Ok(self.property_command("/system/bin/getprop", &[key]))
    }

    pub fn set_property_command(&self, key: &str, value: &str) -> Result<CommandSpec> {
        validate_property_key(key)?;
        validate_property_value(value)?;
        Ok(self.property_command("/system/bin/setprop", &[key, value]))
    }

    fn property_command(&self, program: &str, args: &[&str]) -> CommandSpec {
        let mut command = self.command("lxc-attach", &["--clear-env", "--", program]);
        command
            .args
            .extend(args.iter().map(|arg| (*arg).to_owned()));
        command.with_timeout(Duration::from_secs(3))
    }

    pub fn get_property(&self, runner: &mut impl Runner, key: &str) -> Result<String> {
        let command = self.get_property_command(key)?;
        self.with_running(runner, |runner| {
            runner
                .run(command)
                .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        })
    }

    pub fn set_property(&self, runner: &mut impl Runner, key: &str, value: &str) -> Result<()> {
        let command = self.set_property_command(key, value)?;
        self.with_running(runner, |runner| runner.run(command).map(|_| ()))
    }

    fn with_running<R: Runner, T>(
        &self,
        runner: &mut R,
        action: impl FnOnce(&mut R) -> Result<T>,
    ) -> Result<T> {
        let state = self.status(runner)?;
        if state == State::Stopped {
            return Err(Error::Invalid("container is stopped".into()));
        }
        if state == State::Frozen {
            self.unfreeze(runner)?;
        }
        let result = action(runner);
        if state == State::Frozen {
            let refreeze = self.freeze(runner);
            return match (result, refreeze) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Err(operation), Err(refreeze)) => Err(Error::Command(format!(
                    "property operation failed: {operation}; refreeze failed: {refreeze}"
                ))),
            };
        }
        result
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

pub fn validate_property_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 128
        || key.starts_with(['.', '-'])
        || key.ends_with('.')
        || key.contains("..")
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(Error::Invalid("invalid Android property key".into()));
    }
    Ok(())
}

pub fn validate_property_value(value: &str) -> Result<()> {
    if value.len() > 1024
        || value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(Error::Invalid("invalid property value".into()));
    }
    Ok(())
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

    #[test]
    fn property_commands_are_direct_bounded_lxc_attach() -> Result<()> {
        let lxc = Lxc::new("/var/lib/deckard/lxc".into(), "deckard".into())?;
        let get = lxc.get_property_command("sys.boot_completed")?;
        assert_eq!(get.program, "lxc-attach");
        assert_eq!(
            get.args,
            [
                "-P",
                "/var/lib/deckard/lxc",
                "-n",
                "deckard",
                "--clear-env",
                "--",
                "/system/bin/getprop",
                "sys.boot_completed"
            ]
        );
        assert_eq!(get.timeout, Some(Duration::from_secs(3)));
        let set = lxc.set_property_command("persist.deckard.test", "ready")?;
        assert_eq!(set.args.last().map(String::as_str), Some("ready"));
        assert!(lxc.get_property_command("bad;key").is_err());
        assert!(
            lxc.set_property_command("persist.deckard.test", "bad\nvalue")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn property_read_restores_frozen_state() -> Result<()> {
        let lxc = Lxc::new("/var/lib/deckard/lxc".into(), "deckard".into())?;
        let mut runner = FakeRunner {
            expected: VecDeque::from([
                ("lxc-info", "FROZEN"),
                ("lxc-info", "FROZEN"),
                ("lxc-unfreeze", ""),
                ("lxc-info", "RUNNING"),
                ("lxc-attach", "1\n"),
                ("lxc-info", "RUNNING"),
                ("lxc-freeze", ""),
                ("lxc-info", "FROZEN"),
            ]),
        };
        assert_eq!(lxc.get_property(&mut runner, "sys.boot_completed")?, "1");
        assert!(runner.expected.is_empty());
        Ok(())
    }
}
