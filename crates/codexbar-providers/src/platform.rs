//! Where things are on each platform (#161): the user's home folder, the provider CLIs and the default browser.

use std::path::{Path, PathBuf};

/// The user's home folder: `%USERPROFILE%` on Windows, `$HOME` elsewhere.
pub fn home_dir() -> Option<PathBuf> {
    codexbar_core::home_dir()
}

/// The file names a CLI called `name` installs as: `name.exe` (a native install), then `name.cmd` (npm) on Windows;
/// `name` elsewhere.
pub fn program_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_owned()]
    }
}

/// Folders searched after PATH, where CLI installers put programs. A macOS app started from Finder or at login gets
/// only the system's PATH, not the one the user's shell builds, so these are where its CLIs are found.
fn install_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut dirs: Vec<PathBuf> = home_dir()
        .map(|home| vec![home.join(".local").join("bin"), home.join("bin")])
        .unwrap_or_default();
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/home/linuxbrew/.linuxbrew/bin"]
            .into_iter()
            .map(PathBuf::from),
    );
    dirs
}

/// The CLI called `name`: on PATH, then in the usual install folders.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(install_dirs());
    find_in(name, &dirs)
}

fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names = program_names(name);
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| runnable(candidate))
}

/// A file the system would run: on macOS and Linux it must also be executable, as `execvp` skips one that isn't.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

/// The CLI called `name` in `dir` (an installer's own folder), if it is there.
pub fn program_in(dir: &Path, name: &str) -> Option<PathBuf> {
    find_in(name, &[dir.to_owned()])
}

/// A command for the CLI `program`, a path or a bare name. On macOS and Linux a bare name is looked up like
/// `find_program`, and the CLI's PATH gains its own folder and the usual install folders: an npm-installed CLI is a
/// `#!/usr/bin/env node` script, and a macOS app's PATH doesn't reach `node` (#161). Windows starts it as before.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let program = Path::new(program.as_ref());
    #[cfg(windows)]
    {
        std::process::Command::new(program)
    }
    #[cfg(not(windows))]
    {
        let bare = program.components().count() == 1;
        let resolved = bare
            .then(|| find_program(&program.to_string_lossy()))
            .flatten()
            .unwrap_or_else(|| program.to_owned());
        let mut command = std::process::Command::new(&resolved);
        if let Some(path) = child_path(resolved.parent()) {
            command.env("PATH", path);
        }
        command
    }
}

/// PATH for a CLI started from `dir`: that folder, this process's PATH, then the usual install folders, each once.
#[cfg(not(windows))]
fn child_path(dir: Option<&Path>) -> Option<std::ffi::OsString> {
    let mut dirs: Vec<PathBuf> = dir
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(Path::to_owned)
        .into_iter()
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(install_dirs());
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    std::env::join_paths(dirs).ok()
}

/// Opens `url` in the default browser. No shell parses the URL: Windows' URL protocol handler, `open` on macOS,
/// `xdg-open` on Linux.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg(url);
        command
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program file the system would run.
    fn put(path: &Path) {
        std::fs::write(path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn a_program_is_found_by_its_platform_name_in_the_first_folder_that_has_it() {
        let root = std::env::temp_dir().join(format!("codexbar-platform-{}", std::process::id()));
        let (first, second) = (root.join("first"), root.join("second"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let name = program_names("tool").pop().unwrap();
        put(&second.join(&name));
        assert_eq!(
            find_in("tool", &[first.clone(), second.clone()]),
            Some(second.join(&name))
        );
        put(&first.join(&name));
        assert_eq!(program_in(&first, "tool"), Some(first.join(&name)));
        assert_eq!(find_in("missing", &[first, second]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_isnt_executable_is_passed_over() {
        let root = std::env::temp_dir().join(format!("codexbar-platform-x-{}", std::process::id()));
        let (first, second) = (root.join("first"), root.join("second"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("tool"), "").unwrap();
        put(&second.join("tool"));
        assert_eq!(find_in("tool", &[first, second.clone()]), Some(second.join("tool")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_tries_the_native_install_before_npm() {
        let names = program_names("claude");
        if cfg!(windows) {
            assert_eq!(names, ["claude.exe", "claude.cmd"]);
        } else {
            assert_eq!(names, ["claude"]);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn a_cli_runs_with_its_own_folder_and_the_install_folders_on_path() {
        let command = command("/opt/tools/bin/codex");
        let path = command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value)
            .map(std::env::split_paths)
            .map(Iterator::collect::<Vec<_>>)
            .unwrap();
        assert_eq!(path[0], PathBuf::from("/opt/tools/bin"));
        assert!(path.contains(&PathBuf::from("/opt/homebrew/bin")));
        let unique: std::collections::HashSet<_> = path.iter().collect();
        assert_eq!(unique.len(), path.len(), "each folder once");
    }

    #[cfg(not(windows))]
    #[test]
    fn the_usual_install_folders_follow_path() {
        let dirs = install_dirs();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }
}
