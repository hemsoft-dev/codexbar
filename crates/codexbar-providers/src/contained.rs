//! Child processes that can't outlive CodexBar: started suspended, put in a kill-on-close Windows job, then resumed,
//! so the process and anything it starts end when the job closes — when the handle is dropped, or when CodexBar
//! exits or crashes and Windows closes its handles. Used for the provider CLIs CodexBar drives (Codex, GitHub CLI).

use std::process::{Child, Command};

#[cfg(any(windows, test))]
mod drain;
#[cfg(windows)]
mod job_drain;

/// Console and child-process policy chosen before a suspended Windows process is resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Launch {
    Hidden,
    Console,
    Probe,
}

impl Launch {
    #[cfg(any(windows, test))]
    fn windows_flags(self) -> u32 {
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let console = match self {
            Self::Hidden => CREATE_NO_WINDOW,
            Self::Console => CREATE_NEW_CONSOLE,
            Self::Probe => DETACHED_PROCESS,
        };
        console | CREATE_SUSPENDED
    }

    #[cfg(any(windows, test))]
    fn windows_job_limits(self) -> (u32, u32) {
        const KILL_ON_JOB_CLOSE: u32 = 0x2000;
        const ACTIVE_PROCESS: u32 = 0x0008;
        match self {
            Self::Probe => (KILL_ON_JOB_CLOSE | ACTIVE_PROCESS, 1),
            _ => (KILL_ON_JOB_CLOSE, 0),
        }
    }
}

/// A started child in its job.
pub(crate) struct Contained {
    pub child: Child,
    #[cfg(windows)]
    _job: Option<job::Job>,
    /// The terminal's script folder (macOS and Linux), which the waiter deletes when the CLI finishes. A waiter that
    /// was stopped (a timeout, a cancel) never gets to, so the folder goes once the waiter has ended (#341).
    #[cfg(not(windows))]
    folder: Option<std::path::PathBuf>,
}

#[cfg(not(windows))]
impl Drop for Contained {
    fn drop(&mut self) {
        // Only once the waiter is gone: a running one still waits for the status file in it.
        if let Some(folder) = &self.folder
            && matches!(self.child.try_wait(), Ok(Some(_)))
        {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

impl Contained {
    /// Ends the job's descendants and reaps the root. Unix callers stop their owned group before this call.
    pub(crate) fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        #[cfg(windows)]
        let job_result = self._job.as_ref().map_or(Ok(()), job::Job::terminate);
        let _ = self.child.kill();
        let status = self.child.wait();
        #[cfg(windows)]
        job_result?;
        status
    }

    /// Stops a console's sign-in (`spawn_console`) and returns once its CLI has stopped. On Windows ending the child
    /// ends the CLI, and the job ends what it started. On macOS and Linux the child is only the terminal's waiter: the
    /// script's watchdog notices it is gone at its next one-second check and stops the CLI, and the script writes its
    /// status file once the CLI and what it started have ended. Until then a retry would start a second CLI on the
    /// same profile, so this waits for that file, up to `CLI_STOP` (#345). A script folder already gone means the waiter
    /// read the status and deleted it: the CLI had finished.
    pub(crate) fn close_console(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(not(windows))]
        if let Some(folder) = &self.folder {
            terminal::await_status(folder, CLI_STOP);
        }
    }
}

/// How long `close_console` waits on macOS and Linux for a stopped sign-in's CLI to end: the watchdog's one-second
/// check, then the up to 3 s it waits for the CLI and what it started to end (`terminal::WATCHDOG`). A script that
/// never ran (no terminal opened) has no CLI to wait for, and a CLI that ignores the signal outlasts it.
#[cfg(not(windows))]
const CLI_STOP: std::time::Duration = std::time::Duration::from_secs(6);

/// Spawns `command` with no console window, inside a kill-on-close job.
pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Contained> {
    spawn_with(command, Launch::Hidden)
}

/// Spawns `command` in a console window of its own, for a CLI the user signs in through, inside a kill-on-close
/// job. On macOS and Linux it runs in a terminal window instead (`terminal`).
pub(crate) fn spawn_console(command: &mut Command) -> std::io::Result<Contained> {
    spawn_with(command, Launch::Console)
}

/// Start without a Windows console input device and deny child creation, including agy browser helpers.
pub(crate) fn spawn_detached(command: &mut Command) -> std::io::Result<Contained> {
    spawn_with(command, Launch::Probe)
}

fn spawn_with(command: &mut Command, launch: Launch) -> std::io::Result<Contained> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Suspended until it is in its job, so nothing it starts can run outside the job.
        command.creation_flags(launch.windows_flags());
    }
    #[cfg(not(windows))]
    {
        if launch == Launch::Console {
            return terminal::spawn(command);
        }
        let child = command.spawn()?;
        Ok(Contained { child, folder: None })
    }
    #[cfg(windows)]
    {
        let mut child = command.spawn()?;
        // Without its job the process could outlive CodexBar (and a GitHub sign-in finish unseen), so it doesn't run.
        let job = job::Job::kill_on_close(&child, launch);
        if job.is_none() || !job::resume(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("the process couldn't be started"));
        }
        Ok(Contained { child, _job: job })
    }
}

/// A CLI the user signs in through, in a terminal window of its own on macOS and Linux (#161). The command is written
/// to a script that the terminal runs and that records the CLI's exit code in a file; the returned child waits for
/// that file and exits with that code, so callers watch it as they would the CLI itself. Ending the child doesn't
/// close the terminal window.
#[cfg(not(windows))]
mod terminal {
    use std::ffi::OsStr;
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// `text` in single quotes for `sh`, byte for byte: a Unix path need not be UTF-8.
    pub(super) fn quote(text: &OsStr) -> Vec<u8> {
        let mut quoted = vec![b'\''];
        for &byte in text.as_bytes() {
            if byte == b'\'' {
                quoted.extend_from_slice(br"'\''");
            } else {
                quoted.push(byte);
            }
        }
        quoted.push(b'\'');
        quoted
    }

    /// The script that runs `command` as configured (environment, folder, arguments) and records its exit code.
    pub(super) fn script(command: &Command, dir: &Path) -> Vec<u8> {
        let file = |name: &str| quote(dir.join(name).as_os_str());
        let mut script = b"#!/bin/sh\nwaiter=$(cat ".to_vec();
        script.extend(file("waiter"));
        // The code is written beside the status file and renamed onto it, so the waiter never reads it half written.
        script.extend_from_slice(b")\npublish() { echo \"$1\" > ");
        script.extend(file("status.tmp"));
        script.extend_from_slice(b" && mv ");
        script.extend(file("status.tmp"));
        script.push(b' ');
        script.extend(file("status"));
        // Closing the terminal hangs the script up: that is a sign-in that didn't finish, reported once what the CLI
        // started has ended too (`settle`, SFL round 5 on #347). The CLI ends with CodexBar's sign-in: once the waiter is
        // gone (cancelled, timed out, or CodexBar quit), the watchdog stops the CLI before it can finish a sign-in
        // nobody is waiting for (`SETTLE`).
        // Where the watchdog reads processes' environments on Linux: `/proc`, or a test's fixture folder, which the test
        // names with `CODEXBAR_PROC`. A test never reads the host's (Codex on #347).
        let procs = command
            .get_envs()
            .find(|(name, _)| *name == "CODEXBAR_PROC")
            .and_then(|(_, value)| value)
            .unwrap_or(OsStr::new(PROCS));
        let settle_args = |script: &mut Vec<u8>, waiter: &[u8]| {
            script.extend_from_slice(b"' sh $$ ");
            script.extend_from_slice(waiter);
            script.push(b' ');
            script.extend(file("stopping"));
            script.push(b' ');
            script.extend(quote(dir.as_os_str()));
            script.push(b' ');
            script.extend(quote(procs));
        };
        script.extend_from_slice(b"; }\nsettle() { /bin/sh -c '");
        script.extend_from_slice(SETTLE);
        settle_args(&mut script, b"-");
        script.extend_from_slice(b"; }\ntrap 'settle; publish 129; exit 129' HUP INT TERM\n/bin/sh -c '");
        script.extend_from_slice(WAIT_FOR_WAITER);
        script.extend_from_slice(SETTLE);
        settle_args(&mut script, b"\"$waiter\"");
        // Everything the CLI starts inherits this mark, which the watchdog finds it by (`WATCHDOG`).
        script.extend_from_slice(b" &\nwatchdog=$!\nexport CODEXBAR_SIGN_IN=");
        script.extend(quote(dir.as_os_str()));
        script.push(b'\n');
        for (name, value) in command.get_envs() {
            match value {
                Some(value) => {
                    script.extend_from_slice(b"export ");
                    script.extend_from_slice(name.as_bytes());
                    script.push(b'=');
                    script.extend(quote(value));
                }
                None => {
                    script.extend_from_slice(b"unset ");
                    script.extend_from_slice(name.as_bytes());
                }
            }
            script.push(b'\n');
        }
        if let Some(dir) = command.get_current_dir() {
            script.extend_from_slice(b"cd ");
            script.extend(quote(dir.as_os_str()));
            script.extend_from_slice(b" || exit 1\n");
        }
        script.extend(quote(command.get_program()));
        for arg in command.get_args() {
            script.push(b' ');
            script.extend(quote(arg));
        }
        // A CLI the watchdog stopped is published once the watchdog has seen what it started end.
        script.extend_from_slice(b"\ncode=$?\nif [ -f ");
        script.extend(file("stopping"));
        script.extend_from_slice(
            b" ]; then wait \"$watchdog\"; else kill \"$watchdog\" 2>/dev/null; fi\npublish \"$code\"\n",
        );
        script
    }

    /// The processes the watchdog reads on Linux. A test build reads none unless the test names a fixture folder: tests
    /// never read the host's processes' environments (AGENTS.md, Codex on #347).
    const PROCS: &str = if cfg!(test) {
        "/nonexistent/codexbar-test-proc"
    } else {
        "/proc"
    };

    /// The script's watchdog, run by `sh -c` with the script's process id, the waiter's, the `stopping` file, the
    /// script folder and where processes are read (`PROCS`), so `$$` is its own: it waits for the waiter to go, then
    /// settles (`SETTLE`).
    const WAIT_FOR_WAITER: &[u8] = b"while sleep 1; kill -0 \"$2\" 2>/dev/null; do :; done\n";

    /// What the watchdog does once the waiter is gone: it marks `stopping` and sends the CLI the terminate
    /// signal. Then it waits, up to 3 s, until nothing the CLI started still runs (SFL on #347): a wrapper the CLI was
    /// found as may exit on the signal without passing it on, while the CLI it started signs in. On Linux it finds those
    /// processes in `/proc` by the `CODEXBAR_SIGN_IN` mark the script exports, which they inherit and keep when the
    /// wrapper's exit leaves them to another parent, however late the wrapper started them (SFL round 4 on #347). macOS
    /// shows no other process's environment, so there it follows the script's process tree and remembers what it saw:
    /// a process the wrapper starts and leaves between two looks isn't seen. Nothing running counts only when a second
    /// look a tenth of a second later agrees: a process can start just after one look lists what runs and before the
    /// last process it found ends, and the next look sees it. A process that clears its environment isn't found on
    /// Linux. It never signals them: one can be the browser the CLI opened, whose windows are the user's (SFL round 2 on
    /// #347). A zombie has ended. One still running after the 3 s is left to end on its own: waiting on would hold the
    /// sign-in for as long as a browser the CLI opened stays open. The mark is written by `true`, not `:`: a special
    /// built-in's failed redirection ends a POSIX shell, and the folder may already be gone (SFL round 3 on #347).
    ///
    /// The script's hang-up trap runs the same, without waiting for the waiter (`$2` is `-`), before it reports a closed
    /// terminal: a process the CLI started that survives the hang-up is waited for as well.
    const SETTLE: &[u8] = b"true 2>/dev/null > \"$3\"
mark=\"CODEXBAR_SIGN_IN=$4\"
procs=\"$5\"
tree() { for child in $(pgrep -P \"$1\"); do [ \"$child\" = \"$$\" ] || { echo \"$child\"; tree \"$child\"; }; done; }
seen=$(echo $(tree \"$1\"))
look() {
  if [ -d /proc/self ]; then
    pids=$(grep -lsxzF \"$mark\" \"$procs\"/[0-9]*/environ | sed -e \"s|^$procs/||\" -e \"s|/environ\\$||\")
  else
    for pid in $(tree \"$1\"); do case \" $seen \" in *\" $pid \"*) ;; *) seen=\"$seen $pid\" ;; esac; done
    pids=$seen
  fi
}
running() { state=$(ps -o stat= -p \"$1\" 2>/dev/null) || return 1; case \"$state\" in *Z* | \"\") return 1 ;; esac; }
for child in $(pgrep -P \"$1\"); do [ \"$child\" = \"$$\" ] || kill -TERM \"$child\" 2>/dev/null; done
tries=0
quiet=0
while [ \"$tries\" -lt 30 ]; do
  look \"$1\"
  busy=
  for pid in $pids; do [ \"$pid\" = \"$1\" ] || { running \"$pid\" && busy=1 && break; }; done
  if [ -n \"$busy\" ]; then quiet=0; else quiet=$((quiet + 1)); fi
  [ \"$quiet\" -lt 2 ] || break
  sleep 0.1
  tries=$((tries + 1))
done
";

    /// Opens the script in Terminal (macOS) or the first terminal emulator found (Linux).
    fn launcher() -> &'static str {
        if cfg!(target_os = "macos") {
            r#"/usr/bin/open -a Terminal "$1""#
        } else {
            r#"if command -v x-terminal-emulator >/dev/null; then x-terminal-emulator -e "$1" &
elif command -v gnome-terminal >/dev/null; then gnome-terminal -- "$1" &
elif command -v konsole >/dev/null; then konsole -e "$1" &
elif command -v xfce4-terminal >/dev/null; then xfce4-terminal -e "$1" &
elif command -v kitty >/dev/null; then kitty "$1" &
elif command -v alacritty >/dev/null; then alacritty -e "$1" &
elif command -v xterm >/dev/null; then xterm -e "$1" &
else exit 127; fi"#
        }
    }

    /// Waits until the script in `folder` has written its status file (its CLI has ended), up to `bound`, or the folder
    /// is gone (the waiter read the status and deleted it).
    pub(super) fn await_status(folder: &Path, bound: std::time::Duration) {
        let status = folder.join("status");
        let deadline = std::time::Instant::now() + bound;
        while folder.exists() && !status.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Starts `command` in a terminal: the waiting child, with the script folder it deletes when the CLI finishes.
    pub(super) fn spawn(command: &Command) -> std::io::Result<super::Contained> {
        spawn_with(command, launcher())
    }

    /// `spawn`, with `launch` the shell code that starts the script (`$1`) without waiting for it.
    pub(super) fn spawn_with(command: &Command, launch: &str) -> std::io::Result<super::Contained> {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(|_| std::io::Error::other("no random name for the script folder"))?;
        let suffix: String = name.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("codexbar-terminal-{suffix}"));
        std::fs::create_dir(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        // Terminal opens `.command` files; other terminals run any executable.
        let path = dir.join("sign-in.command");
        let status = dir.join("status");
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&script(command, &dir))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        // The waiter leaves its process id for the script's watchdog, and gives up when CodexBar ($4) is gone.
        let waiter = format!(
            "echo $$ > \"$3/waiter\"\n{}\nwhile [ ! -f \"$2\" ]; do kill -0 \"$4\" 2>/dev/null || exit 1; sleep 0.5; \
             done\ncode=$(cat \"$2\")\nrm -rf \"$3\"\nexit \"${{code:-1}}\"",
            launch
        );
        Command::new("/bin/sh")
            .args(["-c", &waiter, "sh"])
            .arg(&path)
            .arg(&status)
            .arg(&dir)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|child| super::Contained {
                child,
                folder: Some(dir.clone()),
            })
    }
}

/// `spawn_console` with the script run by `sh` in the background rather than in a terminal window, for tests on macOS
/// and Linux: no window opens.
#[cfg(all(test, not(windows)))]
pub(crate) fn spawn_console_without_terminal(command: &mut Command) -> std::io::Result<Contained> {
    terminal::spawn_with(command, r#"/bin/sh "$1" </dev/null >/dev/null 2>&1 &"#)
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::terminal::{quote, script};

    // SFL on #340: a timed-out sign-in killed its waiter before the waiter deleted the script folder.
    #[test]
    fn a_stopped_waiter_s_script_folder_goes_once_the_waiter_has_ended() {
        let root = std::env::temp_dir().join(format!("codexbar-terminal-drop-{}", std::process::id()));
        let (running, stopped) = (root.join("running"), root.join("stopped"));
        std::fs::create_dir_all(&running).unwrap();
        std::fs::create_dir_all(&stopped).unwrap();
        let sleeper = || Command::new("/bin/sh").args(["-c", "sleep 30"]).spawn().unwrap();

        let mut ended = super::Contained {
            child: sleeper(),
            folder: Some(stopped.clone()),
        };
        let _ = ended.child.kill();
        let _ = ended.child.wait();
        drop(ended);
        assert!(!stopped.exists(), "the stopped waiter's folder is deleted");

        let waiting = super::Contained {
            child: sleeper(),
            folder: Some(running.clone()),
        };
        let pid = waiting.child.id().to_string();
        drop(waiting);
        assert!(running.exists(), "a waiter still running keeps its folder");
        let _ = Command::new("kill").arg(&pid).status();
        let _ = std::fs::remove_dir_all(&root);
    }

    // SFL on #347: a waiter that read the status and deleted its folder just before the sign-in was stopped had a
    // finished CLI; the wait for its status file ends at once rather than after `CLI_STOP`.
    #[test]
    fn a_consumed_status_ends_the_wait_at_once() {
        let gone = std::env::temp_dir().join(format!("codexbar-terminal-consumed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        let started = Instant::now();
        super::terminal::await_status(&gone, Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // A console with no script folder (a CLI not started through a terminal) stops at once. A folder whose script never
    // writes its status, as when no terminal ran it, is waited for only up to the bound.
    #[test]
    fn closing_a_console_waits_only_as_long_as_its_script_can_report() {
        let mut plain = super::Contained {
            child: Command::new("sleep").arg("30").spawn().unwrap(),
            folder: None,
        };
        let started = Instant::now();
        plain.close_console();
        assert!(started.elapsed() < Duration::from_secs(1), "nothing to wait for");
        let silent = folder("silent");
        let started = Instant::now();
        super::terminal::await_status(&silent, Duration::from_millis(200));
        assert!(started.elapsed() >= Duration::from_millis(200), "waited the bound");
        let _ = std::fs::remove_dir_all(&silent);
    }

    #[test]
    fn the_terminal_script_runs_the_command_as_configured() {
        let mut command = Command::new("/opt/it's/claude");
        command
            .args(["auth", "login", "--claudeai"])
            .env("CLAUDE_CONFIG_DIR", "/tmp/a b")
            .env_remove("ANTHROPIC_API_KEY")
            .current_dir("/tmp");
        let text = String::from_utf8(script(&command, Path::new("/tmp/x"))).unwrap();
        assert!(text.starts_with("#!/bin/sh\nwaiter=$(cat '/tmp/x/waiter')\n"));
        assert!(text.contains("export CLAUDE_CONFIG_DIR='/tmp/a b'\n"));
        assert!(text.contains("unset ANTHROPIC_API_KEY\n"));
        assert!(text.contains("cd '/tmp' || exit 1\n"));
        assert!(text.contains(r"'/opt/it'\''s/claude' 'auth' 'login' '--claudeai'"));
        assert!(
            text.contains(
                "publish() { echo \"$1\" > '/tmp/x/status.tmp' && mv '/tmp/x/status.tmp' '/tmp/x/status'; }\n"
            )
        );
        assert!(text.ends_with("publish \"$code\"\n"));
        // A test's watchdog reads no process environments unless the test names its own fixture folder (Codex on #347).
        assert!(text.contains(" '/nonexistent/codexbar-test-proc' &\n"));
        assert_eq!(quote(OsStr::new("a'b")), br"'a'\''b'");
        // A path that isn't UTF-8 keeps its bytes.
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(quote(OsStr::from_bytes(b"/tmp/\xff")), b"'/tmp/\xff'");
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("codexbar-terminal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_script_reports_the_command_exit_code_through_sh() {
        let dir = folder("code");
        // A waiter that is alive for the whole run: this test process.
        std::fs::write(dir.join("waiter"), std::process::id().to_string()).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "exit 3"]);
        let path = dir.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        assert!(Command::new("sh").arg(&path).status().unwrap().success());
        assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap().trim(), "3");
        assert!(!dir.join("status.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_the_terminal_reports_a_failed_sign_in() {
        let dir = folder("hangup");
        std::fs::write(dir.join("waiter"), std::process::id().to_string()).unwrap();
        let started_file = dir.join("started");
        let mut command = Command::new("sh");
        command
            .args(["-c", r#"echo > "$STARTED"; exec sleep 30"#])
            .env("STARTED", &started_file);
        let path = dir.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        use std::os::unix::process::CommandExt as _;
        // A process group of its own, like the foreground job of a terminal.
        let mut shell = Command::new("sh").arg(&path).process_group(0).spawn().unwrap();
        // Only once the CLI runs: a hang-up sent while the script is still starting it misses the CLI, and the script
        // then waits out the CLI's 30 s before its trap runs (seen on a loaded Linux machine).
        let waiting = Instant::now();
        while !started_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // What a terminal sends its foreground job when its window closes.
        let group = format!("-{}", shell.id());
        assert!(
            Command::new("kill")
                .args(["-HUP", "--", &group])
                .status()
                .unwrap()
                .success()
        );
        let started = Instant::now();
        shell.wait().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the hang-up ended the script"
        );
        assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap().trim(), "129");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // SFL round 5 on #347: a process the CLI started that survives the terminal's hang-up (it ignores it, or left the
    // terminal) is waited for before the closed terminal is reported, as for a cancelled sign-in. Linux finds it by the
    // script's mark once the CLI's exit has handed it to another parent; macOS can't see that.
    #[cfg(target_os = "linux")]
    #[test]
    fn closing_the_terminal_waits_for_what_the_cli_started() {
        let dir = folder("hangup-survivor");
        let holder = folder("hangup-survivor-pid");
        std::fs::write(dir.join("waiter"), std::process::id().to_string()).unwrap();
        let pid_file = holder.join("survivor");
        // The survivor's entry in the test's own process folder, as `/proc` would show it (Codex on #347).
        let procs = holder.join("proc");
        std::fs::create_dir(&procs).unwrap();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"( trap '' HUP; sleep 2 ) & p=$!
mkdir -p "$CODEXBAR_PROC/$p" && printf 'CODEXBAR_SIGN_IN=%s\000' "$CODEXBAR_SIGN_IN" > "$CODEXBAR_PROC/$p/environ"
echo $p > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#,
            ])
            .env("PID_FILE", &pid_file)
            .env("CODEXBAR_PROC", &procs);
        let path = holder.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        use std::os::unix::process::CommandExt as _;
        let mut shell = Command::new("sh").arg(&path).process_group(0).spawn().unwrap();
        let waiting = Instant::now();
        while !pid_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survivor = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        assert!(
            Command::new("kill")
                .args(["-HUP", "--", &format!("-{}", shell.id())])
                .status()
                .unwrap()
                .success()
        );
        shell.wait().unwrap();
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &survivor])
            .output()
            .expect("ps runs");
        let state = String::from_utf8_lossy(&state.stdout);
        let running = !state.trim().is_empty() && !state.contains('Z');
        assert_eq!(std::fs::read_to_string(dir.join("status")).unwrap().trim(), "129");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&holder);
        assert!(!running, "the survivor ended before the closed terminal was reported");
    }

    #[test]
    fn the_cli_stops_when_its_waiter_is_gone() {
        let dir = folder("watchdog");
        // A waiter that ends at once, as when a sign-in is cancelled.
        let mut gone = Command::new("true").spawn().unwrap();
        let gone_id = gone.id();
        gone.wait().unwrap();
        std::fs::write(dir.join("waiter"), gone_id.to_string()).unwrap();
        let mut command = Command::new("sleep");
        command.arg("30");
        let path = dir.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        let started = Instant::now();
        let status = Command::new("sh").arg(&path).status().unwrap();
        assert!(status.success(), "the script itself finishes");
        assert!(started.elapsed() < Duration::from_secs(10), "the 30 s CLI was stopped");
        assert_ne!(std::fs::read_to_string(dir.join("status")).unwrap().trim(), "0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // SFL rounds 3 and 4 on #347: a Settings sign-in that is cancelled deletes the script folder once its waiter has
    // ended, which can be before the watchdog wakes. The watchdog still stops the CLI: its `stopping` mark can't be
    // written, and a failed redirection must not end it. The folder goes while the CLI runs and the waiter is still
    // alive, so the watchdog can't have written the mark first.
    #[test]
    fn the_cli_stops_when_its_script_folder_went_first() {
        let dir = folder("folder-gone");
        let holder = folder("folder-gone-script");
        let mut waiter = Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(dir.join("waiter"), waiter.id().to_string()).unwrap();
        let started_file = holder.join("started");
        let mut command = Command::new("sh");
        command
            .args(["-c", r#"echo > "$STARTED"; exec sleep 30"#])
            .env("STARTED", &started_file);
        let path = holder.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        let mut shell = Command::new("sh").arg(&path).spawn().unwrap();
        let waiting = Instant::now();
        while !started_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // SFL round 5 on #347: only a CLI that runs proves its stop.
        assert!(started_file.exists(), "the CLI started");
        std::fs::remove_dir_all(&dir).unwrap();
        let _ = waiter.kill();
        let _ = waiter.wait();
        let stopping = Instant::now();
        shell.wait().unwrap();
        assert!(stopping.elapsed() < Duration::from_secs(10), "the 30 s CLI was stopped");
        let _ = std::fs::remove_dir_all(&holder);
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::Child;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    use windows::core::PCWSTR;

    /// A job whose processes all end when it is closed.
    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close(child: &Child, launch: super::Launch) -> Option<Self> {
            // SAFETY: no name and default security; the handle is owned by the returned Job and closed on drop.
            let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?;
            let job = Self(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            let (flags, limit) = launch.windows_job_limits();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(flags);
            info.BasicLimitInformation.ActiveProcessLimit = limit;
            // SAFETY: `info` is the structure this information class expects, with its exact size.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            }
            .ok()?;
            // SAFETY: the child's handle is valid while `child` lives, which outlasts this call.
            unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) }.ok()?;
            Some(job)
        }
    }

    impl Job {
        /// Stop and wait for verified job members, rescanning for descendants created during cleanup.
        pub fn terminate(&self) -> std::io::Result<()> {
            super::job_drain::terminate(self.0)
        }
    }

    /// Resumes the threads of a process started suspended (it has one). False if none could be resumed.
    pub fn resume(pid: u32) -> bool {
        // SAFETY: a snapshot of all threads; the handle is closed below.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
            return false;
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut resumed = false;
        // SAFETY: `entry` is a THREADENTRY32 with its size set, as both calls require.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) }.is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the thread handle is used for this one call and closed.
                if let Ok(thread) = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) } {
                    resumed |= unsafe { ResumeThread(thread) } != u32::MAX;
                    let _ = unsafe { CloseHandle(thread) };
                }
            }
            // SAFETY: as above.
            more = unsafe { Thread32Next(snapshot, &mut entry) }.is_ok();
        }
        // SAFETY: the snapshot handle came from CreateToolhelp32Snapshot and is closed once.
        let _ = unsafe { CloseHandle(snapshot) };
        resumed
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod launch_tests {
    use super::Launch;

    #[test]
    fn only_background_probes_are_detached_and_deny_children() {
        for (launch, flags, limits) in [
            (Launch::Hidden, 0x0800_0004, (0x2000, 0)),
            (Launch::Console, 0x0000_0014, (0x2000, 0)),
            (Launch::Probe, 0x0000_000c, (0x2008, 1)),
        ] {
            assert_eq!(launch.windows_flags(), flags);
            assert_eq!(launch.windows_job_limits(), limits);
        }
    }
}
