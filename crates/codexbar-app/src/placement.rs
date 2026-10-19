//! Where the dashboard window is (#194): its display, normal bounds and state are captured while CodexBar runs, and
//! the window reopens there at the next start. The placement is saved as it changes, never only at exit, because
//! `run.ps1` ends the app with a forced stop.

use codexbar_store::prefs::WindowPlacement;
use gpui_kit::{App, Bounds, DisplayId, Pixels, Size, Window, WindowBounds, point, px, size};

/// The window's size at a first start, and the size a placement falls back to.
pub fn default_size() -> Size<Pixels> {
    size(px(1440.), px(960.))
}

/// The smallest the window can be, as the platform is told.
pub fn min_size() -> Size<Pixels> {
    size(px(1040.), px(720.))
}

/// The window's title bar, the strip the user drags it by: as tall as the dashboard's own title bar.
const TITLE_BAR_HEIGHT: f32 = 40.;
/// How much of the title bar must lie on a display's work area for that display to count as showing the window:
/// enough to grab, not a sliver.
const VISIBLE_WIDTH: f32 = 160.;
const VISIBLE_HEIGHT: f32 = 20.;

/// A connected display, as placement sees it.
#[derive(Clone, Debug)]
pub struct Display {
    pub id: DisplayId,
    /// The platform's stable identifier, when it has one; what a saved placement names.
    pub uuid: Option<String>,
    /// The part not covered by the taskbar or dock, where a window is placed.
    pub work_area: Bounds<Pixels>,
}

/// The connected displays and the primary one's id.
pub fn displays(cx: &App) -> (Vec<Display>, Option<DisplayId>) {
    let displays = cx
        .displays()
        .into_iter()
        .map(|display| Display {
            id: display.id(),
            uuid: display.uuid().ok().map(|uuid| uuid.to_string()),
            work_area: display.visible_bounds(),
        })
        .collect();
    (displays, cx.primary_display().map(|display| display.id()))
}

/// What the window reports now: its normal (restore) bounds in logical pixels, its display, and whether it is
/// maximized or out of the way (minimized, or hidden to the tray).
pub fn capture(window: &Window, cx: &App) -> WindowPlacement {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, true),
    };
    WindowPlacement {
        display: current_display(window, cx)
            .and_then(|display| display.uuid().ok())
            .map(|uuid| uuid.to_string()),
        x: f64::from(f32::from(bounds.origin.x)),
        y: f64::from(f32::from(bounds.origin.y)),
        width: f64::from(f32::from(bounds.size.width)),
        height: f64::from(f32::from(bounds.size.height)),
        maximized,
        // Only the app's own hide and the platform's minimize count, never a window that is merely covered or on
        // another desktop (`is_visible` reports those too).
        minimized: crate::tray::is_out_of_the_way(window),
    }
}

/// The display the window is on now. GPUI's `Window::display` answers with the display the window was opened on,
/// which a move to another display doesn't update, so on Windows the monitor is asked directly.
fn current_display(window: &Window, cx: &App) -> Option<std::rc::Rc<dyn gpui_kit::PlatformDisplay>> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
        if let Some(hwnd) = crate::tray::hwnd(window) {
            let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            let id = DisplayId::new(monitor.0 as u64);
            if let Some(display) = cx.displays().into_iter().find(|display| display.id() == id) {
                return Some(display);
            }
        }
    }
    window.display(cx)
}

/// How the window opens: its bounds and state, the display to open it on, and whether it stays out of the way.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub bounds: WindowBounds,
    pub display: Option<DisplayId>,
    pub hidden: bool,
}

/// Where `saved` reopens on the displays connected now; `None` with nothing saved.
pub fn restore_in(saved: Option<&WindowPlacement>, cx: &App) -> Option<Restored> {
    let (displays, primary) = displays(cx);
    restore(saved, &displays, primary)
}

/// Where `saved` reopens among `displays`: on its own display (by identifier) when that is still connected, at the
/// saved bounds when its title bar still lies on that display's work area and centered on it otherwise; with its
/// display gone, at the saved bounds on a display whose work area shows the title bar, else centered on the primary
/// display at the saved size. The own display is checked first because logical bounds are only comparable within one
/// display: each display's bounds and the bounds of a window on it use that display's own scale, so a window on a
/// 150% display can seem to overlap a 100% display it is nowhere near. The size never drops below the minimum, and a
/// maximized or minimized window comes back that way.
pub fn restore(saved: Option<&WindowPlacement>, displays: &[Display], primary: Option<DisplayId>) -> Option<Restored> {
    let saved = saved?;
    let requested = Bounds {
        origin: point(px(saved.x as f32), px(saved.y as f32)),
        size: size(
            px((saved.width as f32).max(f32::from(min_size().width))),
            px((saved.height as f32).max(f32::from(min_size().height))),
        ),
    };
    let own = saved
        .display
        .as_deref()
        .and_then(|uuid| displays.iter().find(|display| display.uuid.as_deref() == Some(uuid)));
    let primary = primary
        .and_then(|id| displays.iter().find(|display| display.id == id))
        .or(displays.first());
    let (display, bounds) = match own {
        Some(display) if shows(&display.work_area, &requested) => (Some(display), requested),
        // Its display is connected but the title bar would be off it (a changed resolution): bring it back on.
        Some(display) => (Some(display), centered(display, requested.size)),
        None => match displays.iter().find(|display| shows(&display.work_area, &requested)) {
            Some(display) => (Some(display), requested),
            None => match primary {
                Some(display) => (Some(display), centered(display, requested.size)),
                None => (None, requested),
            },
        },
    };
    Some(Restored {
        bounds: if saved.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        },
        display: display.map(|display| display.id),
        hidden: saved.minimized,
    })
}

/// True when enough of the window's title bar lies on `area` to grab it: a window whose bottom edge alone peeks onto
/// a display can't be moved back.
fn shows(area: &Bounds<Pixels>, window: &Bounds<Pixels>) -> bool {
    let title_bar = Bounds {
        origin: window.origin,
        size: size(window.size.width, px(TITLE_BAR_HEIGHT)),
    };
    let left = f32::from(area.origin.x).max(f32::from(title_bar.origin.x));
    let top = f32::from(area.origin.y).max(f32::from(title_bar.origin.y));
    let right = (f32::from(area.origin.x) + f32::from(area.size.width))
        .min(f32::from(title_bar.origin.x) + f32::from(title_bar.size.width));
    let bottom = (f32::from(area.origin.y) + f32::from(area.size.height))
        .min(f32::from(title_bar.origin.y) + f32::from(title_bar.size.height));
    right - left >= VISIBLE_WIDTH && bottom - top >= VISIBLE_HEIGHT
}

/// `wanted`, shrunk to the display's work area when larger, centered on it.
fn centered(display: &Display, wanted: Size<Pixels>) -> Bounds<Pixels> {
    let area = display.work_area;
    let width = f32::from(wanted.width).min(f32::from(area.size.width));
    let height = f32::from(wanted.height).min(f32::from(area.size.height));
    Bounds {
        origin: point(
            px(f32::from(area.origin.x) + (f32::from(area.size.width) - width) / 2.),
            px(f32::from(area.origin.y) + (f32::from(area.size.height) - height) / 2.),
        ),
        size: size(px(width), px(height)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u64, uuid: &str, x: f32, y: f32, width: f32, height: f32) -> Display {
        Display {
            id: DisplayId::new(id),
            uuid: Some(uuid.to_owned()),
            // A 40px taskbar along the bottom.
            work_area: Bounds {
                origin: point(px(x), px(y)),
                size: size(px(width), px(height - 40.)),
            },
        }
    }

    /// A primary 1920×1080 display and a 2560×1440 one to its right.
    fn two_displays() -> Vec<Display> {
        vec![
            display(1, "primary", 0., 0., 1920., 1080.),
            display(2, "right", 1920., 0., 2560., 1440.),
        ]
    }

    fn saved(display: &str, x: f64, y: f64, width: f64, height: f64) -> WindowPlacement {
        WindowPlacement {
            display: Some(display.to_owned()),
            x,
            y,
            width,
            height,
            maximized: false,
            minimized: false,
        }
    }

    fn windowed(restored: &Restored) -> Bounds<Pixels> {
        match restored.bounds {
            WindowBounds::Windowed(bounds) => bounds,
            other => panic!("expected a windowed placement, got {other:?}"),
        }
    }

    #[test]
    fn nothing_saved_means_the_default() {
        assert_eq!(restore(None, &two_displays(), Some(DisplayId::new(1))), None);
    }

    #[test]
    fn a_window_reopens_on_its_own_display_at_its_bounds() {
        let placement = saved("right", 2200., 100., 1440., 960.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(restored.display, Some(DisplayId::new(2)));
        assert_eq!(
            windowed(&restored),
            Bounds {
                origin: point(px(2200.), px(100.)),
                size: size(px(1440.), px(960.)),
            }
        );
        assert!(!restored.hidden);
    }

    #[test]
    fn a_missing_display_with_the_window_off_every_other_one_falls_back_to_the_primary_display() {
        let placement = saved("right", 2200., 100., 1440., 960.);
        let only_primary = vec![display(1, "primary", 0., 0., 1920., 1080.)];
        let restored = restore(Some(&placement), &only_primary, Some(DisplayId::new(1))).unwrap();
        assert_eq!(restored.display, Some(DisplayId::new(1)));
        // Centered on the 1920×1040 work area at the saved size.
        assert_eq!(
            windowed(&restored),
            Bounds {
                origin: point(px(240.), px(40.)),
                size: size(px(1440.), px(960.)),
            }
        );
    }

    #[test]
    fn a_missing_display_keeps_the_bounds_when_another_display_shows_them() {
        let placement = saved("gone", 100., 50., 1440., 960.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(restored.display, Some(DisplayId::new(1)));
        assert_eq!(windowed(&restored).origin, point(px(100.), px(50.)));
    }

    #[test]
    fn the_named_display_wins_over_bounds_that_seem_to_lie_on_another() {
        // Logical bounds are only comparable on one display (each has its own scale): the window stays with the
        // display it was saved on, centered there since its bounds don't fit that display.
        let placement = saved("primary", 2200., 100., 1440., 960.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(restored.display, Some(DisplayId::new(1)));
        assert_eq!(windowed(&restored).origin, point(px(240.), px(40.)));
    }

    #[test]
    fn a_window_whose_title_bar_is_off_the_display_is_centered() {
        // Only the bottom 80px overlap the display: nothing to grab, so it is brought back.
        let placement = saved("primary", 100., -880., 1440., 960.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(windowed(&restored).origin, point(px(240.), px(40.)));
        // Half the title bar on the display is enough.
        let placement = saved("primary", 100., -20., 1440., 960.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(windowed(&restored).origin, point(px(100.), px(-20.)));
    }

    #[test]
    fn a_window_that_drifted_off_its_own_display_is_centered_on_it() {
        // The right display shrank: the saved origin is beyond its new width.
        let placement = saved("right", 4400., 100., 1440., 960.);
        let displays = vec![
            display(1, "primary", 0., 0., 1920., 1080.),
            display(2, "right", 1920., 0., 1920., 1080.),
        ];
        let restored = restore(Some(&placement), &displays, Some(DisplayId::new(1))).unwrap();
        assert_eq!(restored.display, Some(DisplayId::new(2)));
        assert_eq!(
            windowed(&restored),
            Bounds {
                origin: point(px(2160.), px(40.)),
                size: size(px(1440.), px(960.)),
            }
        );
    }

    #[test]
    fn a_window_larger_than_the_display_is_shrunk_to_its_work_area() {
        let placement = saved("gone", 5000., 5000., 3000., 2000.);
        let only_primary = vec![display(1, "primary", 0., 0., 1920., 1080.)];
        let restored = restore(Some(&placement), &only_primary, Some(DisplayId::new(1))).unwrap();
        assert_eq!(
            windowed(&restored),
            Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(1920.), px(1040.)),
            }
        );
    }

    #[test]
    fn a_saved_size_below_the_minimum_is_raised_to_it() {
        let placement = saved("primary", 10., 10., 300., 200.);
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert_eq!(windowed(&restored).size, min_size());
    }

    #[test]
    fn maximized_and_minimized_states_come_back() {
        let mut placement = saved("primary", 10., 10., 1440., 960.);
        placement.maximized = true;
        placement.minimized = true;
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert!(matches!(restored.bounds, WindowBounds::Maximized(_)));
        assert!(restored.hidden);
    }
}
