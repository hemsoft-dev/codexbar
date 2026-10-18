//! Embeds CodexBar's mark as the Windows executable's icon resource 1 (#182). GPUI's Windows backend loads that
//! resource for its window class, so the taskbar, Alt-Tab, Explorer and shortcuts show the mark instead of the generic
//! executable icon. The other platforms take their icons from the package (`package-macos.sh`, `package-linux.sh`).

#[path = "src/mark.rs"]
mod mark;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/mark.rs");
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
    // A build without a resource compiler (rc.exe from the Windows SDK) fails here rather than shipping without the icon.
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_required()
        .expect("compile the Windows icon resource; rc.exe comes with the Windows SDK");
}

#[cfg(not(windows))]
fn embed_windows_icon() {
    panic!("the Windows icon resource is compiled on Windows; cross-building codexbar for Windows isn't supported");
}
