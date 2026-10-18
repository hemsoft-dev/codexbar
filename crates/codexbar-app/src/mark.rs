//! CodexBar's mark: three gold usage bars of falling length on a black rounded square.
//!
//! One drawing serves the executable's Windows icon (`build.rs`, #182), the tray icon (`tray.rs`) and the macOS and
//! Linux package icons (`examples/icon.rs`, #167), so they can't drift apart. `package.ps1` draws the same mark for the
//! MSIX logos. The build script and the example include this file by path, so it depends on nothing.

// Each consumer uses a subset: the build script only `ico`, the app only `rgba`.
#![allow(dead_code)]

/// The background: HemSoft black.
pub const BLACK: [u8; 4] = [0x0A, 0x0A, 0x0A, 0xFF];
/// The used part of each bar: HemSoft gold.
pub const GOLD: [u8; 4] = [0xD4, 0xAF, 0x37, 0xFF];
/// The rest of each bar.
pub const TRACK: [u8; 4] = [0x2A, 0x2A, 0x2A, 0xFF];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// The mark on a 32-unit grid: each bar's top and where its gold ends. Bars run from x 5 to 27 and are 4 units tall.
const BARS: [(f32, f32); 3] = [(6.0, 26.0), (14.0, 20.0), (22.0, 14.0)];
/// The square's corner radius, in grid units.
const RADIUS: f32 = 6.0;

/// The sizes an icon resource holds, so Windows finds a crisp rendering at every DPI and in every place it shows
/// the icon: title bars, the taskbar, Alt-Tab, Explorer, shortcuts and the Start menu.
pub const ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

/// The pixel at (`x`, `y`) of the mark drawn at `size` px, as RGBA.
pub fn pixel(x: u32, y: u32, size: u32) -> [u8; 4] {
    let unit = size as f32 / 32.0;
    let radius = RADIUS * unit;
    let max = size as f32;
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let (cx, cy) = (px.clamp(radius, max - radius), py.clamp(radius, max - radius));
    if (px - cx).powi(2) + (py - cy).powi(2) > radius * radius {
        return CLEAR;
    }
    let (gx, gy) = (px / unit, py / unit);
    for (top, end) in BARS {
        if (top..top + 4.0).contains(&gy) && (5.0..27.0).contains(&gx) {
            return if gx < end { GOLD } else { TRACK };
        }
    }
    BLACK
}

/// The mark at `size` px as RGBA, top row first.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            out.extend_from_slice(&pixel(x, y, size));
        }
    }
    out
}

/// An `.ico` file with the mark at each of `sizes` (1 to 256 px) as a 32-bit bitmap with alpha, the form every
/// Windows version reads.
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    assert!(
        !sizes.is_empty() && sizes.len() <= u16::MAX as usize,
        "an icon holds 1 to 65535 images"
    );
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bitmap(size)).collect();
    let mut out = Vec::new();
    // ICONDIR: reserved, type 1 (icon), image count.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        assert!((1..=256).contains(&size), "an icon image is 1 to 256 px");
        // ICONDIRENTRY: width and height (0 means 256), palette size, reserved, planes, bits per pixel, byte size, offset.
        let side = if size == 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in &images {
        out.extend_from_slice(image);
    }
    out
}

/// One icon image: a BITMAPINFOHEADER, the BGRA pixels bottom row first, then the 1-bit mask (set where transparent).
fn bitmap(size: u32) -> Vec<u8> {
    let mask_stride = (size.div_ceil(8)).div_ceil(4) * 4;
    let pixels_len = size * size * 4;
    let mask_len = mask_stride * size;
    let mut out = Vec::with_capacity((40 + pixels_len + mask_len) as usize);
    for field in [
        40u32,
        size,
        size * 2,       // The height counts the pixel rows and the mask rows.
        1 | (32 << 16), // Planes and bits per pixel.
        0,              // No compression.
        pixels_len + mask_len,
        0,
        0,
        0,
        0,
    ] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    for y in (0..size).rev() {
        for x in 0..size {
            let [r, g, b, a] = pixel(x, y, size);
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    for y in (0..size).rev() {
        let mut row = vec![0u8; mask_stride as usize];
        for x in 0..size {
            if pixel(x, y, size)[3] == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 32 px mark as the tray drew it before the drawing was shared, on integer pixels.
    fn tray_icon_before_sharing() -> Vec<u8> {
        const SIZE: usize = 32;
        let bars = [(6, 26), (14, 20), (22, 14)];
        let mut rgba = vec![0u8; SIZE * SIZE * 4];
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (fx, fy, r) = (x as f32 + 0.5, y as f32 + 0.5, 6.0f32);
                let (cx, cy) = (fx.clamp(r, 32.0 - r), fy.clamp(r, 32.0 - r));
                let corner = (fx - cx).powi(2) + (fy - cy).powi(2) > r * r;
                let mut pixel = if corner { [0, 0, 0, 0] } else { BLACK };
                for (top, end) in bars {
                    if (top..top + 4).contains(&y) && (5..27).contains(&x) {
                        pixel = if x < end { GOLD } else { TRACK };
                    }
                }
                rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&pixel);
            }
        }
        rgba
    }

    #[test]
    fn the_tray_icon_is_unchanged_by_sharing_the_drawing() {
        assert_eq!(rgba(32), tray_icon_before_sharing());
    }

    #[test]
    fn the_mark_scales_its_corners_and_bars_with_the_size() {
        for size in ICON_SIZES {
            let last = size - 1;
            assert_eq!(pixel(0, 0, size), CLEAR, "{size}: top-left corner is cut");
            assert_eq!(pixel(last, last, size), CLEAR, "{size}: bottom-right corner is cut");
            assert_eq!(
                pixel(size / 2, 0, size),
                BLACK,
                "{size}: the top edge's middle is black"
            );
            // The third bar (rows 22 to 26) is gold to grid unit 14 and track to 27: sample well inside each.
            let unit = size as f32 / 32.0;
            let y = (24.0 * unit) as u32;
            assert_eq!(
                pixel((8.0 * unit) as u32, y, size),
                GOLD,
                "{size}: the third bar starts gold"
            );
            assert_eq!(
                pixel((20.0 * unit) as u32, y, size),
                TRACK,
                "{size}: the third bar ends in track"
            );
            assert_eq!(
                pixel((16.0 * unit) as u32, (11.0 * unit) as u32, size),
                BLACK,
                "{size}: between bars is black"
            );
        }
    }

    /// Reads one image's entry: its size, and where its pixels start.
    fn entry(ico: &[u8], index: usize) -> (u32, usize) {
        let entry = &ico[6 + 16 * index..][..16];
        let size = if entry[0] == 0 { 256 } else { entry[0] as u32 };
        assert_eq!(entry[1], entry[0], "square");
        assert_eq!(u16::from_le_bytes([entry[4], entry[5]]), 1, "one plane");
        assert_eq!(u16::from_le_bytes([entry[6], entry[7]]), 32, "32 bits per pixel");
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        (size, offset)
    }

    #[test]
    fn the_icon_file_lists_every_size_and_stores_the_pixels_bottom_up() {
        let ico = ico(&ICON_SIZES);
        assert_eq!(&ico[..6], &[0, 0, 1, 0, ICON_SIZES.len() as u8, 0]);
        let mut expected_offset = 6 + 16 * ICON_SIZES.len();
        for (index, &size) in ICON_SIZES.iter().enumerate() {
            let (listed, offset) = entry(&ico, index);
            assert_eq!(listed, size);
            assert_eq!(offset, expected_offset, "{size}: images follow each other");
            let header = &ico[offset..][..40];
            assert_eq!(
                u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
                size,
                "{size}: width"
            );
            assert_eq!(
                u32::from_le_bytes([header[8], header[9], header[10], header[11]]),
                size * 2,
                "{size}: height"
            );
            let pixels = &ico[offset + 40..];
            let at = |x: u32, y: u32| {
                let i = ((size - 1 - y) * size + x) as usize * 4;
                let [b, g, r, a] = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
                [r, g, b, a]
            };
            for (x, y) in [(0, 0), (size / 2, size / 2), (size / 4, size / 4), (size - 1, size / 2)] {
                assert_eq!(at(x, y), pixel(x, y, size), "{size}: pixel ({x}, {y})");
            }
            let mask_stride = size.div_ceil(8).div_ceil(4) * 4;
            let mask = &pixels[(size * size * 4) as usize..][..(mask_stride * size) as usize];
            // The bottom-up mask's last row is the image's top row, whose first pixel is a cut corner.
            assert_eq!(
                mask[((size - 1) * mask_stride) as usize] & 0x80,
                0x80,
                "{size}: the corner is masked"
            );
            expected_offset = offset + 40 + (size * size * 4 + mask_stride * size) as usize;
        }
        assert_eq!(ico.len(), expected_offset, "nothing trails the last image");
    }

    #[test]
    #[should_panic(expected = "1 to 256 px")]
    fn an_icon_image_cannot_exceed_256_px() {
        let _ = ico(&[512]);
    }
}
