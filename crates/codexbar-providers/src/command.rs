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

/// Options honored only for this child process, without changing the parent's environment.
#[derive(Clone, Debug)]
pub struct CommandOptions {
    /// The child's working folder; `None` keeps this process's.
    pub cwd: Option<std::path::PathBuf>,
    pub remove_env: Vec<&'static str>,
    pub env: Vec<(&'static str, &'static str)>,
    pub output_limit: usize,
    /// Stderr text that means the command now waits for a person (agy's sign-in prompt). Once it appears the command
    /// is stopped and returns what it wrote so far, instead of waiting out its deadline. ASCII case is ignored.
    pub stop_on_stderr: Option<&'static str>,
}

/// Runs local CLIs (`gh`, …). A seam so provider logic is tested without touching the user's real tools.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput, CommandError>;
    fn run_with_options(
        &self,
        _program: &str,
        _args: &[&str],
        _timeout: Duration,
        _options: &CommandOptions,
    ) -> Result<CommandOutput, CommandError> {
        Err(CommandError::Failed)
    }
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
        self.run_scoped(program, args, timeout, None)
    }
    fn run_with_options(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
        options: &CommandOptions,
    ) -> Result<CommandOutput, CommandError> {
        self.run_scoped(program, args, timeout, Some(options))
    }
}
impl SystemCommandRunner {
    fn run_scoped(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
        options: Option<&CommandOptions>,
    ) -> Result<CommandOutput, CommandError> {
        let mut command = crate::platform::command(program);
        if let Some(options) = options {
            configure(&mut command, options);
        }
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
        let output = collect(
            &mut process.inner.child,
            timeout,
            options.map_or(OUTPUT_LIMIT, |o| o.output_limit.min(OUTPUT_LIMIT)),
            options
                .and_then(|o| o.stop_on_stderr)
                .filter(|marker| !marker.is_empty()),
        );
        let status = process.stop().map_err(|_| CommandError::Failed)?;
        let (stdout, stderr) = output?;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

/// Applies the options that hold only for this child: its working folder and its environment.
fn configure(command: &mut std::process::Command, options: &CommandOptions) {
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    // Remove every case variant too, matching the shipped Muse subscription-only contract.
    for (name, _) in std::env::vars_os() {
        if options
            .remove_env
            .iter()
            .any(|removed| name.to_string_lossy().eq_ignore_ascii_case(removed))
        {
            command.env_remove(name);
        }
    }
    for name in &options.remove_env {
        command.env_remove(name);
    }
    command.envs(options.env.iter().copied());
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
    limit: usize,
}
impl<P: pipe::Pipe> Capture<P> {
    fn new(pipe: P, limit: usize) -> Result<Self, CommandError> {
        pipe::prepare(&pipe).map_err(|_| CommandError::Failed)?;
        Ok(Self {
            pipe,
            bytes: Vec::new(),
            closed: false,
            limit,
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
                if count > self.limit - self.bytes.len() {
                    return Err(CommandError::Failed);
                }
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(true)
            }
        }
    }
}

/// Whether `marker` (ignoring ASCII case) ends somewhere past `searched`, the length already searched.
fn contains_from(bytes: &[u8], searched: usize, marker: &[u8]) -> bool {
    let start = searched.saturating_sub(marker.len() - 1);
    bytes[start..]
        .windows(marker.len())
        .any(|window| window.eq_ignore_ascii_case(marker))
}

fn collect(
    child: &mut std::process::Child,
    timeout: Duration,
    limit: usize,
    stop_on_stderr: Option<&str>,
) -> Result<(Vec<u8>, Vec<u8>), CommandError> {
    let start = Instant::now();
    let mut stdout = Capture::new(child.stdout.take().ok_or(CommandError::Failed)?, limit)?;
    let mut stderr = Capture::new(child.stderr.take().ok_or(CommandError::Failed)?, limit)?;
    let mut searched = 0;
    loop {
        if start.elapsed() >= timeout {
            return Err(CommandError::TimedOut);
        }
        let progress = stdout.drain()? | stderr.drain()?;
        if let Some(marker) = stop_on_stderr {
            if contains_from(&stderr.bytes, searched, marker.as_bytes()) {
                return Ok((stdout.bytes, stderr.bytes));
            }
            searched = stderr.bytes.len();
        }
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

    #[test]
    fn a_stop_marker_is_found_across_reads_and_case() {
        let text = b"Authentication required. Please visit the URL";
        assert!(contains_from(text, 0, b"authentication required"));
        // Split after "Authenti": the second read still finds the marker that straddles both.
        assert!(contains_from(text, 8, b"authentication required"));
        assert!(!contains_from(text, text.len(), b"authentication required"));
        assert!(!contains_from(
            b"Authentication succeeded",
            0,
            b"authentication required"
        ));
    }
}
