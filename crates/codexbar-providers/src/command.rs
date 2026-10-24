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

/// Runs commands with no console window, bounded output and a deadline covering collection and process exit.
/// Each stream is limited to 8 MiB. Oversized output fails without returning or logging a partial payload.
#[derive(Default)]
pub struct SystemCommandRunner;

mod pipe;
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_millis(25);

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput, CommandError> {
        let mut command = crate::platform::command(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let inner = crate::contained::spawn(&mut command).map_err(spawn_error)?;
        let mut process = Running { inner, stopped: false };
        let output = collect(&mut process.inner.child, timeout);
        let status = process.stop().map_err(|_| CommandError::Failed)?;
        let (stdout, stderr) = output?;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

fn spawn_error(error: std::io::Error) -> CommandError {
    if error.kind() == std::io::ErrorKind::NotFound {
        CommandError::NotFound
    } else {
        CommandError::Failed
    }
}

/// The root remains unreaped until group cleanup, so its process-group ID cannot refer to somebody else's work.
struct Running {
    inner: crate::contained::Contained,
    stopped: bool,
}
impl Running {
    fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !self.stopped {
            self.stopped = true;
            #[cfg(unix)]
            pipe::stop_group(&self.inner.child);
            let status = self.inner.stop();
            #[cfg(unix)]
            pipe::wait_group(self.inner.child.id())?;
            return status;
        }
        self.inner.child.wait()
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct Capture<P> {
    pipe: P,
    bytes: Vec<u8>,
    closed: bool,
}
impl<P: pipe::Pipe> Capture<P> {
    fn new(pipe: P) -> Result<Self, CommandError> {
        pipe::prepare(&pipe).map_err(|_| CommandError::Failed)?;
        Ok(Self {
            pipe,
            bytes: Vec::new(),
            closed: false,
        })
    }

    /// At most one chunk per stream per turn, so a noisy stream cannot starve the other stream or the deadline.
    fn drain(&mut self) -> Result<bool, CommandError> {
        if self.closed {
            return Ok(false);
        }
        let mut bytes = [0u8; 8192];
        match pipe::read(&mut self.pipe, &mut bytes).map_err(|_| CommandError::Failed)? {
            None => Ok(false),
            Some(0) => {
                self.closed = true;
                Ok(true)
            }
            Some(count) => {
                if count > OUTPUT_LIMIT - self.bytes.len() {
                    return Err(CommandError::Failed);
                }
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(true)
            }
        }
    }
}

fn collect(child: &mut std::process::Child, timeout: Duration) -> Result<(Vec<u8>, Vec<u8>), CommandError> {
    let start = Instant::now();
    let mut stdout = Capture::new(child.stdout.take().ok_or(CommandError::Failed)?)?;
    let mut stderr = Capture::new(child.stderr.take().ok_or(CommandError::Failed)?)?;
    loop {
        if start.elapsed() >= timeout {
            return Err(CommandError::TimedOut);
        }
        let progress = stdout.drain()? | stderr.drain()?;
        if stdout.closed && stderr.closed && pipe::finished(child).map_err(|_| CommandError::Failed)? {
            return Ok((stdout.bytes, stderr.bytes));
        }
        if !progress {
            std::thread::sleep(CHECK_INTERVAL.min(timeout.saturating_sub(start.elapsed())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spawning_errors_distinguish_missing_programs_from_other_failures() {
        assert_eq!(spawn_error(std::io::ErrorKind::NotFound.into()), CommandError::NotFound);
        assert_eq!(
            spawn_error(std::io::ErrorKind::PermissionDenied.into()),
            CommandError::Failed
        );
    }
}
