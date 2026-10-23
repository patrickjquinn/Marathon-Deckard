use crate::{Error, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

fn drain_output(mut pipe: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut truncated = false;
    loop {
        let size = pipe.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let retained = size.min(MAX_OUTPUT_BYTES.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..retained]);
        truncated |= retained != size;
    }
    Ok((output, truncated))
}

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
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Command("child stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Command("child stderr unavailable".into()))?;
        let stdout_reader = thread::spawn(move || drain_output(stdout));
        let stderr_reader = thread::spawn(move || drain_output(stderr));
        let stdin_writer = if let Some(input) = command.stdin {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| Error::Command("child stdin unavailable".into()))?;
            Some(thread::spawn(move || stdin.write_all(input.as_bytes())))
        } else {
            None
        };
        if let Some(timeout) = command.timeout {
            let deadline = Instant::now() + timeout;
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::Timeout(format!("{} timed out", command.program)));
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
        let status = child.wait()?;
        let (stdout, stdout_truncated) = stdout_reader
            .join()
            .map_err(|_| Error::Command("stdout reader panicked".into()))??;
        let (stderr, stderr_truncated) = stderr_reader
            .join()
            .map_err(|_| Error::Command("stderr reader panicked".into()))??;
        if let Some(writer) = stdin_writer {
            writer
                .join()
                .map_err(|_| Error::Command("stdin writer panicked".into()))??;
        }
        if stdout_truncated || stderr_truncated {
            return Err(Error::Command(format!(
                "{} output exceeded {} bytes",
                command.program, MAX_OUTPUT_BYTES
            )));
        }
        if !status.success() {
            return Err(Error::Command(format!(
                "{} failed ({}): {}",
                command.program,
                status,
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
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

    #[test]
    fn command_output_does_not_block_exit() {
        let mut runner = ProcessRunner;
        let result = runner.run(
            CommandSpec::new("head", ["-c", "1048576", "/dev/zero"])
                .with_timeout(Duration::from_millis(500)),
        );
        assert_eq!(result.map(|output| output.len()).ok(), Some(1_048_576));
    }
}
