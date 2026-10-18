//! Draws CodexBar's mark (`src/mark.rs`) at any size, for the macOS and Linux packages (#167). The Windows executable
//! embeds the same mark as its icon (`build.rs`, #182), and `package.ps1` draws it for the MSIX logos.
//!
//! `cargo run -p codexbar-app --example icon -- <out.png> <size>`

use image::RgbaImage;

#[path = "../src/mark.rs"]
mod mark;

fn icon(size: u32) -> RgbaImage {
    RgbaImage::from_raw(size, size, mark::rgba(size)).expect("the mark has size x size pixels")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(out), Some(size)) = (args.next(), args.next().and_then(|size| size.parse::<u32>().ok())) else {
        eprintln!("usage: icon <out.png> <size>");
        std::process::exit(2);
    };
    if let Err(err) = icon(size).save(&out) {
        eprintln!("icon: couldn't write {out}: {err}");
        std::process::exit(1);
    }
}
