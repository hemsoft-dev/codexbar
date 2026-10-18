//! Draws CodexBar's app icon, the tray icon's three gold usage bars on a black rounded square, at any size, for the
//! macOS and Linux packages (#167). `package.ps1` draws the same mark for the Windows package.
//!
//! `cargo run -p codexbar-app --example icon -- <out.png> <size>`

use image::{Rgba, RgbaImage};

const BLACK: Rgba<u8> = Rgba([0x0A, 0x0A, 0x0A, 0xFF]);
const GOLD: Rgba<u8> = Rgba([0xD4, 0xAF, 0x37, 0xFF]);
const TRACK: Rgba<u8> = Rgba([0x2A, 0x2A, 0x2A, 0xFF]);

/// The mark on a 32-unit grid, as the tray icon draws it: each bar's top and where its gold ends.
const BARS: [(f32, f32); 3] = [(6.0, 26.0), (14.0, 20.0), (22.0, 14.0)];

fn icon(size: u32) -> RgbaImage {
    let unit = size as f32 / 32.0;
    let radius = 6.0 * unit;
    let max = size as f32;
    RgbaImage::from_fn(size, size, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (cx, cy) = (px.clamp(radius, max - radius), py.clamp(radius, max - radius));
        if (px - cx).powi(2) + (py - cy).powi(2) > radius * radius {
            return Rgba([0, 0, 0, 0]);
        }
        let (gx, gy) = (px / unit, py / unit);
        for (top, end) in BARS {
            if (top..top + 4.0).contains(&gy) && (5.0..27.0).contains(&gx) {
                return if gx < end { GOLD } else { TRACK };
            }
        }
        BLACK
    })
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
