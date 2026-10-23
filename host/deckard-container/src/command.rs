use crate::{Error, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub timeout: Option<Duration>,
}

impl CommandSpec {
    pub fn new(program: &str, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            stdin: None,
            timeout: None,
        }
    }

    pub fn with_stdin(mut self, stdin: String) -> Self {
        self.stdin = Some(stdin);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

pub trait Runner {
    fn run(&mut self, command: CommandSpec) -> Result<String>;
}

pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&mut self, command: CommandSpec) -> Result<String> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(input) = command.stdin {
            let Some(mut stdin) = child.stdin.take() else {
                return Err(Error::Command("child stdin unavailable".into()));
            };
            stdin.write_all(input.as_bytes())?;
        }
        if let Some(timeout) = command.timeout {
            let deadline = Instant::now() + timeout;
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait_with_output();
                    return Err(Error::Timeout(format!("{} timed out", command.program)));
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(Error::Command(format!(
                "{} failed ({}): {}",
                command.program,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_command_is_killed_on_timeout() {
        let mut runner = ProcessRunner;
        let result =
            runner.run(CommandSpec::new("sleep", ["1"]).with_timeout(Duration::from_millis(40)));
        assert!(matches!(result, Err(Error::Timeout(_))));
    }
}
