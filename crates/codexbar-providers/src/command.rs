use std::process::Stdio;
use std::time::{Duration, Instant};

/// Output of a finished command. `stdout` may carry secrets (tokens); callers must never log it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why a command produced no output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The program is not installed or not on PATH.
    NotFound,
    TimedOut,
    Failed,
}

/// Runs local CLIs (`gh`, …). A seam so provider logic is tested without touching the user's real tools.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput, CommandError>;
}

/// Runs commands with no console window and a hard timeout.
#[derive(Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput, CommandError> {
        let mut command = crate::platform::command(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => CommandError::NotFound,
            _ => CommandError::Failed,
        })?;

        // gh output is small, so polling for exit before reading the pipes cannot deadlock on a full buffer.
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CommandError::TimedOut);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => return Err(CommandError::Failed),
            }
        }
        let output = child.wait_with_output().map_err(|_| CommandError::Failed)?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
