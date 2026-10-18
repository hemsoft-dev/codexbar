//! Signing a GitHub account in for CodexBar through the GitHub CLI's own browser sign-in (#79), without touching the
//! user's own `gh` setup.
//!
//! `gh auth login --web` runs in a private, temporary GitHub CLI config folder with `--insecure-storage`, so the token
//! lands in that folder's `hosts.yml` rather than in the Windows credential store `gh` shares across config folders.
//! Not running in a terminal, `gh` prints the one-time code and the device page instead of prompting, and sets up no
//! git credential helper. Once it reports the sign-in, CodexBar reads the token with `gh auth token`, hands it to the
//! caller (who keeps it in Credential Manager) and deletes the folder. CodexBar holds no GitHub client id of its own.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const HOST: &str = "github.com";
const POLL: Duration = Duration::from_millis(200);
/// How long `gh` gets to print the code and the device page.
const START_TIMEOUT: Duration = Duration::from_secs(60);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Environment variables that would make `gh` use another token or host than the sign-in.
const OVERRIDES: [&str; 5] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
];

/// Why a GitHub sign-in failed. Never carries the token or the CLI's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhLoginError {
    NotInstalled,
    /// The GitHub CLI exited without signing in, or its output couldn't be read.
    Failed,
    TimedOut,
    Cancelled,
}

impl std::fmt::Display for GhLoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInstalled => "The GitHub CLI isn't installed, or `gh` isn't on PATH.",
            Self::Failed => "The GitHub CLI didn't complete the sign-in.",
            Self::TimedOut => "The sign-in took too long and was stopped.",
            Self::Cancelled => "Sign-in was cancelled.",
        })
    }
}

impl std::error::Error for GhLoginError {}

/// The code to enter on GitHub's device page, and that page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub code: String,
    pub url: String,
}

/// A signed-in GitHub account. `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq)]
pub struct GhAccount {
    pub username: String,
    pub token: String,
}

impl std::fmt::Debug for GhAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhAccount")
            .field("username", &self.username)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// What `gh auth login --web` reported on one line of its output.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    Code(String),
    Url(String),
    SignedIn(String),
    Other,
}

/// Reads one line of `gh`'s sign-in output (colors are off: it isn't a terminal).
fn parse_line(line: &str) -> Line {
    let line = line.trim();
    let after = |marker: &str| {
        let start = line.find(marker)? + marker.len();
        line[start..].split_whitespace().next().map(str::to_owned)
    };
    if let Some(code) = after("one-time code: ").filter(|code| code.len() >= 4) {
        return Line::Code(code);
    }
    // With the clipboard on, gh says "One-time code (ABCD-1234) copied to clipboard".
    if let Some(code) = after("One-time code (")
        .map(|rest| rest.trim_end_matches(')').to_owned())
        .filter(|code| code.len() >= 4)
    {
        return Line::Code(code);
    }
    if line.contains("Open this URL")
        && let Some(url) = line.split_whitespace().find(|word| word.starts_with("https://"))
    {
        return Line::Url(url.to_owned());
    }
    if let Some(user) = after("Logged in as ") {
        return Line::SignedIn(user);
    }
    Line::Other
}

/// A sign-in `gh` is waiting on: the code is shown, and the browser page waits for it.
pub struct PendingLogin {
    process: crate::contained::Contained,
    lines: Receiver<Line>,
    config: TempConfig,
    device: DeviceCode,
    program: PathBuf,
}

impl PendingLogin {
    /// Starts `gh auth login --web` in a new private config folder inside `parent`, asking for `scopes` beyond the
    /// GitHub CLI's own (`repo`, `read:org`, `gist`), and waits for the code. `gh` runs in a kill-on-close job, so a
    /// sign-in left pending when CodexBar exits or crashes stops with it rather than finishing unseen.
    pub fn start(parent: &Path, scopes: &[&str]) -> Result<Self, GhLoginError> {
        let program = gh_program().ok_or(GhLoginError::NotInstalled)?;
        let config = TempConfig::new(parent).map_err(|_| GhLoginError::Failed)?;
        let mut command = gh(&program, &config.0);
        command.args(["auth", "login", "--web", "--hostname", HOST, "--insecure-storage"]);
        if !scopes.is_empty() {
            command.args(["--scopes", &scopes.join(",")]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = crate::contained::spawn(&mut command).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => GhLoginError::NotInstalled,
            _ => GhLoginError::Failed,
        })?;
        let child = &mut process.child;
        let (sender, lines) = channel();
        for stream in [
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(parse_line(&line)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut pending = Self {
            process,
            lines,
            config,
            device: DeviceCode {
                code: String::new(),
                url: String::new(),
            },
            program,
        };
        let deadline = Instant::now() + START_TIMEOUT;
        let (mut code, mut url) = (None, None);
        while code.is_none() || url.is_none() {
            match pending.next_line(deadline) {
                Ok(Line::Code(found)) => code = Some(found),
                Ok(Line::Url(found)) => url = Some(found),
                Ok(_) => {}
                Err(err) => {
                    pending.stop();
                    return Err(err);
                }
            }
        }
        pending.device = DeviceCode {
            code: code.unwrap_or_default(),
            url: url.unwrap_or_default(),
        };
        Ok(pending)
    }

    pub fn device(&self) -> &DeviceCode {
        &self.device
    }

    /// Waits until GitHub confirms the code, then reads the token. Cancellation or the timeout stops `gh`.
    pub fn finish(mut self, timeout: Duration, cancel: &AtomicBool) -> Result<GhAccount, GhLoginError> {
        let deadline = Instant::now() + timeout;
        let mut username = None;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.stop();
                return Err(GhLoginError::Cancelled);
            }
            match self.next_line((Instant::now() + POLL).min(deadline)) {
                Ok(Line::SignedIn(user)) => username = Some(user),
                Ok(_) => {}
                Err(GhLoginError::TimedOut) if Instant::now() < deadline => {}
                // Output closed: `gh` is exiting.
                Err(GhLoginError::Failed) => break,
                Err(err) => {
                    self.stop();
                    return Err(err);
                }
            }
        }
        let signed_in = self
            .process
            .child
            .wait()
            .map(|status| status.success())
            .unwrap_or(false);
        let username = username.filter(|_| signed_in).ok_or(GhLoginError::Failed)?;
        let token = self.token()?;
        Ok(GhAccount { username, token })
    }

    fn token(&self) -> Result<String, GhLoginError> {
        let mut command = gh(&self.program, &self.config.0);
        command
            .args(["auth", "token", "--hostname", HOST])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let process = crate::contained::spawn(&mut command).map_err(|_| GhLoginError::Failed)?;
        let output = wait_output(process, TOKEN_TIMEOUT)?;
        let token = String::from_utf8(output).map_err(|_| GhLoginError::Failed)?;
        let token = token.trim();
        if token.is_empty() {
            Err(GhLoginError::Failed)
        } else {
            Ok(token.to_owned())
        }
    }

    fn next_line(&mut self, deadline: Instant) -> Result<Line, GhLoginError> {
        match self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(GhLoginError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(GhLoginError::Failed),
        }
    }

    fn stop(&mut self) {
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
    }
}

impl Drop for PendingLogin {
    fn drop(&mut self) {
        // A sign-in left behind (the app quitting) doesn't keep `gh` polling GitHub.
        if let Ok(None) = self.process.child.try_wait() {
            self.stop();
        }
    }
}

/// Waits for `child` to exit and returns its stdout, within `timeout`.
fn wait_output(mut process: crate::contained::Contained, timeout: Duration) -> Result<Vec<u8>, GhLoginError> {
    use std::io::Read as _;
    let child = &mut process.child;
    let mut stdout = child.stdout.take().ok_or(GhLoginError::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return Err(GhLoginError::Failed),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GhLoginError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    reader.join().ok().and_then(Result::ok).ok_or(GhLoginError::Failed)
}

/// `gh` with the private config folder, no prompts, colors, update checks or token overrides.
fn gh(program: &Path, config: &Path) -> Command {
    let mut command = crate::platform::command(program);
    command
        .env("GH_CONFIG_DIR", config)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    for name in OVERRIDES {
        command.env_remove(name);
    }
    // On Linux `gh` asks the Secret Service for a stored token even with `--insecure-storage`, and a locked keyring
    // with nobody to unlock it holds it there forever (#161). A private folder never uses the keyring, so `gh` gets a
    // session bus it can't reach and goes straight on.
    #[cfg(not(any(windows, target_os = "macos")))]
    command.env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/dev/null");
    command
}

/// Deletes private config folders a previous run left behind (CodexBar was stopped mid sign-in, or a delete failed),
/// so no token stays on disk. Returns how many were removed.
pub fn clean_up(parent: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with(PREFIX)
            && entry.file_type().is_ok_and(|kind| kind.is_dir());
        if stale {
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn gh_program() -> Option<PathBuf> {
    crate::platform::find_program("gh")
}

/// The name every private config folder starts with.
const PREFIX: &str = "gh-sign-in-";

/// A private GitHub CLI config folder, deleted with its token when the sign-in ends.
struct TempConfig(PathBuf);

impl TempConfig {
    fn new(parent: &Path) -> std::io::Result<Self> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|_| std::io::Error::other("no randomness"))?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = parent.join(format!("{PREFIX}{name}"));
        std::fs::create_dir_all(&dir)?;
        let config = Self(dir);
        // `gh` copies the code to the clipboard by default, which would replace whatever the user copied; the
        // dialog shows the code with its own Copy button instead.
        std::fs::write(config.0.join("config.yml"), "clipboard: disabled\nprompt: disabled\n")?;
        Ok(config)
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_output_is_read_line_by_line() {
        // `gh auth login --web` output when not in a terminal (gh 2.x).
        assert_eq!(
            parse_line("! First copy your one-time code: ABCD-1234"),
            Line::Code("ABCD-1234".to_owned())
        );
        assert_eq!(
            parse_line("Open this URL to continue in your web browser: https://github.com/login/device"),
            Line::Url("https://github.com/login/device".to_owned())
        );
        assert_eq!(
            parse_line("✓ Logged in as octocat"),
            Line::SignedIn("octocat".to_owned())
        );
        assert_eq!(
            parse_line("! One-time code (ABCD-1234) copied to clipboard"),
            Line::Code("ABCD-1234".to_owned())
        );
        assert_eq!(parse_line("✓ Authentication complete."), Line::Other);
        assert_eq!(parse_line("Open this URL: http://insecure.example"), Line::Other);
        assert_eq!(parse_line(""), Line::Other);
    }

    /// Starts a real device sign-in with the installed GitHub CLI and stops it once the code is shown: checks that `gh`
    /// prints the code and page when it isn't in a terminal, and that the private folder is gone afterwards. Opens no
    /// browser. Run with `--ignored`.
    #[test]
    #[ignore = "starts the installed GitHub CLI and asks GitHub for a device code"]
    fn the_installed_github_cli_shows_a_device_code() {
        let parent = std::env::temp_dir().join(format!("codexbar-live-gh-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        let login = PendingLogin::start(&parent, &[]).expect("gh prints a code");
        let device = login.device().clone();
        assert!(device.code.len() >= 8 && device.code.contains('-'), "{}", device.code);
        assert_eq!(device.url, "https://github.com/login/device");
        let cancel = AtomicBool::new(true);
        assert_eq!(
            login.finish(Duration::from_secs(5), &cancel),
            Err(GhLoginError::Cancelled)
        );
        assert_eq!(
            std::fs::read_dir(&parent).unwrap().count(),
            0,
            "the private config folder is deleted"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn folders_left_by_an_earlier_run_are_deleted_and_nothing_else() {
        let parent = std::env::temp_dir().join(format!("codexbar-gh-clean-{}", std::process::id()));
        let stale = parent.join("gh-sign-in-0123456789abcdef");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("hosts.yml"), "token").unwrap();
        std::fs::create_dir_all(parent.join("codex")).unwrap();
        std::fs::write(parent.join("gh-sign-in-note.txt"), "a file, not a folder").unwrap();
        assert_eq!(clean_up(&parent).unwrap(), 1);
        assert!(!stale.exists());
        assert!(parent.join("codex").exists() && parent.join("gh-sign-in-note.txt").exists());
        assert_eq!(clean_up(&parent.join("missing")).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_signed_in_account_never_shows_its_token() {
        let account = GhAccount {
            username: "octocat".to_owned(),
            token: "gho_secret".to_owned(),
        };
        let shown = format!("{account:?}");
        assert!(shown.contains("octocat") && !shown.contains("gho_secret"));
    }

    #[test]
    fn the_private_config_folder_is_deleted_with_it() {
        let parent = std::env::temp_dir().join(format!("codexbar-gh-config-{}", std::process::id()));
        let config = TempConfig::new(&parent).unwrap();
        let dir = config.0.clone();
        std::fs::write(dir.join("hosts.yml"), "github.com: {}").unwrap();
        let config_yml = std::fs::read_to_string(dir.join("config.yml")).unwrap();
        assert!(config_yml.contains("clipboard: disabled"));
        assert!(dir.starts_with(&parent) && dir.is_dir());
        drop(config);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
