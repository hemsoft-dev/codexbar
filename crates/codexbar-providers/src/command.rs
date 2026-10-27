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
            options.and_then(|o| o.stop_on_stderr).unwrap_or_default(),
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

    fn open_pipe(&self) -> Option<&P> {
        (!self.closed).then_some(&self.pipe)
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

/// Watches stderr for `CommandOptions::stop_on_stderr`, ignoring ASCII case. Each look searches only what arrived
/// since the previous one, plus enough before it to catch a marker split across reads. An empty marker never matches.
struct StopMarker<'a> {
    marker: &'a [u8],
    searched: usize,
}

impl StopMarker<'_> {
    fn seen(&mut self, stderr: &[u8]) -> bool {
        let start = self.searched.saturating_sub(self.marker.len().saturating_sub(1));
        self.searched = stderr.len();
        !self.marker.is_empty()
            && stderr[start..]
                .windows(self.marker.len())
                .any(|window| window.eq_ignore_ascii_case(self.marker))
    }
}

/// Whether collection is over: the command asked for a person (`stop`), or both streams closed and it exited.
fn complete<P: pipe::Pipe>(
    child: &mut std::process::Child,
    stdout_closed: bool,
    stderr: &Capture<P>,
    stop: &mut StopMarker,
) -> Result<bool, CommandError> {
    if stop.seen(&stderr.bytes) {
        return Ok(true);
    }
    Ok(stdout_closed && stderr.closed && pipe::finished(child).map_err(|_| CommandError::Failed)?)
}

fn collect(
    child: &mut std::process::Child,
    timeout: Duration,
    limit: usize,
    stop_on_stderr: &str,
) -> Result<(Vec<u8>, Vec<u8>), CommandError> {
    let start = Instant::now();
    let mut stdout = Capture::new(child.stdout.take().ok_or(CommandError::Failed)?, limit)?;
    let mut stderr = Capture::new(child.stderr.take().ok_or(CommandError::Failed)?, limit)?;
    let mut stop = StopMarker {
        marker: stop_on_stderr.as_bytes(),
        searched: 0,
    };
    loop {
        if start.elapsed() >= timeout {
            return Err(CommandError::TimedOut);
        }
        let progress = stdout.drain()? | stderr.drain()?;
        if complete(child, stdout.closed, &stderr, &mut stop)? {
            return Ok((stdout.bytes, stderr.bytes));
        }
        if !progress {
            pipe::wait(
                stdout.open_pipe(),
                stderr.open_pipe(),
                CHECK_INTERVAL.min(timeout.saturating_sub(start.elapsed())),
            );
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

    /// Every drain outcome on a real pipe, independent of how a command's writes and EOFs happen to interleave.
    #[cfg(unix)]
    #[test]
    fn a_drain_reports_pending_output_eof_a_closed_stream_and_overflow() {
        use std::io::Write as _;
        let (reader, mut writer) = std::io::pipe().unwrap();
        let mut capture = Capture::new(reader, 4).unwrap();
        assert_eq!(capture.drain(), Ok(false), "nothing written yet");
        writer.write_all(b"abc").unwrap();
        assert_eq!(capture.drain(), Ok(true));
        assert_eq!(capture.bytes, b"abc");
        writer.write_all(b"de").unwrap();
        assert_eq!(capture.drain(), Err(CommandError::Failed), "past the 4-byte limit");
        let (reader, writer) = std::io::pipe().unwrap();
        let mut capture = Capture::new(reader, 4).unwrap();
        drop(writer);
        assert_eq!(capture.drain(), Ok(true), "EOF is progress");
        assert!(capture.closed);
        assert_eq!(capture.drain(), Ok(false), "a closed stream is never read again");
    }

    /// Every way a collection turn ends or goes on, independent of which pipe a real command closes first.
    #[cfg(unix)]
    #[test]
    fn collection_ends_at_the_stop_marker_or_once_both_streams_closed_and_the_command_exited() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let (reader, _writer) = std::io::pipe().unwrap();
        let mut stderr = Capture {
            pipe: reader,
            bytes: b"Error: authentication required.".to_vec(),
            closed: false,
            limit: OUTPUT_LIMIT,
        };
        let stop = |marker| StopMarker { marker, searched: 0 };
        assert_eq!(
            complete(&mut child, false, &stderr, &mut stop(b"authentication required")),
            Ok(true)
        );
        assert_eq!(complete(&mut child, false, &stderr, &mut stop(b"")), Ok(false));
        assert_eq!(
            complete(&mut child, true, &stderr, &mut stop(b"")),
            Ok(false),
            "stderr still open"
        );
        stderr.closed = true;
        let deadline = Instant::now() + Duration::from_secs(5);
        while complete(&mut child, true, &stderr, &mut stop(b"")) != Ok(true) {
            assert!(Instant::now() < deadline, "the exited command ends collection");
            std::thread::sleep(Duration::from_millis(5));
        }
        child.wait().unwrap();
    }

    #[test]
    fn a_stop_marker_is_found_across_reads_and_case() {
        let text = b"Authentication required. Please visit the URL";
        let marker = |searched| StopMarker {
            marker: b"authentication required",
            searched,
        };
        assert!(marker(0).seen(text));
        // Read in two parts, split after "Authenti": the second look finds the marker that straddles both.
        let mut split = marker(0);
        assert!(!split.seen(&text[..8]));
        assert!(split.seen(text));
        // Already searched to the end: nothing new to find.
        assert!(!marker(text.len()).seen(text));
        assert!(!marker(0).seen(b"Authentication succeeded"));
        let mut none = StopMarker {
            marker: b"",
            searched: 0,
        };
        assert!(!none.seen(text), "an empty marker never stops a command");
    }
}
