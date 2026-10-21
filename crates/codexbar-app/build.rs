//! Embeds CodexBar's mark as the Windows executable's icon resource 1 (#182). GPUI's Windows backend loads that
//! resource for its window class, so the taskbar, Alt-Tab, Explorer and shortcuts show the mark instead of the generic
//! executable icon. The other platforms take their icons from the package (`package-macos.sh`, `package-linux.sh`).
//!
//! It also works out the version the title bar shows (#244), as `CODEXBAR_VERSION`.

#[path = "src/mark.rs"]
mod mark;

#[path = "src/version_scheme.rs"]
mod version_scheme;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/mark.rs");
    println!("cargo:rerun-if-changed=src/version_scheme.rs");
    stamp_version();
    for name in rc_override_names() {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_icon();
    }
}

#[cfg(windows)]
fn embed_windows_icon() {
    use std::path::PathBuf;

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ico = out.join("codexbar.ico");
    std::fs::write(&ico, mark::ico(&mark::ICON_SIZES)).expect("write the icon next to the build output");
    // Resource scripts read C-style escapes, so the path's backslashes are doubled.
    let script = out.join("codexbar.rc");
    let quoted = ico.display().to_string().replace('\\', "\\\\");
    std::fs::write(&script, format!("1 ICON \"{quoted}\"\n")).expect("write the resource script");
    // embed-resource's own search for rc.exe depends on registry keys that are stale on some machines (#184), so for
    // an MSVC target the usual places are tried here first and the find is handed over through the RC variable it
    // honours. When they hold nothing, embed-resource still gets to follow the SDK's registration and Visual Studio's
    // discovery, which cover SDKs installed elsewhere. A GNU target compiles its resources with windres, which
    // embed-resource finds itself.
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let (rc, searched) = match msvc.then(find_rc) {
        Some(Ok(rc)) => {
            // SAFETY: the build script is single-threaded here; nothing reads the environment concurrently.
            unsafe { std::env::set_var("RC", &rc) };
            (rc.display().to_string(), None)
        }
        Some(Err(searched)) => ("embed-resource's own search".to_owned(), Some(searched)),
        None => ("embed-resource's own search".to_owned(), None),
    };
    // A build whose resource compiler fails stops here rather than shipping without the icon.
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|err| {
            let looked = searched
                .map(|searched| format!(" Looked for: {searched}."))
                .unwrap_or_default();
            panic!(
                "compile the Windows icon resource with {rc}: {err}. rc.exe comes with the Windows SDK (the Visual \
                 Studio Build Tools' \"Desktop development with C++\" workload); set RC to its path.{looked}"
            )
        });
}

/// Sets `CODEXBAR_VERSION`: the version `package.ps1` passes in, else the package scheme at this commit, so a build from
/// source shows the version `package.ps1` would give the same commit. Without git (a source archive) the commit count
/// is left out; the build doesn't fail over it.
fn stamp_version() {
    println!("cargo:rerun-if-env-changed=CODEXBAR_VERSION");
    let (version, source) = match std::env::var("CODEXBAR_VERSION") {
        Ok(version) if !version.trim().is_empty() => (version.trim().to_owned(), "stamped"),
        _ => {
            let commits = git(&["rev-list", "--count", "HEAD"]).and_then(|count| count.parse().ok());
            let cargo = std::env::var("CARGO_PKG_VERSION").expect("cargo sets it");
            let source = if commits.is_some() { "git" } else { "no-git" };
            (version_scheme::package_style(&cargo, commits), source)
        }
    };
    println!("cargo:rustc-env=CODEXBAR_VERSION={version}");
    // Where the version came from, so the app's tests can check it exactly.
    println!("cargo:rustc-env=CODEXBAR_VERSION_SOURCE={source}");
    watch_git_head();
}

/// Runs git in the crate's folder; `None` when git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Reruns the build script when a commit, checkout or pull moves HEAD, so the commit count stays current. HEAD's reflog
/// changes with every move; HEAD itself and the branch's ref cover a repository without reflogs.
fn watch_git_head() {
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let git_dir = std::path::PathBuf::from(git_dir);
    for path in [git_dir.join("HEAD"), git_dir.join("logs").join("HEAD")] {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    // A linked worktree keeps its branches in the main repository's folder.
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).map(std::path::PathBuf::from);
    if let (Some(common), Some(branch)) = (common, git(&["symbolic-ref", "-q", "HEAD"])) {
        for path in [common.join(branch), common.join("packed-refs")] {
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

/// The environment variables that name a resource compiler, as embed-resource defines them and in its order of
/// precedence: `RC_<target>`, the same with underscores, then `RC`.
fn rc_override_names() -> [String; 3] {
    let target = std::env::var("TARGET").expect("cargo sets TARGET");
    [
        format!("RC_{target}"),
        format!("RC_{}", target.replace('-', "_")),
        "RC".to_owned(),
    ]
}

/// The MSVC resource compiler from the usual places: an `RC` override, else the newest Windows 10/11 SDK under either
/// Program Files (the host's architecture, since the host runs it), else one on PATH. `Err` lists where it looked.
#[cfg(windows)]
fn find_rc() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;

    let host = std::env::var("HOST").expect("cargo sets HOST");
    for name in rc_override_names() {
        if let Some(rc) = std::env::var_os(&name).map(PathBuf::from) {
            if rc.is_file() {
                return Ok(rc);
            }
            return Err(format!("{name}={}, which is not a file", rc.display()));
        }
    }
    let arch = match host.split('-').next() {
        Some("x86_64") => "x64",
        Some("aarch64") => "arm64",
        _ => "x86",
    };
    let mut searched = Vec::new();
    for root in ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
    {
        let bin = PathBuf::from(root).join("Windows Kits").join("10").join("bin");
        let Ok(entries) = std::fs::read_dir(&bin) else {
            searched.push(bin.join("<version>").join(arch).join("rc.exe").display().to_string());
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("10."))
            })
            .collect();
        // Folder names are dotted versions of equal width, so the newest sorts last.
        versions.sort();
        for version in versions.into_iter().rev() {
            let rc = version.join(arch).join("rc.exe");
            if rc.is_file() {
                return Ok(rc);
            }
            searched.push(rc.display().to_string());
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let rc = dir.join("rc.exe");
            if rc.is_file() {
                return Ok(rc);
            }
        }
        searched.push("rc.exe on PATH".to_owned());
    }
    Err(searched.join(", "))
}

#[cfg(not(windows))]
fn embed_windows_icon() {
    panic!("the Windows icon resource is compiled on Windows; cross-building codexbar for Windows isn't supported");
}
