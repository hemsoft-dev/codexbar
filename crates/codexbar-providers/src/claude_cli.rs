//! Signing Claude accounts in and out and renewing them through Claude Code's own CLI (#80), each in its own config
//! folder (`CLAUDE_CONFIG_DIR`). Claude Code writes `.credentials.json` and `.claude.json` there; CodexBar only reads
//! them and never holds an Anthropic client id.
//!
//! - `claude auth login` is an interactive terminal UI, so it runs in a console window of its own for the user.
//! - `claude auth status --json` makes Claude Code check the sign-in, renewing an expired token through its own
//!   refresh, which takes Claude Code's refresh lock (`.oauth_refresh.lock.owner`) so it never races another Claude
//!   Code process. That is CodexBar's renewal: a stale refresh can't overwrite newer credentials because CodexBar
//!   never writes them.
//! - `claude auth logout` deletes the folder's sign-in.
//!
//! Every process runs in a kill-on-close job, so none outlives CodexBar.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

const STATUS_TIMEOUT: Duration = Duration::from_secs(60);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

/// Why a Claude Code command failed. Never carries tokens or the CLI's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeCliError {
    NotInstalled,
    Failed,
    TimedOut,
    Cancelled,
}

impl std::fmt::Display for ClaudeCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInstalled => "Claude Code isn't installed, or `claude` isn't on PATH.",
            Self::Failed => "Claude Code didn't complete the sign-in.",
            Self::TimedOut => "Claude Code didn't finish in time.",
            Self::Cancelled => "Sign-in was cancelled.",
        })
    }
}

impl std::error::Error for ClaudeCliError {}

/// The sign-in file in a Claude Code config folder.
pub fn credentials_path(config: &Path) -> PathBuf {
    config.join(".credentials.json")
}

/// Asks Claude Code to check the folder's sign-in, renewing it if it has expired. Ok when Claude Code reports a
/// signed-in account afterwards.
pub fn renew(config: &Path) -> Result<(), ClaudeCliError> {
    let output = run(config, &["auth", "status", "--json"], STATUS_TIMEOUT)?;
    let status: Value = serde_json::from_slice(&output).map_err(|_| ClaudeCliError::Failed)?;
    match status.get("loggedIn").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(ClaudeCliError::Failed),
    }
}

/// Signs the folder out; Claude Code deletes its sign-in.
pub fn sign_out(config: &Path) -> Result<(), ClaudeCliError> {
    run(config, &["auth", "logout"], LOGOUT_TIMEOUT)?;
    crate::claude::signed_out(&credentials_path(config));
    Ok(())
}

/// A `claude auth login` window the user is signing in through.
pub struct LoginWindow {
    process: crate::contained::Contained,
    credentials: PathBuf,
}

impl LoginWindow {
    /// Opens `claude auth login --claudeai` for `config` in a console window of its own.
    pub fn open(config: &Path) -> Result<Self, ClaudeCliError> {
        let mut command = claude(config)?;
        command.args(["auth", "login", "--claudeai"]);
        let process = crate::contained::spawn_console(&mut command).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ClaudeCliError::NotInstalled,
            _ => ClaudeCliError::Failed,
        })?;
        Ok(Self {
            process,
            credentials: credentials_path(config),
        })
    }

    /// Waits until the window closes, then checks that Claude Code reported success and left a sign-in (a sign-in
    /// that was abandoned leaves the previous credentials, which must not pass for a new one). Cancellation or the
    /// timeout closes the window.
    pub fn finish(mut self, timeout: Duration, cancel: &AtomicBool) -> Result<(), ClaudeCliError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.close();
                return Err(ClaudeCliError::Cancelled);
            }
            match self.process.child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(_)) => return Err(ClaudeCliError::Failed),
                Ok(None) if Instant::now() >= deadline => {
                    self.close();
                    return Err(ClaudeCliError::TimedOut);
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(_) => return Err(ClaudeCliError::Failed),
            }
        }
        if crate::claude::read_credentials(&self.credentials).is_ok() {
            Ok(())
        } else {
            Err(ClaudeCliError::Failed)
        }
    }

    fn close(&mut self) {
        let _ = self.process.child.kill();
        let _ = self.process.child.wait();
    }
}

/// Runs a short `claude` command for `config` with no window, returning its stdout.
fn run(config: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, ClaudeCliError> {
    use std::io::Read as _;
    let mut command = claude(config)?;
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = crate::contained::spawn(&mut command).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ClaudeCliError::NotInstalled,
        _ => ClaudeCliError::Failed,
    })?;
    let mut stdout = process.child.stdout.take().ok_or(ClaudeCliError::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    loop {
        match process.child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return Err(ClaudeCliError::Failed),
            Ok(None) if Instant::now() >= deadline => {
                let _ = process.child.kill();
                let _ = process.child.wait();
                return Err(ClaudeCliError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
    reader.join().ok().and_then(Result::ok).ok_or(ClaudeCliError::Failed)
}

fn claude(config: &Path) -> Result<Command, ClaudeCliError> {
    let program = claude_program().ok_or(ClaudeCliError::NotInstalled)?;
    Ok(command_for(&program, config))
}

/// `program` set up to act on `config` only.
fn command_for(program: &Path, config: &Path) -> Command {
    let mut command = crate::platform::command(program);
    // Claude Code prefers CLAUDE_SECURESTORAGE_CONFIG_DIR over CLAUDE_CONFIG_DIR for its credentials
    // (anthropics/claude-code#79223), so it is always set to the one CodexBar reads the sign-in from
    // (`claude::secure_storage_selector`): empty for Claude Code's own `~/.claude`, the folder for one CodexBar signed
    // in, so an inherited one never signs in, renews or signs out another folder.
    command.env("CLAUDE_CONFIG_DIR", config).env(
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
        crate::claude::secure_storage_selector(config),
    );
    // An API key or token in CodexBar's environment would sign Claude Code in as someone else.
    for name in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"] {
        command.env_remove(name);
    }
    command
}

/// Claude Code: `claude.exe` (the native install), else `claude.cmd` (npm) on Windows, from PATH or an install
/// folder.
fn claude_program() -> Option<PathBuf> {
    crate::platform::find_program("claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_claude_command_acts_on_its_own_folder_only() {
        let config = Path::new(r"C:\codexbar\claude\abc");
        let command = command_for(Path::new("claude.exe"), config);
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let value = |name: &str| {
            envs.iter()
                .find(|(known, _)| known == name)
                .map(|(_, value)| value.clone())
        };
        let folder = Some(Some(config.to_string_lossy().into_owned()));
        assert_eq!(value("CLAUDE_CONFIG_DIR"), folder);
        // Claude Code prefers this one for credentials, so it must name the same folder.
        assert_eq!(value("CLAUDE_SECURESTORAGE_CONFIG_DIR"), folder);
        for name in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"] {
            assert_eq!(value(name), Some(None), "{name} is removed");
        }
    }

    #[test]
    fn claude_codes_own_folder_keeps_its_plain_keychain_item() {
        if ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            return;
        }
        let home = crate::platform::home_dir().expect("a home folder");
        let command = command_for(Path::new("claude"), &home.join(".claude"));
        let secure_storage = command
            .get_envs()
            .find(|(name, _)| *name == "CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .and_then(|(_, value)| value);
        // Empty means `~/.claude` to Claude Code, under the item name without a folder hash.
        assert_eq!(secure_storage, Some(std::ffi::OsStr::new("")));
    }

    /// Runs the installed Claude Code against a temporary folder holding a fake, expired sign-in: `auth status`
    /// tries Claude Code's own refresh (which fails for the fake token, so CodexBar reports it not renewed) and
    /// leaves the file as it was, and `auth logout` deletes it. Opens no window. Run with `--ignored`.
    #[test]
    #[ignore = "starts the installed Claude Code"]
    fn the_installed_claude_code_checks_and_signs_out_a_folder() {
        let config = std::env::temp_dir().join(format!("codexbar-live-claude-{}", std::process::id()));
        std::fs::create_dir_all(&config).unwrap();
        let fake = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-fake","refreshToken":"sk-ant-ort01-fake","expiresAt":1700000000000,"subscriptionType":"pro"}}"#;
        std::fs::write(credentials_path(&config), fake).unwrap();
        let _ = renew(&config);
        assert_eq!(std::fs::read_to_string(credentials_path(&config)).unwrap(), fake);
        assert_eq!(sign_out(&config), Ok(()));
        assert!(!credentials_path(&config).exists());
        let _ = std::fs::remove_dir_all(&config);
    }
}
