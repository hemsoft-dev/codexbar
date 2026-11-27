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
    /// same profile, so this waits for the script to end (#345): up to `CLI_STOP` for the watchdog to start stopping the
    /// CLI, then for as long as it is still stopping it (#349, `terminal::await_status`). A script folder already gone
    /// means the waiter read the status and deleted it: the CLI had finished. The script folder is marked cancelled
    /// first, so a CLI that ends just as the waiter is stopped is settled too (SFL round 1 on #352).
    pub(crate) fn close_console(&mut self) {
        #[cfg(not(windows))]
        terminal::mark_cancelled(self.folder.as_deref());
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(not(windows))]
        if let Some(folder) = &self.folder {
            terminal::await_status(folder, CLI_STOP);
        }
    }
}

/// How long `close_console` waits on macOS and Linux for a stopped sign-in's watchdog to show it is stopping the CLI:
/// its one-second check, with room to spare. A script that never ran (no terminal opened) has no CLI to wait for.
#[cfg(not(windows))]
const CLI_STOP: std::time::Duration = std::time::Duration::from_secs(6);

/// How recently a stopping watchdog must have renewed its `stopping` mark to count as still stopping: it renews it at
/// least every half second (`terminal::SETTLE`), and a file system that keeps whole seconds still shows a change every
/// second. A mark that stopped changing is a script that was ended before it could report.
#[cfg(not(windows))]
const STOPPING: std::time::Duration = std::time::Duration::from_secs(2);

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
        // The waiter's process id and CodexBar's (tests may write only the waiter's).
        let mut script = b"#!/bin/sh\nread -r waiter app < ".to_vec();
        script.extend(file("waiter"));
        // The code is written beside the status file and renamed onto it, so the waiter never reads it half written.
        script.extend_from_slice(b"\npublish() { echo \"$1\" > ");
        script.extend(file("status.tmp"));
        script.extend_from_slice(b" && mv ");
        script.extend(file("status.tmp"));
        script.push(b' ');
        script.extend(file("status"));
        // The last thing the script does, once its status is out and what a cancelled CLI started has ended: what
        // `close_console` waits for.
        script.extend_from_slice(b"; }\nend() { touch ");
        script.extend(file("ended"));
        script.extend_from_slice(b" 2>/dev/null");
        // Closing the terminal hangs the script up: that is a sign-in that didn't finish, reported once what the CLI
        // started has ended too (`settle`, SFL round 5 on #347). An interrupt (Ctrl+C) or terminate signal leaves the
        // terminal open, so what the CLI started is waited for as for a stopped sign-in, not for 3 s only (SFL round 2 on
        // #352). The CLI ends with CodexBar's sign-in: once the waiter is
        // gone (cancelled, timed out, or CodexBar quit), the watchdog stops the CLI before it can finish a sign-in
        // nobody is waiting for (`SETTLE`). A CLI that ended on its own as the waiter went, before the watchdog saw it
        // go, is settled by the script itself (`stopped`, #349): what it started may still be signing in. So is one that
        // ended just before the sign-in was cancelled, while its waiter still ran: `close_console` marks the folder
        // `cancel` before it stops the waiter, and the script looks for that mark once it has published (SFL round 1 on
        // #352).
        let settle_args = |script: &mut Vec<u8>, waiter: &[u8]| {
            script.extend_from_slice(b"' sh $$ ");
            script.extend_from_slice(waiter);
            script.push(b' ');
            script.extend(file("stopping"));
            script.extend_from_slice(b" \"$app\"");
        };
        script.extend_from_slice(b"; }\nsettle() { /bin/sh -c '");
        script.extend_from_slice(SETTLE);
        settle_args(&mut script, b"\"$1\"");
        script.extend_from_slice(b"; }\nstopped() { /bin/sh -c '");
        script.extend_from_slice(SETTLE);
        settle_args(&mut script, b"\"$waiter\"");
        script.extend_from_slice(
            b"; }\ntrap 'settle -; publish 129; end; exit 129' HUP\ntrap 'settle +; publish 130; end; exit 130' INT TERM\n\
              /bin/sh -c '",
        );
        script.extend_from_slice(WAIT_FOR_WAITER);
        script.extend_from_slice(SETTLE);
        settle_args(&mut script, b"\"$waiter\"");
        script.extend_from_slice(b" &\nwatchdog=$!\n");
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
        // A CLI the watchdog stopped is published once the watchdog has seen what it started end; one that ended as
        // its waiter went, once the script has. One that ended as the sign-in was cancelled is settled once it is
        // published.
        script.extend_from_slice(b"\ncode=$?\nif [ -f ");
        script.extend(file("stopping"));
        script.extend_from_slice(
            b" ]; then wait \"$watchdog\"; else kill \"$watchdog\" 2>/dev/null; kill -0 \"$waiter\" 2>/dev/null || \
              stopped; fi\npublish \"$code\"\n[ -f ",
        );
        script.extend(file("stopping"));
        script.extend_from_slice(b" ] || [ ! -f ");
        script.extend(file("cancel"));
        script.extend_from_slice(b" ] || stopped\nend\n");
        script
    }

    /// The script's watchdog, run by `sh -c` with the script's process id, the waiter's, the `stopping` file and
    /// CodexBar's process id, so `$$` is its own: it waits for the waiter to go, then settles (`SETTLE`). It outlives a
    /// closed terminal, so a CLI that ignores the hang-up is still stopped once CodexBar stops waiting for it (#349). It
    /// stops waiting once the script is no longer its parent: a script ended without its trap leaves its process id free
    /// for another process, whose children the watchdog must never signal (SFL round 2 on #352).
    const WAIT_FOR_WAITER: &[u8] = b"trap \"\" HUP
while sleep 1; [ \"$(ps -o ppid= -p $$ 2>/dev/null | tr -d \" \")\" = \"$1\" ] && kill -0 \"$2\" 2>/dev/null; do :; done
";

    /// What the watchdog does once the waiter is gone: it marks `stopping` and sends the CLI the terminate signal. Then
    /// it waits until nothing the CLI started still runs (SFL on #347): a wrapper the CLI was found as may exit on the
    /// signal without passing it on, while the CLI it started signs in. On Linux it follows the script's session, which
    /// the terminal starts it in: a process the CLI starts stays in it when the wrapper's exit leaves it to another
    /// parent, however late the wrapper started it (SFL round 4 on #347). The script's own ancestors in that session, a
    /// shell the terminal ran it through, aren't waited for. On macOS it follows the script's process group, which
    /// Terminal starts it as the leader of, the same way (#349). Where the script leads neither, it follows the script's
    /// process tree and remembers what it saw. It always follows that tree as well, so a process the CLI started in a
    /// session or group of its own while still its descendant is waited for too (SFL round 4 on #352). No process's
    /// environment is read (SFL round 7 on #347: PRIVACY.md says a
    /// cancelled sign-in never inspects the user's browser). Nothing running counts only when a second look agrees: a
    /// process can start just after one look lists what runs and before the last process it found ends, and the next
    /// look sees it. A process that leaves the script's tree and starts a session or group of its own before any look
    /// sees it leaves the wait; the script's own child, the CLI, is always in the tree, so it is still killed (SFL round
    /// 3 on #352). A zombie has ended. The mark
    /// is written by `touch`, not `:`: a special built-in's failed redirection ends a POSIX shell, and the folder may
    /// already be gone (SFL round 3 on #347).
    ///
    /// The CLI gets 3 s to end on the signal; one that ignores it is then killed (#349). Nothing else is signalled: it
    /// can be the browser the CLI opened, whose windows are the user's (SFL round 2 on #347). What the CLI started is
    /// waited for as long as it runs, renewing the mark that keeps `close_console` waiting, since a retry would overlap a
    /// sign-in still running (#349). The terminal then says so, and pressing Return there ends the wait: a browser the
    /// CLI opened holds the sign-in only until the user does. Closing the window would end it too, but a terminal sends
    /// its hang-up to everything in its foreground group, a browser the CLI started there among them, so the note
    /// doesn't suggest it (SFL round 1 on #352). The process reading Return isn't waited for, and is ended with the wait,
    /// or when the watchdog is stopped (an interrupt's trap stops it), but only while it is still the watchdog's child:
    /// one that already ended has left its process id free (SFL round 4 on #352). Without a terminal to read,
    /// as in the tests, the wait goes on. The wait also ends once CodexBar (`$4`) or the script folder is gone: nothing
    /// then waits for the status.
    ///
    /// The script's hang-up trap runs the same, without waiting for the waiter (`$2` is `-`), before it reports a closed
    /// terminal: a process the CLI started that survives the hang-up is waited for as well, for the 3 s only. Its
    /// interrupt trap (`$2` is `+`) waits as long as a stop does: the terminal is still open.
    ///
    /// It settles, and signals, only while the script (`$1`, kept as `script`: inside a function `$1` is the function's
    /// own argument) is its parent (`WAIT_FOR_WAITER`, SFL round 2 on #352).
    const SETTLE: &[u8] = b"script=$1
mine() { [ \"$(ps -o ppid= -p $$ 2>/dev/null | tr -d \" \")\" = \"$script\" ]; }
mine || exit 0
touch \"$3\" 2>/dev/null
tree() { for child in $(pgrep -P \"$1\"); do [ \"$child\" = \"$$\" ] || { echo \"$child\"; tree \"$child\"; }; done; }
running() { state=$(ps -o stat= -p \"$1\" 2>/dev/null) || return 1; case \"$state\" in *Z* | \"\") return 1 ;; esac; }
signal() { mine || return 0; for child in $(pgrep -P \"$2\"); do [ \"$child\" = \"$$\" ] || kill \"-$1\" \"$child\" 2>/dev/null; done; }
skip=\" $1 $$ \"
sid=
group=
seen=
if [ -d /proc/self ]; then
  sid=$(ps -o sid= -p \"$1\" 2>/dev/null | tr -d \" \")
  up=$1
  while up=$(ps -o ppid= -p \"$up\" 2>/dev/null | tr -d \" \") && [ -n \"$up\" ] && [ \"$up\" -gt 1 ]; do skip=\"$skip$up \"; done
else
  group=$(ps -o pgid= -p \"$1\" 2>/dev/null | tr -d \" \")
  [ \"$group\" = \"$1\" ] || group=
fi
seen=$(echo $(tree \"$script\"))
look() {
  for pid in $(tree \"$script\"); do case \" $seen \" in *\" $pid \"*) ;; *) seen=\"$seen $pid\" ;; esac; done
  pids=$seen
  if [ -n \"$sid\" ]; then
    pids=\"$pids $(ps -o pid= -s \"$sid\" 2>/dev/null)\"
  elif [ -n \"$group\" ]; then
    pids=\"$pids $(ps -A -o pid= -o pgid= 2>/dev/null | while read -r pid pg; do [ \"$pg\" != \"$group\" ] || echo \"$pid\"; done)\"
  fi
}
busy() {
  look \"$1\"
  for pid in $pids; do case \"$skip\" in *\" $pid \"*) continue ;; esac; running \"$pid\" && return 0; done
  return 1
}
quiet=0
settled() { if busy \"$1\"; then quiet=0; else quiet=$((quiet + 1)); fi; [ \"$quiet\" -ge 2 ]; }
signal TERM \"$1\"
tries=0
while [ \"$tries\" -lt 30 ]; do
  touch \"$3\" 2>/dev/null
  settled \"$1\" && exit 0
  sleep 0.1
  tries=$((tries + 1))
done
signal KILL \"$1\"
[ \"$2\" != - ] || exit 0
told=
reader=
child() { [ \"$(ps -o ppid= -p \"$1\" 2>/dev/null | tr -d \" \")\" = \"$$\" ]; }
quit() { [ -z \"$reader\" ] || ! child \"$reader\" || kill -KILL \"$reader\" 2>/dev/null; exit 0; }
trap quit TERM
while [ -d \"${3%/*}\" ] && [ ! -f \"$3.done\" ] && { [ -z \"$4\" ] || kill -0 \"$4\" 2>/dev/null; }; do
  touch \"$3\" 2>/dev/null
  settled \"$1\" && break
  if [ \"$quiet\" = 0 ] && [ -z \"$told\" ]; then
    told=1
    if true 2>/dev/null < /dev/tty; then
      echo \"CodexBar stopped this sign-in and is waiting for what it started to end. Press Return to stop waiting.\" > /dev/tty
      ( read -r line < /dev/tty && touch \"$3.done\" 2>/dev/null ) &
      reader=$!
      skip=\"$skip$reader \"
    fi
  fi
  sleep 0.5
done
quit
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

    /// Waits until the script in `folder` has ended (its `ended` mark: its CLI and what a cancelled CLI started have
    /// ended, and its status is out), or the folder is gone (the waiter read the status and deleted it). It waits up to
    /// `bound` for the watchdog to start stopping the CLI, then for as long as the watchdog keeps renewing its
    /// `stopping` mark (#349, `Stopping`): a CLI that outlasts the terminate signal, or what it started, is still
    /// stopping, however long that takes. The terminal then says the user can press Return to stop waiting.
    pub(super) fn await_status(folder: &Path, bound: std::time::Duration) {
        let ended = folder.join("ended");
        let mut stopping = Stopping::new(folder.join("stopping"), bound);
        while folder.exists() && !ended.exists() && stopping.goes_on() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Whether a stopped sign-in's watchdog is still stopping its CLI: within the bound it gets to start, or its mark
    /// changed in the last `STOPPING`. Changes are timed by this process's own clock, not the mark's time, so a change
    /// of the system clock neither ends the wait early nor holds it (SFL round 1 on #352).
    pub(super) struct Stopping {
        mark: std::path::PathBuf,
        deadline: std::time::Instant,
        seen: Option<std::time::SystemTime>,
        changed: std::time::Instant,
    }

    impl Stopping {
        pub(super) fn new(mark: std::path::PathBuf, bound: std::time::Duration) -> Self {
            let now = std::time::Instant::now();
            Self {
                mark,
                deadline: now + bound,
                seen: None,
                changed: now,
            }
        }

        /// Looks at the mark again: whether the wait goes on.
        pub(super) fn goes_on(&mut self) -> bool {
            let modified = self.mark.metadata().and_then(|meta| meta.modified()).ok();
            if modified != self.seen {
                self.seen = modified;
                self.changed = std::time::Instant::now();
            }
            std::time::Instant::now() < self.deadline
                || (self.seen.is_some() && self.changed.elapsed() < super::STOPPING)
        }
    }

    /// Marks the script folder `cancel`, if there is one, before its waiter is stopped (`Contained::close_console`).
    pub(super) fn mark_cancelled(folder: Option<&Path>) {
        if let Some(folder) = folder {
            let _ = std::fs::write(folder.join("cancel"), "");
        }
    }

    /// Starts `command` in a terminal: the waiting child, with the script folder `Contained` deletes once it has ended.
    pub(super) fn spawn(command: &Command) -> std::io::Result<super::Contained> {
        spawn_with(command, launcher())
    }

    /// The waiter's shell code, run with the script (`$1`), its status file (`$2`), its folder (`$3`) and CodexBar's process
    /// id (`$4`); `launch` starts the script without waiting for it. The waiter leaves its process id, and CodexBar's,
    /// for the script's watchdog, gives up when CodexBar is gone, and exits with the code it read. It leaves the folder:
    /// `Contained` deletes it once the waiter has ended. A waiter deleting it could race a cancel, and take the `cancel`
    /// mark away before the script settled what a CLI that ended at that moment started (SFL rounds 2 and 3 on #352).
    pub(super) fn waiter(launch: &str) -> String {
        format!(
            "echo \"$$ $4\" > \"$3/waiter\"\n{launch}\nwhile [ ! -f \"$2\" ]; do kill -0 \"$4\" 2>/dev/null || exit 1; sleep \
             0.5; done\ncode=$(cat \"$2\")\nexit \"${{code:-1}}\""
        )
    }

    /// The watchdog's shell code, for the tests.
    #[cfg(test)]
    pub(super) fn watchdog() -> Vec<u8> {
        [WAIT_FOR_WAITER, SETTLE].concat()
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
        Command::new("/bin/sh")
            .args(["-c", &waiter(launch), "sh"])
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
    // A session of its own, as a terminal gives the script (`terminal::SETTLE`), where `setsid` is found (Linux), or
    // else a process group of its own, as Terminal gives it (macOS).
    terminal::spawn_with(
        command,
        r#"if command -v setsid >/dev/null; then setsid /bin/sh "$1" </dev/null >/dev/null 2>&1 &
elif command -v perl >/dev/null; then perl -e 'setpgrp(0, 0); exec @ARGV or die' /bin/sh "$1" </dev/null >/dev/null 2>&1 &
else /bin/sh "$1" </dev/null >/dev/null 2>&1 &
fi"#,
    )
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
        // A folder of the test's own, gone as the waiter leaves it (SFL round 7 on #347).
        let gone = folder("consumed");
        std::fs::remove_dir(&gone).unwrap();
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
        assert!(text.starts_with("#!/bin/sh\nread -r waiter app < '/tmp/x/waiter'\n"));
        assert!(text.contains("export CLAUDE_CONFIG_DIR='/tmp/a b'\n"));
        assert!(text.contains("unset ANTHROPIC_API_KEY\n"));
        assert!(text.contains("cd '/tmp' || exit 1\n"));
        assert!(text.contains(r"'/opt/it'\''s/claude' 'auth' 'login' '--claudeai'"));
        assert!(
            text.contains(
                "publish() { echo \"$1\" > '/tmp/x/status.tmp' && mv '/tmp/x/status.tmp' '/tmp/x/status'; }\n"
            )
        );
        assert!(text.ends_with("] || stopped\nend\n"));
        assert!(text.contains("\npublish \"$code\"\n"));
        // The watchdog reads no process's environment (SFL round 7 on #347).
        assert!(!text.contains("environ"));
        assert_eq!(quote(OsStr::new("a'b")), br"'a'\''b'");
        // A path that isn't UTF-8 keeps its bytes.
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(quote(OsStr::from_bytes(b"/tmp/\xff")), b"'/tmp/\xff'");
    }

    /// A new folder of the test's own, which the scripts the tests run live in: one already there, which another user
    /// could have made and filled, fails the test rather than being used (SFL round 6 on #347).
    fn folder(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("codexbar-terminal-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    /// `sh` running the script at `path` in a session and process group of its own, as a terminal runs it: with `setsid`
    /// on Linux, where the watchdog follows the session; in a process group on macOS.
    fn run_script(path: &Path) -> Command {
        use std::os::unix::process::CommandExt as _;
        let mut shell = if cfg!(target_os = "linux") {
            let mut shell = Command::new("setsid");
            shell.arg("sh");
            shell
        } else {
            let mut shell = Command::new("sh");
            shell.process_group(0);
            shell
        };
        shell.arg(path);
        shell
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
        // A process group of its own, like the foreground job of a terminal.
        let mut shell = run_script(&path).spawn().unwrap();
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
    // terminal) is waited for before the closed terminal is reported, as for a cancelled sign-in: in the script's session
    // on Linux, its process group on macOS (#349), once the CLI's exit has handed it to another parent. The survivor
    // ends when the test says, so the wait is timed from that rather than from when the survivor started (#349).
    #[test]
    fn closing_the_terminal_waits_for_what_the_cli_started() {
        let dir = folder("hangup-survivor");
        let holder = folder("hangup-survivor-pid");
        std::fs::write(dir.join("waiter"), std::process::id().to_string()).unwrap();
        let (pid_file, release) = (holder.join("survivor"), holder.join("release"));
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"( trap '' HUP; n=0; while [ ! -f "$RELEASE" ] && [ "$n" -lt 400 ]; do sleep 0.05; n=$((n + 1)); done ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#,
            ])
            .env("PID_FILE", &pid_file)
            .env("RELEASE", &release);
        let path = holder.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        let mut shell = run_script(&path).spawn().unwrap();
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
        // Longer than the watchdog's one-second sleep, which ends with it: only the survivor can still hold the script.
        // Watched rather than sampled once, so a test thread scheduled late, past the 3 s, doesn't fail it (SFL round 2
        // on #352).
        let hung_up = Instant::now();
        let mut waited = true;
        while hung_up.elapsed() < Duration::from_millis(1500) {
            if shell.try_wait().unwrap().is_some() {
                waited = hung_up.elapsed() >= Duration::from_millis(2500);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(&release, "").unwrap();
        shell.wait().unwrap();
        // It ended with the survivor rather than after the 3 s (SFL round 6 on #347): timed between the release file
        // and the script's `ended` mark, so a test thread scheduled late doesn't count (SFL round 4 on #352).
        let written = |path: &Path| path.metadata().and_then(|meta| meta.modified()).unwrap();
        let ended = written(&dir.join("ended"))
            .duration_since(written(&release))
            .unwrap_or_default();
        let running = runs(&survivor);
        let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&holder);
        assert!(waited, "the closed terminal waited for the survivor");
        assert!(
            ended < Duration::from_millis(1200),
            "ended with the survivor: {ended:?}"
        );
        assert!(!running, "the survivor ended before the closed terminal was reported");
        assert_eq!(status.trim(), "129");
    }

    /// Whether the process `pid` still runs (a zombie has ended).
    fn runs(pid: &str) -> bool {
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .expect("ps runs");
        let state = String::from_utf8_lossy(&state.stdout);
        !state.trim().is_empty() && !state.contains('Z')
    }

    // SFL round 2 on #352: an interrupt (Ctrl+C) leaves the terminal open, so a process the CLI started that survives
    // it is waited for as for a stopped sign-in, past the 3 s a closed terminal gets, until it ends.
    #[test]
    fn an_interrupted_sign_in_waits_for_what_the_cli_started_until_it_ends() {
        let dir = folder("interrupt-survivor");
        let holder = folder("interrupt-survivor-pid");
        std::fs::write(dir.join("waiter"), std::process::id().to_string()).unwrap();
        let (pid_file, release) = (holder.join("survivor"), holder.join("release"));
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"( trap '' HUP INT; n=0; while [ ! -f "$RELEASE" ] && [ "$n" -lt 400 ]; do sleep 0.05; n=$((n + 1)); done ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; exec sleep 30"#,
            ])
            .env("PID_FILE", &pid_file)
            .env("RELEASE", &release);
        let path = holder.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        let mut shell = run_script(&path).spawn().unwrap();
        let waiting = Instant::now();
        while !pid_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survivor = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        // What a terminal sends its foreground job for Ctrl+C.
        assert!(
            Command::new("kill")
                .args(["-INT", "--", &format!("-{}", shell.id())])
                .status()
                .unwrap()
                .success()
        );
        let interrupted = Instant::now();
        let mut held = true;
        // Well past the 3 s a closed terminal gets, which its looks at what runs stretch to about 4 s.
        while interrupted.elapsed() < Duration::from_millis(8000) {
            if shell.try_wait().unwrap().is_some() {
                held = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::write(&release, "").unwrap();
        shell.wait().unwrap();
        let running = runs(&survivor);
        let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&holder);
        assert!(held, "still waiting for the survivor 8 s after the interrupt");
        assert!(
            !running,
            "the survivor ended before the interrupted sign-in was reported"
        );
        assert_eq!(status.trim(), "130");
    }

    // SFL rounds 2 and 3 on #352: a waiter reports the code it read and leaves the folder, cancelled or not, so a cancel
    // racing it can't take the `cancel` mark away before the script has settled; `Contained` deletes the folder once the
    // waiter has ended.
    #[test]
    fn a_waiter_reports_its_status_and_leaves_the_folder() {
        let run = |dir: &Path| {
            std::fs::write(dir.join("status"), "0").unwrap();
            Command::new("/bin/sh")
                .args(["-c", &super::terminal::waiter(":"), "sh"])
                .arg(dir.join("unused"))
                .arg(dir.join("status"))
                .arg(dir)
                .arg(std::process::id().to_string())
                .status()
                .unwrap()
                .success()
        };
        let cancelled = folder("waiter-cancelled");
        super::terminal::mark_cancelled(Some(&cancelled));
        let succeeded = run(&cancelled);
        let kept = cancelled.join("cancel").exists();
        let _ = std::fs::remove_dir_all(&cancelled);
        assert!(succeeded, "it reports the code it read");
        assert!(kept, "the cancelled sign-in's mark stays for its script");
        let finished = folder("waiter-finished");
        let succeeded = run(&finished);
        let kept = finished.exists();
        let _ = std::fs::remove_dir_all(&finished);
        assert!(
            succeeded && kept,
            "a finished sign-in's folder stays for `Contained` to delete"
        );
    }

    // SFL round 2 on #352: a watchdog whose script is no longer its parent, as after the script was ended without its
    // trap and its process id went to another process, signals nothing: here that process id is a stand-in's, whose
    // child records any terminate signal.
    #[test]
    fn a_watchdog_whose_script_is_gone_signals_nothing() {
        let dir = folder("orphan-watchdog");
        let signalled = dir.join("signalled");
        let mut stand_in = Command::new("/bin/sh")
            .args([
                "-c",
                r#"( trap 'echo > "$1"; exit 1' TERM; sleep 5 & wait ) & wait"#,
                "sh",
            ])
            .arg(&signalled)
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let mut gone = Command::new("true").spawn().unwrap();
        let gone_id = gone.id();
        gone.wait().unwrap();
        let started = Instant::now();
        let status = Command::new("/bin/sh")
            .args(["-c", &String::from_utf8(super::terminal::watchdog()).unwrap(), "sh"])
            .arg(stand_in.id().to_string())
            .arg(gone_id.to_string())
            .arg(dir.join("stopping"))
            .arg("")
            .status()
            .unwrap();
        let took = started.elapsed();
        let was_signalled = signalled.exists();
        let _ = stand_in.kill();
        let _ = stand_in.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success());
        assert!(!was_signalled, "the stand-in's child wasn't signalled");
        assert!(took < Duration::from_secs(3), "the watchdog stopped at once: {took:?}");
    }

    // #349: a CLI that ends on its own just as its waiter goes, before the watchdog's next check has seen the waiter go,
    // used to be published at once. What it started, here a process that ends on its own a second later, may still be
    // signing in: the script waits for it as the watchdog would. The watchdog can still see the waiter go first; the
    // CLI then records the terminate signal, and the run is tried again, so the script's own path is what passes (SFL
    // round 1 on #352).
    #[test]
    fn a_cli_that_ends_as_its_waiter_goes_is_published_once_what_it_started_has_ended() {
        for _ in 0..5 {
            let dir = folder("ended-first");
            let holder = folder("ended-first-pid");
            let mut waiter = Command::new("sleep").arg("30").spawn().unwrap();
            std::fs::write(dir.join("waiter"), waiter.id().to_string()).unwrap();
            let (pid_file, signalled) = (holder.join("started"), holder.join("signalled"));
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    r#"trap 'echo > "$SIGNALLED"' TERM; ( sleep 2 ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; while kill -0 "$WAITER" 2>/dev/null; do sleep 0.02; done"#,
                ])
                .env("PID_FILE", &pid_file)
                .env("SIGNALLED", &signalled)
                .env("WAITER", waiter.id().to_string());
            let path = holder.join("script.sh");
            std::fs::write(&path, script(&command, &dir)).unwrap();
            let mut shell = run_script(&path).spawn().unwrap();
            let waiting = Instant::now();
            while !pid_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let started = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
            // Still running as the waiter goes, or the test proves nothing (SFL round 3 on #352).
            assert!(runs(&started), "what the CLI started runs when its waiter goes");
            let _ = waiter.kill();
            let _ = waiter.wait();
            shell.wait().unwrap();
            let running = runs(&started);
            let watchdog_first = signalled.exists();
            let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&holder);
            if watchdog_first {
                // Let what it started end before the next run.
                std::thread::sleep(Duration::from_millis(2200));
                continue;
            }
            assert!(!running, "what the CLI started ended before its status was published");
            assert_eq!(status.trim(), "0");
            return;
        }
        panic!("the watchdog saw the waiter go first every time");
    }

    // SFL round 1 on #352: a CLI that ends just as the sign-in is cancelled, while its waiter still runs, is published at
    // once. `close_console` has marked the folder `cancel` before stopping the waiter, so the script then waits for what
    // the CLI started, here a process that ends on its own a second later, before it ends.
    #[test]
    fn a_cli_that_ends_as_the_sign_in_is_cancelled_ends_once_what_it_started_has_ended() {
        let dir = folder("ended-at-cancel");
        let holder = folder("ended-at-cancel-pid");
        let mut waiter = Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(dir.join("waiter"), waiter.id().to_string()).unwrap();
        let (pid_file, go) = (holder.join("started"), holder.join("go"));
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"( sleep 2 ) & echo $! > "$PID_FILE.tmp" && mv "$PID_FILE.tmp" "$PID_FILE"; n=0; while [ ! -f "$GO" ] && [ "$n" -lt 600 ]; do sleep 0.02; n=$((n + 1)); done"#,
            ])
            .env("PID_FILE", &pid_file)
            .env("GO", &go);
        let path = holder.join("script.sh");
        std::fs::write(&path, script(&command, &dir)).unwrap();
        let mut shell = run_script(&path).spawn().unwrap();
        let waiting = Instant::now();
        while !pid_file.exists() && waiting.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let started = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        assert!(runs(&started), "what the CLI started runs as the sign-in is cancelled");
        // `close_console`'s mark, then the CLI ends while the waiter still runs.
        super::terminal::mark_cancelled(Some(&dir));
        std::fs::write(&go, "").unwrap();
        shell.wait().unwrap();
        let running = runs(&started);
        let ended = dir.join("ended").exists();
        let _ = waiter.kill();
        let _ = waiter.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&holder);
        assert!(!running, "what the CLI started ended before the script did");
        assert!(ended, "the script marked its end");
    }

    // SFL round 1 on #352: the wait follows the `stopping` mark's changes on this process's clock, so a mark whose time
    // reads as the future, after the system clock was set back, still keeps the wait going while it changes.
    #[test]
    fn a_stopping_mark_from_the_future_still_keeps_the_wait_going() {
        let dir = folder("future-mark");
        let stopping = dir.join("stopping");
        std::fs::write(&stopping, "").unwrap();
        let renewing = {
            let stopping = stopping.clone();
            std::thread::spawn(move || {
                let ahead = std::time::SystemTime::now() + Duration::from_secs(3600);
                for step in 0..12 {
                    if let Ok(file) = std::fs::File::options().write(true).open(&stopping) {
                        let _ = file.set_modified(ahead + Duration::from_secs(step));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };
        let started = Instant::now();
        super::terminal::await_status(&dir, Duration::from_millis(200));
        let waited = started.elapsed();
        renewing.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            waited >= Duration::from_millis(1100),
            "waited while the mark changed: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(5),
            "ended once it stopped changing: {waited:?}"
        );
    }

    // #349: a watchdog still stopping the CLI renews its `stopping` mark, and the wait goes on past the bound for as long
    // as it does; a mark that is no longer renewed, a script ended before it could report, ends the wait.
    #[test]
    fn closing_a_console_waits_while_its_watchdog_is_still_stopping() {
        let dir = folder("renewed");
        let stopping = dir.join("stopping");
        std::fs::write(&stopping, "").unwrap();
        let renewing = {
            let stopping = stopping.clone();
            std::thread::spawn(move || {
                for _ in 0..12 {
                    let _ = std::fs::write(&stopping, "");
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
        };
        let started = Instant::now();
        super::terminal::await_status(&dir, Duration::from_millis(200));
        let waited = started.elapsed();
        renewing.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            waited >= Duration::from_millis(1100),
            "waited while the mark was renewed: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(5),
            "ended once it was no longer renewed: {waited:?}"
        );
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
        let status = run_script(&path).status().unwrap();
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
        let mut shell = run_script(&path).spawn().unwrap();
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
