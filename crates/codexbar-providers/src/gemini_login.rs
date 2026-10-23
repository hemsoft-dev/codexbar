//! Antigravity owns Google consent, the default browser and its shared native-keyring credentials. Launch only
//! its interactive CLI, without a model prompt. Completion is verified separately through the safe /usage command.

use std::path::Path;
use std::process::{Child, Command};

pub struct LoginWindow {
    #[cfg(windows)]
    process: Child,
    #[cfg(not(windows))]
    process: crate::contained::Contained,
}

fn command(dir: &Path) -> Command {
    let mut command = crate::platform::command(crate::gemini_coding::cli());
    command.current_dir(dir);
    // A desktop app launched by an SSH qualification harness must still use the vendor's local browser flow.
    for name in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
        command.env_remove(name);
    }
    command
}

impl LoginWindow {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let mut command = command(dir);
        #[cfg(windows)]
        let process = {
            use std::os::windows::process::CommandExt as _;
            // Own only the CLI root. A kill-on-close job would also capture the user's default browser, whose
            // existing windows and sessions must survive cancellation. No browser is controlled or inspected.
            command.creation_flags(0x0000_0010);
            command.spawn()
        };
        #[cfg(not(windows))]
        let process = crate::contained::spawn_console(&mut command);
        process.map(|process| Self { process }).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "Install the Antigravity CLI (agy), then try again.".to_owned(),
            _ => "The Antigravity CLI window couldn't be opened. Run agy in a terminal, then check the connection."
                .to_owned(),
        })
    }

    fn child(&mut self) -> &mut Child {
        #[cfg(windows)]
        {
            &mut self.process
        }
        #[cfg(not(windows))]
        {
            &mut self.process.child
        }
    }
}

impl Drop for LoginWindow {
    fn drop(&mut self) {
        stop(self.child());
    }
}

fn stop(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_owned_cli_reaps_it_and_keeps_other_processes_running() {
        fn sleeper() -> Child {
            #[cfg(windows)]
            let mut command = {
                use std::os::windows::process::CommandExt as _;
                let mut command = Command::new("powershell.exe");
                command
                    .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
                    .creation_flags(0x0800_0000);
                command
            };
            #[cfg(not(windows))]
            let mut command = {
                let mut command = Command::new("sh");
                command.args(["-c", "exec sleep 60"]);
                command
            };
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        }
        let mut owned = sleeper();
        let mut unrelated = sleeper();
        assert!(owned.try_wait().unwrap().is_none());
        stop(&mut owned);
        let reaped = owned.try_wait().unwrap().is_some();
        let unrelated_alive = unrelated.try_wait().unwrap().is_none();
        stop(&mut unrelated);
        assert!(reaped, "the owned CLI is reaped");
        assert!(unrelated_alive, "unrelated browser processes must remain alone");
    }

    #[test]
    fn native_login_has_no_prompt_browser_control_or_ssh_oauth_mode() {
        let dir = std::env::temp_dir();
        let command = command(&dir);
        assert_eq!(command.get_current_dir(), Some(dir.as_path()));
        assert_eq!(command.get_args().count(), 0, "no prompt or model turn is submitted");
        let removed: Vec<_> = command
            .get_envs()
            .filter_map(|(name, value)| value.is_none().then_some(name))
            .collect();
        for name in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
            assert!(removed.contains(&std::ffi::OsStr::new(name)));
        }
        assert!(
            !command.get_envs().any(|(key, _)| ["HOME", "USERPROFILE", "AGY_HOME"]
                .iter()
                .any(|name| key == std::ffi::OsStr::new(name))),
            "use the vendor's existing local sign-in"
        );
    }
}
