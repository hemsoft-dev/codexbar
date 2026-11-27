//! A CLI's own sign-in, run for the user from a dashboard failure row (#337). An account on the GitHub CLI's, the Codex
//! CLI's, Claude Code's or the Grok CLI's own sign-in is fixed by signing that CLI in again; CodexBar runs the CLI's
//! sign-in in a console window of its own (inside a kill-on-close job), as the Claude and Grok sign-ins already do,
//! so the user doesn't have to find a terminal and type the command. Nothing starts without a click.

use std::path::PathBuf;
use std::process::Command;

/// A CLI whose own sign-in an account can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    Claude(crate::claude_cli::LoginWindow),
}

impl SignInWindow {
    /// Opens `cli`'s sign-in in a console window of its own.
    pub fn open(cli: Cli) -> Result<Self, CliSignInError> {
        if cli == Cli::Claude {
            return open_claude();
        }
        let base = program(
            cli,
            crate::platform::home_dir().as_deref(),
            &crate::platform::find_program,
        )
        .map(crate::platform::command);
        open_with(cli, base, profile(cli), crate::contained::spawn_console)
    }
    /// Waits until the console window closes, up to `timeout`. Ok when the CLI reported success. A sign-in stopped by
    /// the timeout or `cancel` returns once its CLI has stopped (`Contained::close_console`), so a retry can't overlap it.
    pub fn finish(
        self,
        timeout: std::time::Duration,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), CliSignInError> {
        match self.0 {
            Window::Claude(window) => window.finish(timeout, cancel).map_err(|_| CliSignInError::Failed),
            Window::Console(mut process) => wait_for(
                |ask| match ask {
                    Ask::Poll => process
                        .child
                        .try_wait()
                        .map(|status| status.map(|status| status.success())),
                    Ask::Stop => {
                        process.close_console();
                        Ok(None)
                    }
                },
                timeout,
                cancel,
                std::time::Duration::from_millis(250),
            ),
        }
    }
}

/// `open` for a CLI other than Claude Code: `base` is the command for its program, `None` when it isn't installed, and
/// `spawn` starts it in a console of its own.
fn open_with(
    cli: Cli,
    base: Option<Command>,
    profile: Vec<(&'static str, std::ffi::OsString)>,
    spawn: impl FnOnce(&mut Command) -> std::io::Result<crate::contained::Contained>,
) -> Result<SignInWindow, CliSignInError> {
    let base = base.ok_or(CliSignInError::NotInstalled)?;
    prepare(&profile);
    let mut command = command_for(cli, base, profile);
    spawn(&mut command)
        .map(|process| SignInWindow(Window::Console(process)))
        .map_err(start_error)
}

/// Claude Code's sign-in for its own folder, with its environment set the way CodexBar reads it (`claude_cli`).
fn open_claude() -> Result<SignInWindow, CliSignInError> {
    crate::claude_cli::LoginWindow::open(&crate::claude::default_config_dir())
        .map(|window| SignInWindow(Window::Claude(window)))
        .map_err(|err| match err {
            crate::claude_cli::ClaudeCliError::NotInstalled => CliSignInError::NotInstalled,
            other => CliSignInError::Start(other.to_string()),
        })
}

/// `cli`'s sign-in command, from `command` (`platform::command` for the CLI's program), set up to sign in `profile`.
fn command_for(cli: Cli, mut command: Command, profile: Vec<(&'static str, std::ffi::OsString)>) -> Command {
    command.args(args(cli));
    // CodexBar's working folder, which the terminal (macOS and Linux) otherwise wouldn't start in: a relative program
    // or profile path then resolves where CodexBar's refresh resolves it.
    crate::platform::in_working_folder(&mut command);
    // The profile CodexBar reads, set on the command itself: a console started through a terminal (macOS and Linux)
    // gets only what is set here, and would otherwise sign in whatever profile the terminal points at.
    command.envs(profile);
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
    command
}

/// Makes the profile folders a sign-in names, so a first sign-in can start: the Codex CLI refuses a `CODEX_HOME` that
/// doesn't exist yet, where on its own it would make `~/.codex`. The GitHub CLI's folder is left to the GitHub CLI.
fn prepare(profile: &[(&'static str, std::ffi::OsString)]) {
    for (name, folder) in profile {
        if matches!(*name, "CODEX_HOME" | "GROK_HOME") {
            let _ = std::fs::create_dir_all(folder);
        }
    }
}

/// Why a console couldn't start.
fn start_error(err: std::io::Error) -> CliSignInError {
    match err.kind() {
        std::io::ErrorKind::NotFound => CliSignInError::NotInstalled,
        _ => CliSignInError::Start(err.to_string()),
    }
}

/// What the waiting loop asks of the running CLI.
enum Ask {
    /// Whether it has exited, and if so whether it succeeded.
    Poll,
    /// Stop it.
    Stop,
}

/// Waits for a CLI's console: Ok when it exits successfully; a failed exit, an error, cancellation or the timeout
/// (which stop it) are all a sign-in that didn't finish.
fn wait_for(
    mut cli: impl FnMut(Ask) -> std::io::Result<Option<bool>>,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
    every: std::time::Duration,
) -> Result<(), CliSignInError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) || std::time::Instant::now() >= deadline {
            let _ = cli(Ask::Stop);
            return Err(CliSignInError::Failed);
        }
        match cli(Ask::Poll) {
            Ok(Some(true)) => return Ok(()),
            Ok(Some(false)) | Err(_) => return Err(CliSignInError::Failed),
            Ok(None) => std::thread::sleep(every),
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

/// The profile the CLI signs in, as the variable naming the folder CodexBar reads, always set: `CODEX_HOME` (or
/// `~/.codex`), `GROK_HOME` (or `~/.grok`), or the GitHub CLI's `GH_CONFIG_DIR`, resolved the way the GitHub CLI
/// resolves it. A terminal's own setting then can't point the sign-in at another profile.
fn profile(cli: Cli) -> Vec<(&'static str, std::ffi::OsString)> {
    let get = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    profile_in(cli, &get, crate::platform::home_dir(), cfg!(windows))
        .into_iter()
        .map(|(name, value)| (name, absolute(value)))
        .collect()
}

/// `value` as an absolute path, against CodexBar's working folder: a relative one would otherwise resolve against
/// wherever the console's terminal starts.
fn absolute(value: std::ffi::OsString) -> std::ffi::OsString {
    std::path::absolute(PathBuf::from(&value)).map_or(value, PathBuf::into_os_string)
}

/// `profile` with the environment (`get`, which treats an empty value as unset), the home folder and the platform
/// given.
fn profile_in(
    cli: Cli,
    get: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    windows: bool,
) -> Vec<(&'static str, std::ffi::OsString)> {
    let at_home = |folder: &str| home.as_ref().map(|home| home.join(folder).into_os_string());
    let named = |name: &'static str, value: Option<std::ffi::OsString>| value.map(|value| (name, value));
    match cli {
        // GH_CONFIG_DIR, else $XDG_CONFIG_HOME/gh, else $AppData/GitHub CLI on Windows, else ~/.config/gh
        // (https://cli.github.com/manual/gh_help_environment).
        Cli::GitHub => named(
            "GH_CONFIG_DIR",
            get("GH_CONFIG_DIR")
                .or_else(|| get("XDG_CONFIG_HOME").map(|xdg| PathBuf::from(xdg).join("gh").into_os_string()))
                .or_else(|| {
                    get("AppData")
                        .filter(|_| windows)
                        .map(|app_data| PathBuf::from(app_data).join("GitHub CLI").into_os_string())
                })
                .or_else(|| at_home(".config/gh")),
        )
        .into_iter()
        .collect(),
        Cli::Codex => named("CODEX_HOME", get("CODEX_HOME").or_else(|| at_home(".codex")))
            .into_iter()
            .collect(),
        Cli::Grok => named("GROK_HOME", get("GROK_HOME").or_else(|| at_home(".grok")))
            .into_iter()
            .collect(),
        // Claude Code's folder is set by `claude_cli`.
        Cli::Claude => Vec::new(),
    }
}

/// Where the CLI is, found the way its provider finds it for refreshes, with the home folder and the PATH lookup
/// (`platform::find_program`) given, so a test reads neither (#343).
fn program(cli: Cli, home: Option<&std::path::Path>, find: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    match cli {
        Cli::GitHub => find("gh"),
        Cli::Codex => find("codex"),
        Cli::Claude => find("claude"),
        // Not `grok::cli`, whose fallback to a bare `grok` a terminal (macOS and Linux) would start and only then find
        // missing, reporting a sign-in that didn't finish rather than a CLI that isn't installed (#341).
        Cli::Grok => crate::grok::find_cli_in(home, find),
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

    /// `profile_in` over a fixed environment and home folder; never the real ones.
    fn resolve(cli: Cli, env: &[(&str, &str)], windows: bool) -> Vec<(&'static str, std::ffi::OsString)> {
        let env: std::collections::HashMap<String, String> = env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let get = |name: &str| {
            env.get(name)
                .filter(|value| !value.is_empty())
                .map(std::ffi::OsString::from)
        };
        profile_in(cli, &get, Some(PathBuf::from("home")), windows)
    }

    fn one(name: &'static str, path: PathBuf) -> Vec<(&'static str, std::ffi::OsString)> {
        vec![(name, path.into_os_string())]
    }

    #[test]
    fn the_codex_and_grok_sign_ins_name_the_folder_codexbar_reads() {
        assert_eq!(
            resolve(Cli::Codex, &[], false),
            one("CODEX_HOME", PathBuf::from("home").join(".codex"))
        );
        assert_eq!(
            resolve(Cli::Codex, &[("CODEX_HOME", "codex-home")], false),
            one("CODEX_HOME", PathBuf::from("codex-home"))
        );
        assert_eq!(
            resolve(Cli::Grok, &[], false),
            one("GROK_HOME", PathBuf::from("home").join(".grok"))
        );
        assert_eq!(
            resolve(Cli::Grok, &[("GROK_HOME", "")], false),
            one("GROK_HOME", PathBuf::from("home").join(".grok")),
            "an empty value is unset"
        );
        assert!(
            resolve(Cli::Claude, &[], false).is_empty(),
            "claude_cli sets Claude Code's folder"
        );
    }

    #[test]
    fn the_github_cli_sign_in_always_names_the_profile_the_github_cli_reads() {
        let gh = |env: &[(&str, &str)], windows| resolve(Cli::GitHub, env, windows);
        assert_eq!(
            gh(&[("GH_CONFIG_DIR", "gh-dir"), ("XDG_CONFIG_HOME", "xdg")], true),
            one("GH_CONFIG_DIR", PathBuf::from("gh-dir"))
        );
        assert_eq!(
            gh(&[("XDG_CONFIG_HOME", "xdg"), ("AppData", "roaming")], true),
            one("GH_CONFIG_DIR", PathBuf::from("xdg").join("gh"))
        );
        assert_eq!(
            gh(&[("AppData", "roaming")], true),
            one("GH_CONFIG_DIR", PathBuf::from("roaming").join("GitHub CLI"))
        );
        assert_eq!(
            gh(&[("AppData", "roaming")], false),
            one("GH_CONFIG_DIR", PathBuf::from("home").join(".config/gh")),
            "AppData counts on Windows only"
        );
    }

    #[test]
    fn a_relative_profile_is_made_absolute() {
        let made = PathBuf::from(absolute("relative-profile".into()));
        assert!(made.is_absolute());
        assert!(made.ends_with("relative-profile"));
        let already = std::env::temp_dir().join("profile").into_os_string();
        assert_eq!(absolute(already.clone()), already);
    }

    #[test]
    fn the_github_cli_sign_in_runs_in_codexbar_s_folder_with_its_profile_and_no_token() {
        let profile = vec![("GH_CONFIG_DIR", std::ffi::OsString::from("gh-profile"))];
        // A plain command, never `platform::command`, which reads the host's PATH and home folder.
        let command = command_for(Cli::GitHub, Command::new("gh"), profile);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["auth", "login", "--web", "--hostname", "github.com"]
        );
        assert_eq!(command.get_current_dir(), std::env::current_dir().ok().as_deref());
        let envs: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("GH_CONFIG_DIR")),
            Some(&Some(std::ffi::OsStr::new("gh-profile")))
        );
        for token in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ] {
            assert_eq!(envs.get(std::ffi::OsStr::new(token)), Some(&None), "{token} is removed");
        }
    }

    #[test]
    fn the_codex_sign_in_keeps_the_environment_s_tokens() {
        let command = command_for(Cli::Codex, Command::new("codex"), Vec::new());
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["login"]);
        assert!(command.get_envs().next().is_none(), "nothing set or removed");
    }

    #[test]
    fn a_first_sign_in_gets_the_codex_and_grok_folders_it_names() {
        let root = std::env::temp_dir().join(format!("codexbar-cli-prepare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let codex = root.join("codex-home");
        let grok = root.join("grok-home");
        let gh = root.join("gh-config");
        prepare(&[
            ("CODEX_HOME", codex.clone().into_os_string()),
            ("GROK_HOME", grok.clone().into_os_string()),
            ("GH_CONFIG_DIR", gh.clone().into_os_string()),
        ]);
        assert!(codex.is_dir() && grok.is_dir());
        assert!(!gh.exists(), "the GitHub CLI makes its own folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn each_cli_names_itself() {
        let names = [Cli::GitHub, Cli::Codex, Cli::Claude, Cli::Grok].map(Cli::name);
        assert_eq!(
            names,
            ["the GitHub CLI", "the Codex CLI", "Claude Code", "the Grok CLI"]
        );
    }

    #[test]
    fn a_cli_that_isnt_installed_never_starts() {
        let mut started = false;
        let result = open_with(Cli::Codex, None, Vec::new(), |_| {
            started = true;
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert!(matches!(result, Err(CliSignInError::NotInstalled)));
        assert!(!started, "nothing to start");
    }

    // SFL on #342 (#343): a bare `grok` for a missing Grok CLI would start in a terminal and only then fail, as a
    // sign-in that didn't finish.
    #[test]
    fn a_missing_cli_is_found_nowhere_rather_than_by_its_bare_name() {
        let home = std::env::temp_dir().join(format!("codexbar-cli-program-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let nowhere = |_: &str| None;
        for cli in [Cli::GitHub, Cli::Codex, Cli::Claude, Cli::Grok] {
            assert_eq!(program(cli, Some(&home), &nowhere), None, "{cli:?}");
        }
        assert_eq!(program(Cli::Grok, None, &nowhere), None, "no home folder");
        // Found on PATH, or in its installer's own folder first.
        let on_path = |name: &str| Some(PathBuf::from(format!("/bin/{name}")));
        assert_eq!(
            program(Cli::Grok, Some(&home), &on_path),
            Some(PathBuf::from("/bin/grok"))
        );
        let bin = home.join(".grok").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let installed = bin.join(&crate::platform::program_names("grok")[0]);
        std::fs::write(&installed, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(program(Cli::Grok, Some(&home), &on_path), Some(installed));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_console_that_fails_to_start_says_why_after_running_the_cli_s_own_sign_in() {
        let mut ran = Vec::new();
        let result = open_with(Cli::Codex, Some(Command::new("codex")), Vec::new(), |command| {
            ran = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert!(matches!(result, Err(CliSignInError::NotInstalled)));
        assert_eq!(ran, ["login"]);
        let result = open_with(Cli::Grok, Some(Command::new("grok")), Vec::new(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(matches!(result, Err(CliSignInError::Start(_))));
    }

    /// What `stop` found when `finish` returned.
    #[cfg(unix)]
    #[derive(Debug, PartialEq)]
    struct Stopped {
        /// The process whose id the CLI left in `$PID_FILE` still ran.
        running: bool,
        /// A process the CLI started got the terminate signal (it writes `$SIGNALLED`).
        signalled: bool,
        /// The CLI left a later process id in `$PID_FILE` than the one it started with.
        replaced: bool,
    }

    /// Whether process `pid` runs: a zombie has ended, though its parent hasn't reaped it yet. `ps` failing other than
    /// by finding no such process fails the test rather than counting as an ended process (SFL round 3 on #347).
    #[cfg(unix)]
    fn runs(pid: &str) -> bool {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .expect("ps runs");
        let state = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() || out.stderr.is_empty(),
            "ps failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.status.success() && !state.trim().is_empty() && !state.contains('Z')
    }

    /// Starts `script` as the CLI through the real waiter and terminal script (run by `sh`, no window), cancels it once
    /// it has left a process id in `$PID_FILE`, and says what `finish` left behind.
    #[cfg(unix)]
    fn stop(name: &str, script: &str) -> Stopped {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("codexbar-cli-stop-{name}-{}-{nanos}", std::process::id()));
        // A folder of this test's own: one already there, which another user could have made, fails the test.
        std::fs::create_dir(&dir).unwrap();
        let (pid_file, signalled) = (dir.join("cli"), dir.join("signalled"));
        let mut base = Command::new("/bin/sh");
        base.args(["-c", script])
            .env("PID_FILE", &pid_file)
            .env("SIGNALLED", &signalled);
        let window = open_with(
            Cli::Codex,
            Some(base),
            Vec::new(),
            crate::contained::spawn_console_without_terminal,
        )
        .unwrap();
        let started = std::time::Instant::now();
        // A whole process id: the scripts write it beside the file and rename it into place.
        let pid = loop {
            let read = std::fs::read_to_string(&pid_file).unwrap_or_default();
            if read.trim().parse::<u32>().is_ok() {
                break read.trim().to_owned();
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "the CLI started"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        // On a thread, so a stop that never returns, now that it waits for as long as the CLI is stopping (#349), fails
        // the test rather than hanging it.
        let (sent, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cancel = std::sync::atomic::AtomicBool::new(true);
            let _ = sent.send(window.finish(std::time::Duration::from_secs(60), &cancel));
        });
        let result = finished.recv_timeout(std::time::Duration::from_secs(30));
        // The CLI may have left a later process id (`a_stopped_sign_in_waits_for_what_a_wrapper_starts_as_it_exits`).
        let last = std::fs::read_to_string(&pid_file).map_or_else(|_| pid.clone(), |read| read.trim().to_owned());
        if result.is_err() {
            let _ = Command::new("kill").args(["-KILL", &pid, &last]).status();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("finish didn't return");
        }
        assert_eq!(result.unwrap(), Err(CliSignInError::Failed));
        let stopped = Stopped {
            running: runs(&last),
            signalled: signalled.exists(),
            replaced: last != pid,
        };
        let pid = last;
        if stopped.running {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
        let _ = std::fs::remove_dir_all(&dir);
        stopped
    }

    // SFL on #344 (#345): on macOS and Linux a timed-out or cancelled sign-in stopped only the terminal's waiter, and
    // the CLI ran on for up to a second, until the script's watchdog stopped it. A retry in that time signed the same
    // profile in twice.
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_returns_once_its_cli_has_stopped() {
        // A CLI that waits, as a sign-in does for the browser.
        let cli = r#"echo $$ > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#;
        assert_eq!(
            stop("cli", cli),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "the CLI has stopped when finish returns"
        );
    }

    // SFL rounds 1 and 2 on #347: what the CLI started runs on when the CLI stops: the CLI itself, when a wrapper it
    // was found as exits on the signal without passing it on, or the browser the CLI opened. `finish` waits for it, a
    // few seconds at most, and never signals it: a browser's windows are the user's.
    // SFL round 4 on #347: a wrapper that starts the CLI just as it exits on the signal leaves it to another parent before
    // the watchdog has seen it. The watchdog still waits for it: on Linux it follows the script's session, on macOS its
    // process group (#349). The wrapper must have left the successor's id, or the test proves nothing (#349).
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_waits_for_what_a_wrapper_starts_as_it_exits() {
        let wrapper = r#"echo $$ > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"
trap '( sleep 2.5 ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exit 0' TERM
n=0; while [ "$n" -lt 600 ]; do sleep 0.1; n=$((n + 1)); done"#;
        assert_eq!(
            stop("late", wrapper),
            Stopped {
                running: false,
                signalled: false,
                replaced: true
            },
            "the process the wrapper started as it exited ended before finish returned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_waits_for_what_its_cli_started_without_signalling_it() {
        // A process that ends on its own 2.5 s after it starts, and says so if it is signalled first.
        let started = r#"( trap 'echo > "$SIGNALLED"; exit 1' TERM; sleep 2.5 & wait ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#;
        assert_eq!(
            stop("started", started),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "it ended on its own before finish returned"
        );
    }

    // #349: a CLI that ignores the terminate signal is killed once it has had 3 s to end, rather than left running past
    // `finish` for a retry to overlap.
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_ends_a_cli_that_ignores_the_terminate_signal() {
        let cli = r#"trap '' TERM; echo $$ > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; n=0; while [ "$n" -lt 600 ]; do sleep 0.1; n=$((n + 1)); done"#;
        assert_eq!(
            stop("stubborn", cli),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "the CLI was ended before finish returned"
        );
    }

    // SFL round 3 on #352: a CLI that starts a session (Linux) or process group (macOS) of its own and ignores the
    // terminate signal is still the script's child: it is looked at and killed, rather than left out of the wait.
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_ends_a_cli_that_leaves_its_session() {
        let cli = r#"inner='trap "" TERM; echo $$ > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; n=0; while [ "$n" -lt 600 ]; do sleep 0.1; n=$((n + 1)); done'
if command -v setsid >/dev/null; then exec setsid /bin/sh -c "$inner"; else exec perl -e 'setpgrp(0, 0); exec @ARGV' /bin/sh -c "$inner"; fi"#;
        assert_eq!(
            stop("own-session", cli),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "the CLI was ended before finish returned"
        );
    }

    // SFL round 4 on #352: a wrapper that starts a helper in a session (Linux) or process group (macOS) of its own, and
    // stays until it is stopped, leaves that helper outside what the script's session or group holds. The helper was
    // the script's descendant when the watchdog first looked, so it is still waited for, without being signalled.
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_waits_for_a_helper_its_wrapper_started_apart() {
        let wrapper = r#"if command -v setsid >/dev/null; then setsid /bin/sh -c 'sleep 4' & else perl -e 'setpgrp(0, 0); exec @ARGV' /bin/sh -c 'sleep 4' & fi
echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"
trap 'exit 0' TERM
n=0; while [ "$n" -lt 600 ]; do sleep 0.1; n=$((n + 1)); done"#;
        assert_eq!(
            stop("apart", wrapper),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "the helper ended before finish returned"
        );
    }

    // #349: what the CLI started is waited for as long as it runs, not only for a few seconds: here it ends on its own
    // 12 s after it starts, well after the 3 s the CLI gets (which looking at what runs stretches to about 6 s on a
    // Mac) and the 6 s `finish` used to wait at most. It still isn't signalled.
    #[cfg(unix)]
    #[test]
    fn a_stopped_sign_in_waits_for_what_its_cli_started_however_long_it_runs() {
        let started = r#"( trap 'echo > "$SIGNALLED"; exit 1' TERM; sleep 12 & wait ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#;
        assert_eq!(
            stop("long", started),
            Stopped {
                running: false,
                signalled: false,
                replaced: false
            },
            "it ended on its own before finish returned"
        );
    }

    #[test]
    fn a_console_that_cant_start_says_whether_the_cli_is_missing() {
        assert_eq!(
            start_error(std::io::Error::from(std::io::ErrorKind::NotFound)),
            CliSignInError::NotInstalled
        );
        assert!(matches!(
            start_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            CliSignInError::Start(_)
        ));
    }

    /// Runs `wait_for` over the answers a CLI gives to each poll, returning its result and whether it was stopped.
    fn wait(
        answers: Vec<std::io::Result<Option<bool>>>,
        timeout: std::time::Duration,
        cancel: bool,
    ) -> (Result<(), CliSignInError>, bool) {
        let mut answers = answers.into_iter();
        let mut stopped = false;
        let cancel = std::sync::atomic::AtomicBool::new(cancel);
        let result = wait_for(
            |ask| match ask {
                Ask::Poll => answers.next().unwrap_or(Ok(None)),
                Ask::Stop => {
                    stopped = true;
                    Ok(None)
                }
            },
            timeout,
            &cancel,
            std::time::Duration::ZERO,
        );
        (result, stopped)
    }

    #[test]
    fn a_cli_that_exits_successfully_signed_in() {
        let long = std::time::Duration::from_secs(60);
        assert_eq!(wait(vec![Ok(None), Ok(Some(true))], long, false), (Ok(()), false));
    }

    #[test]
    fn a_failed_exit_or_an_error_didn_t_sign_in() {
        let long = std::time::Duration::from_secs(60);
        assert_eq!(
            wait(vec![Ok(Some(false))], long, false),
            (Err(CliSignInError::Failed), false)
        );
        assert_eq!(
            wait(vec![Err(std::io::Error::other("gone"))], long, false),
            (Err(CliSignInError::Failed), false)
        );
    }

    #[test]
    fn cancelling_or_timing_out_stops_the_cli() {
        assert_eq!(
            wait(Vec::new(), std::time::Duration::from_secs(60), true),
            (Err(CliSignInError::Failed), true)
        );
        assert_eq!(
            wait(Vec::new(), std::time::Duration::ZERO, false),
            (Err(CliSignInError::Failed), true)
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
