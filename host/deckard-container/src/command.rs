//! External commands with bounded output and an optional time limit.

use std::io::Read;
use std::process::{Child, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Result;

const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

/// A program and its arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub args: Vec<String>,
    pub timeout: Option<Duration>,
}

impl Command {
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            timeout: None,
        }
    }

    /// `line` split on whitespace; only for words that hold none.
    #[must_use]
    pub fn line(line: &str) -> Self {
        Self::new(line.split_whitespace())
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(pipe) = pipe {
            pipe.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut output)?;
        }
        Ok(output)
    })
}

fn wait(child: &mut Child, timeout: Duration, program: &str) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} timed out").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `command` to completion and returns its standard output.
///
/// # Errors
/// The command cannot start, fails, times out or prints more than 8 MiB.
pub fn run(command: Command) -> Result<String> {
    let (program, args) = command.args.split_first().ok_or("empty command")?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = match command.timeout {
        Some(timeout) => wait(&mut child, timeout, program)?,
        None => child.wait()?,
    };
    let stdout = stdout.join().map_err(|_| "stdout reader panicked")??;
    let stderr = stderr.join().map_err(|_| "stderr reader panicked")??;
    if stdout.len().max(stderr.len()) as u64 > MAX_OUTPUT_BYTES {
        return Err(format!("{program} output exceeded {MAX_OUTPUT_BYTES} bytes").into());
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(format!("{program} failed ({status}): {}", stderr.trim()).into());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_command_is_killed_on_timeout() {
        let command = Command::line("sleep 1").timeout(Duration::from_millis(40));
        let result = run(command);
        assert!(result.is_err_and(|error| error.to_string().contains("timed out")));
    }

    #[test]
    fn command_output_does_not_block_exit() {
        let script = "head -c 1048576 /dev/zero >&2; head -c 1048576 /dev/zero";
        let command = Command::new(["sh", "-c", script]).timeout(Duration::from_millis(500));
        let result = run(command);
        assert_eq!(result.map(|output| output.len()).ok(), Some(1_048_576));
    }
}
