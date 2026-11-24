//! A CLI's own sign-in, run for the user from a dashboard failure row (#337). An account on the GitHub CLI's, the Codex
//! CLI's, Claude Code's or the Grok CLI's own sign-in is fixed by signing that CLI in again; CodexBar runs the CLI's
//! sign-in in a console window of its own (inside a kill-on-close job), as the Claude and Grok sign-ins already do,
//! so the user doesn't have to find a terminal and type the command. Nothing starts without a click.

use std::path::PathBuf;

/// A CLI whose own sign-in an account can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cli {
    /// `gh auth login --web`: Copilot's Automatic and Command line methods read the GitHub CLI's accounts.
    GitHub,
    /// `codex login`: Codex's Automatic method reads the Codex CLI's sign-in.
    Codex,
    /// `claude auth login --claudeai`: Claude's OAuth and Automatic methods read Claude Code's own sign-in.
    Claude,
    /// `grok login`: Grok's Automatic method reads the Grok CLI's sign-in.
    Grok,
}

impl Cli {
    /// Its name, mid-sentence.
    pub fn name(self) -> &'static str {
        match self {
            Self::GitHub => "the GitHub CLI",
            Self::Codex => "the Codex CLI",
            Self::Claude => "Claude Code",
            Self::Grok => "the Grok CLI",
        }
    }

    /// The command the console runs, as the user would type it.
    pub fn command_line(self) -> &'static str {
        match self {
            Self::GitHub => "gh auth login --web --hostname github.com",
            Self::Codex => "codex login",
            Self::Claude => "claude auth login --claudeai",
            Self::Grok => "grok login",
        }
    }

    /// Its own instructions for signing in, when it publishes them.
    pub fn docs(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some("https://cli.github.com/manual/gh_auth_login"),
            Self::Codex | Self::Claude | Self::Grok => None,
        }
    }

    /// Where to get it, when it isn't installed and it has one download page.
    pub fn install(self) -> Option<&'static str> {
        match self {
            Self::GitHub => Some("https://cli.github.com/"),
            Self::Codex | Self::Claude | Self::Grok => None,
        }
    }
}

/// Why a CLI's sign-in couldn't start or didn't finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliSignInError {
    /// The CLI isn't installed (or not where CodexBar looks for it).
    NotInstalled,
    /// It started but exited without signing in (closed, cancelled, or failed).
    Failed,
    /// It couldn't be started.
    Start(String),
}

impl std::fmt::Display for CliSignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "isn't installed"),
            Self::Failed => write!(f, "didn't finish signing in"),
            Self::Start(reason) => write!(f, "couldn't start: {reason}"),
        }
    }
}

/// A CLI's sign-in running in its console window.
pub struct SignInWindow(Window);

enum Window {
    Console(crate::contained::Contained),
    Claude(Option<crate::claude_cli::LoginWindow>),
}

impl SignInWindow {
    /// Opens `cli`'s sign-in in a console window of its own.
    pub fn open(cli: Cli) -> Result<Self, CliSignInError> {
        if cli == Cli::Claude {
            // Claude Code's own folder, with its environment set the way CodexBar reads it (claude_cli).
            return crate::claude_cli::LoginWindow::open(&crate::claude::default_config_dir())
                .map(|window| Self(Window::Claude(Some(window))))
                .map_err(|err| match err {
                    crate::claude_cli::ClaudeCliError::NotInstalled => CliSignInError::NotInstalled,
                    other => CliSignInError::Start(other.to_string()),
                });
        }
        let program = program(cli).ok_or(CliSignInError::NotInstalled)?;
        let mut command = crate::platform::command(&program);
        command.args(args(cli));
        // The profile CodexBar reads, set on the command itself: a console started through a terminal (macOS and
        // Linux) gets only what is set here, and would otherwise sign in whatever profile the terminal points at.
        for (name, value) in profile(cli) {
            command.env(name, value);
        }
        if cli == Cli::GitHub {
            // A token in CodexBar's environment would make `gh` act as that token instead of signing in.
            for name in [
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
            ] {
                command.env_remove(name);
            }
        }
        crate::contained::spawn_console(&mut command)
            .map(|process| Self(Window::Console(process)))
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => CliSignInError::NotInstalled,
                _ => CliSignInError::Start(err.to_string()),
            })
    }

    /// Waits until the console window closes, up to `timeout`. Ok when the CLI reported success.
    pub fn finish(
        mut self,
        timeout: std::time::Duration,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), CliSignInError> {
        match &mut self.0 {
            Window::Claude(window) => {
                let Some(window) = window.take() else {
                    return Err(CliSignInError::Failed);
                };
                window.finish(timeout, cancel).map_err(|_| CliSignInError::Failed)
            }
            Window::Console(process) => {
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if cancel.load(std::sync::atomic::Ordering::SeqCst) || std::time::Instant::now() >= deadline {
                        let _ = process.child.kill();
                        let _ = process.child.wait();
                        return Err(CliSignInError::Failed);
                    }
                    match process.child.try_wait() {
                        Ok(Some(status)) if status.success() => return Ok(()),
                        Ok(Some(_)) | Err(_) => return Err(CliSignInError::Failed),
                        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(250)),
                    }
                }
            }
        }
    }
}

/// The CLI's arguments for signing in.
fn args(cli: Cli) -> &'static [&'static str] {
    match cli {
        Cli::GitHub => &["auth", "login", "--web", "--hostname", "github.com"],
        Cli::Codex | Cli::Grok => &["login"],
        Cli::Claude => &["auth", "login", "--claudeai"],
    }
}

/// The profile the CLI signs in, as the variables naming it: the folder its provider reads (`CODEX_HOME`, or
/// `~/.codex`; `GROK_HOME`, or `~/.grok`), and the GitHub CLI's `GH_CONFIG_DIR` when CodexBar runs with one.
fn profile(cli: Cli) -> Vec<(&'static str, std::ffi::OsString)> {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let home = |name: &'static str, folder: &str| {
        set(name)
            .or_else(|| crate::platform::home_dir().map(|home| home.join(folder).into_os_string()))
            .map(|value| (name, value))
    };
    match cli {
        Cli::GitHub => set("GH_CONFIG_DIR")
            .map(|value| ("GH_CONFIG_DIR", value))
            .into_iter()
            .collect(),
        Cli::Codex => home("CODEX_HOME", ".codex").into_iter().collect(),
        Cli::Grok => home("GROK_HOME", ".grok").into_iter().collect(),
        // Claude Code's folder is set by `claude_cli`.
        Cli::Claude => Vec::new(),
    }
}

/// Where the CLI is, found the way its provider finds it for refreshes.
fn program(cli: Cli) -> Option<PathBuf> {
    match cli {
        Cli::GitHub => crate::platform::find_program("gh"),
        Cli::Codex => crate::platform::find_program("codex"),
        Cli::Claude => crate::platform::find_program("claude"),
        // `grok::cli` falls back to a bare `grok`, which fails to start as NotFound when it isn't installed.
        Cli::Grok => Some(PathBuf::from(crate::grok::cli())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cli_runs_its_own_sign_in_command() {
        for cli in [Cli::GitHub, Cli::Codex, Cli::Claude, Cli::Grok] {
            let typed = cli.command_line().split(' ').skip(1).collect::<Vec<_>>();
            assert_eq!(typed, args(cli), "{cli:?} runs what its hint shows");
        }
    }

    #[test]
    fn only_the_github_cli_links_to_its_sign_in_and_install_pages() {
        assert_eq!(Cli::GitHub.docs(), Some("https://cli.github.com/manual/gh_auth_login"));
        assert_eq!(Cli::GitHub.install(), Some("https://cli.github.com/"));
        for cli in [Cli::Codex, Cli::Claude, Cli::Grok] {
            assert_eq!((cli.docs(), cli.install()), (None, None));
        }
    }

    #[test]
    fn the_codex_and_grok_sign_ins_name_the_folder_codexbar_reads() {
        let names = |cli| profile(cli).into_iter().map(|(name, _)| name).collect::<Vec<_>>();
        assert_eq!(names(Cli::Codex), ["CODEX_HOME"]);
        assert_eq!(names(Cli::Grok), ["GROK_HOME"]);
        assert!(names(Cli::Claude).is_empty(), "claude_cli sets Claude Code's folder");
        let codex = profile(Cli::Codex).remove(0).1;
        assert_eq!(
            std::path::Path::new(&codex).join("auth.json"),
            crate::codex::default_auth_path(),
            "the same auth.json the Codex provider reads"
        );
    }

    #[test]
    fn errors_say_what_happened() {
        assert_eq!(CliSignInError::NotInstalled.to_string(), "isn't installed");
        assert_eq!(CliSignInError::Failed.to_string(), "didn't finish signing in");
        assert_eq!(
            CliSignInError::Start("access denied".into()).to_string(),
            "couldn't start: access denied"
        );
    }
}
