//! Child processes that can't outlive CodexBar: started suspended, put in a kill-on-close Windows job, then resumed,
//! so the process and anything it starts end when the job closes — when the handle is dropped, or when CodexBar
//! exits or crashes and Windows closes its handles. Used for the provider CLIs CodexBar drives (Codex, GitHub CLI).

use std::process::{Child, Command};

/// A started child in its job.
pub(crate) struct Contained {
    pub child: Child,
    #[cfg(windows)]
    _job: Option<job::Job>,
}

/// Spawns `command` with no console window, inside a kill-on-close job.
pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Contained> {
    spawn_with(command, false)
}

/// Spawns `command` in a console window of its own, for a CLI the user signs in through, inside a kill-on-close
/// job. On macOS and Linux it runs in a terminal window instead (`terminal`).
pub(crate) fn spawn_console(command: &mut Command) -> std::io::Result<Contained> {
    spawn_with(command, true)
}

fn spawn_with(command: &mut Command, console: bool) -> std::io::Result<Contained> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let window = if console { CREATE_NEW_CONSOLE } else { CREATE_NO_WINDOW };
        // Suspended until it is in its job, so nothing it starts can run outside the job.
        command.creation_flags(window | CREATE_SUSPENDED);
    }
    #[cfg(not(windows))]
    {
        let child = if console {
            terminal::spawn(command)?
        } else {
            command.spawn()?
        };
        Ok(Contained { child })
    }
    #[cfg(windows)]
    {
        let mut child = command.spawn()?;
        // Without its job the process could outlive CodexBar (and a GitHub sign-in finish unseen), so it doesn't run.
        let job = job::Job::kill_on_close(&child);
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
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    /// `text` in single quotes for `sh`.
    pub(super) fn quote(text: &std::ffi::OsStr) -> String {
        format!("'{}'", text.to_string_lossy().replace('\'', r"'\''"))
    }

    /// The script that runs `command` as configured (environment, folder, arguments) and records its exit code.
    pub(super) fn script(command: &Command, dir: &Path) -> String {
        let file = |name: &str| quote(dir.join(name).as_os_str());
        // The CLI ends with CodexBar's sign-in: once the waiter is gone (cancelled, timed out, or CodexBar quit), the
        // watchdog stops the CLI before it can finish a sign-in nobody is waiting for.
        let mut script = format!(
            "#!/bin/sh\nwaiter=$(cat {})\n( while sleep 1; kill -0 \"$waiter\" 2>/dev/null; do :; done; pkill -TERM -P $$ ) &\n\
             watchdog=$!\n",
            file("waiter")
        );
        for (name, value) in command.get_envs() {
            let name = name.to_string_lossy();
            match value {
                Some(value) => script.push_str(&format!("export {name}={}\n", quote(value))),
                None => script.push_str(&format!("unset {name}\n")),
            }
        }
        if let Some(dir) = command.get_current_dir() {
            script.push_str(&format!("cd {} || exit 1\n", quote(dir.as_os_str())));
        }
        script.push_str(&quote(command.get_program()));
        for arg in command.get_args() {
            script.push(' ');
            script.push_str(&quote(arg));
        }
        // The code is written beside the status file and renamed onto it, so the waiter never reads it half written.
        script.push_str(&format!(
            "\ncode=$?\nkill \"$watchdog\" 2>/dev/null\necho \"$code\" > {tmp} && mv {tmp} {status}\n",
            tmp = file("status.tmp"),
            status = file("status")
        ));
        script
    }

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

    pub(super) fn spawn(command: &Command) -> std::io::Result<Child> {
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
        file.write_all(script(command, &dir).as_bytes())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        // The waiter leaves its process id for the script's watchdog, and gives up when CodexBar ($4) is gone.
        let waiter = format!(
            "echo $$ > \"$3/waiter\"\n{}\nwhile [ ! -f \"$2\" ]; do kill -0 \"$4\" 2>/dev/null || exit 1; sleep 0.5; \
             done\ncode=$(cat \"$2\")\nrm -rf \"$3\"\nexit \"${{code:-1}}\"",
            launcher()
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
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::terminal::{quote, script};

    #[test]
    fn the_terminal_script_runs_the_command_as_configured() {
        let mut command = Command::new("/opt/it's/claude");
        command
            .args(["auth", "login", "--claudeai"])
            .env("CLAUDE_CONFIG_DIR", "/tmp/a b")
            .env_remove("ANTHROPIC_API_KEY")
            .current_dir("/tmp");
        let text = script(&command, Path::new("/tmp/x"));
        assert!(text.starts_with("#!/bin/sh\nwaiter=$(cat '/tmp/x/waiter')\n"));
        assert!(text.contains("export CLAUDE_CONFIG_DIR='/tmp/a b'\n"));
        assert!(text.contains("unset ANTHROPIC_API_KEY\n"));
        assert!(text.contains("cd '/tmp' || exit 1\n"));
        assert!(text.contains(r"'/opt/it'\''s/claude' 'auth' 'login' '--claudeai'"));
        assert!(text.ends_with("echo \"$code\" > '/tmp/x/status.tmp' && mv '/tmp/x/status.tmp' '/tmp/x/status'\n"));
        assert_eq!(quote(OsStr::new("a'b")), r"'a'\''b'");
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
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    use windows::core::PCWSTR;

    /// A job whose processes all end when it is closed.
    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close(child: &Child) -> Option<Self> {
            // SAFETY: no name and default security; the handle is owned by the returned Job and closed on drop.
            let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?;
            let job = Self(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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
