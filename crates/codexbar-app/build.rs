//! Embeds CodexBar's mark as the Windows executable's icon resource 1 (#182). GPUI's Windows backend loads that
//! resource for its window class, so the taskbar, Alt-Tab, Explorer and shortcuts show the mark instead of the generic
//! executable icon. The other platforms take their icons from the package (`package-macos.sh`, `package-linux.sh`).

#[path = "src/mark.rs"]
mod mark;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/mark.rs");
    println!("cargo:rerun-if-env-changed=RC");
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
    // embed-resource's own search for rc.exe depends on registry keys that are stale on some machines (#182), so the
    // compiler is located here and handed over through the RC variable it honours.
    let rc = find_rc().unwrap_or_else(|searched| {
        panic!(
            "rc.exe (the resource compiler from the Windows SDK, installed with the Visual Studio Build Tools' \
             \"Desktop development with C++\" workload) was not found. Set RC to its path. Looked for: {searched}"
        )
    });
    // SAFETY: the build script is single-threaded here; nothing reads the environment concurrently.
    unsafe { std::env::set_var("RC", &rc) };
    // A build whose resource compiler fails stops here rather than shipping without the icon.
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|err| panic!("compile the Windows icon resource with {}: {err}", rc.display()));
}

/// The resource compiler for this host: an `RC` override, else the newest Windows 10/11 SDK under either Program Files,
/// else one on PATH. `Err` lists where it looked.
#[cfg(windows)]
fn find_rc() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;

    let host = std::env::var("HOST").expect("cargo sets HOST");
    for name in [
        format!("RC_{host}"),
        format!("RC_{}", host.replace('-', "_")),
        "RC".to_owned(),
    ] {
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
