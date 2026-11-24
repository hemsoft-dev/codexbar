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

/// The argument run.ps1's `Run` entry passes when Windows starts CodexBar at sign-in. The package's startup task is
/// told apart by its activation kind instead.
pub const STARTUP_ARG: &str = "--startup";

/// What started this CodexBar, which decides whether its window shows (#336).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    /// Windows started it at sign-in: the package's startup task, or the `Run` entry with `--startup`.
    pub with_windows: bool,
    /// It was asked to show something: a widget tile's account, or Settings.
    pub asked_to_show: bool,
}

impl Start {
    /// What this process's arguments say started it. `startup_task` is whether the package's startup task did, and
    /// `focus` whether a widget tile asked for an account.
    pub fn from_args(args: &[String], startup_task: bool, focus: bool) -> Self {
        let has = |wanted: &str| args.iter().any(|arg| arg == wanted);
        Self {
            with_windows: startup_task || has(STARTUP_ARG),
            asked_to_show: focus || has("--settings"),
        }
    }

    /// Only a start with Windows stays in the notification area, and only when it wasn't asked to show something.
    /// Where the window was when CodexBar last ran (hidden to the tray, minimized) never hides it: a user who starts
    /// CodexBar expects to see it.
    pub fn stays_hidden(self) -> bool {
        self.with_windows && !self.asked_to_show
    }
}

/// How the window opens: its bounds and state, and the display to open it on.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    pub bounds: WindowBounds,
    pub display: Option<DisplayId>,
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
/// maximized window comes back maximized. Whether the window was hidden or minimized doesn't decide anything here: a
/// start the user made always shows the window (`Start`).
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
    })
}

/// A window rectangle in physical pixels, as Windows keeps a window's normal (restore) position. Only Windows settles
/// a window; the tests check the arithmetic on every platform.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edges {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Where settling can put a window: its saved bounds, or, where those would take it onto another display, its saved
/// size where it is now (`None` when its size is already right).
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settled {
    pub at_saved: Edges,
    pub in_place: Option<Edges>,
}

/// The normal rectangles that give the window `wanted` as its content bounds, from its normal rectangle and content
/// bounds now, at the display's `scale`; `None` when it already has them (within a logical pixel). GPUI can have put
/// it elsewhere: it centers a window whose saved bounds have their center off the display, though CodexBar keeps
/// bounds whose title bar is still on it. The frame around the content (its borders and shadow) keeps the size it
/// has now.
#[cfg(any(windows, test))]
pub fn settled_normal(normal: Edges, now: Bounds<Pixels>, wanted: Bounds<Pixels>, scale: f32) -> Option<Settled> {
    let close = |a: Pixels, b: Pixels| (f32::from(a) - f32::from(b)).abs() < 1.;
    let same_size = close(now.size.width, wanted.size.width) && close(now.size.height, wanted.size.height);
    if same_size && close(now.origin.x, wanted.origin.x) && close(now.origin.y, wanted.origin.y) {
        return None;
    }
    let physical = |value: Pixels| (f32::from(value) * scale).round() as i32;
    let frame = Edges {
        left: physical(now.origin.x) - normal.left,
        top: physical(now.origin.y) - normal.top,
        right: normal.right - physical(now.origin.x + now.size.width),
        bottom: normal.bottom - physical(now.origin.y + now.size.height),
    };
    let around = |origin: gpui_kit::Point<Pixels>| Edges {
        left: physical(origin.x) - frame.left,
        top: physical(origin.y) - frame.top,
        right: physical(origin.x + wanted.size.width) + frame.right,
        bottom: physical(origin.y + wanted.size.height) + frame.bottom,
    };
    Some(Settled {
        at_saved: around(wanted.origin),
        in_place: (!same_size).then(|| around(now.origin)),
    })
}

/// Gives a restored window the bounds it was saved with (#336). GPUI turns the saved bounds into the display's
/// pixels, but the window is created on the primary display; when it then moves to a display with another scale,
/// Windows scales it once more, so on a 175% display beside a 100% one it opened 1.75 times too large, and larger at
/// every start. Called once the window is on its display, so nothing scales it again. It also restores saved bounds
/// GPUI replaced with its own centered ones (a window left partly past a display's outer edge). The placement is
/// applied from a task, never while `App` is borrowed: Windows sends the window's move and size messages before `SetWindowPlacement`
/// returns, and GPUI's handlers for them update the app.
/// With nothing saved, the window opened at its default bounds and there is nothing to settle.
#[cfg(windows)]
pub fn settle(window: &Window, restored: Option<&Restored>, cx: &App) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, SetWindowPlacement, WINDOWPLACEMENT};
    let (Some(restored), Some(hwnd)) = (restored, crate::tray::hwnd(window)) else {
        return;
    };
    let bounds_of = |bounds: WindowBounds| match bounds {
        WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) | WindowBounds::Fullscreen(bounds) => bounds,
    };
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: `hwnd` is this window's handle and `placement` is a properly sized WINDOWPLACEMENT.
    if unsafe { GetWindowPlacement(hwnd, &mut placement) }.is_err() {
        return;
    }
    let normal = placement.rcNormalPosition;
    let Some(settled) = settled_normal(
        Edges {
            left: normal.left,
            top: normal.top,
            right: normal.right,
            bottom: normal.bottom,
        },
        bounds_of(window.window_bounds()),
        bounds_of(restored.bounds),
        window.scale_factor(),
    ) else {
        return;
    };
    cx.foreground_executor()
        .spawn(async move {
            use windows::Win32::Graphics::Gdi::{
                MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MonitorFromRect, MonitorFromWindow,
            };
            use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SW_HIDE};
            let rect = |edges: Edges| RECT {
                left: edges.left,
                top: edges.top,
                right: edges.right,
                bottom: edges.bottom,
            };
            // The saved bounds, unless most of them lie on another display: Windows would move the window there and
            // scale it to that display, which is what settling undoes. Then just the saved size, where it is.
            let saved = rect(settled.at_saved);
            // SAFETY: plain monitor lookups for this window's handle and a rectangle on the stack.
            let (on, home) = unsafe {
                (
                    MonitorFromRect(&saved, MONITOR_DEFAULTTONULL),
                    MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                )
            };
            let target = if on.is_invalid() || on == home {
                Some(saved)
            } else {
                settled.in_place.map(rect)
            };
            let Some(target) = target else {
                return;
            };
            // Read again: startup may have hidden the window (a start with Windows) since `settle` was called.
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            // SAFETY: as above.
            if unsafe { GetWindowPlacement(hwnd, &mut placement) }.is_err() {
                return;
            }
            placement.rcNormalPosition = target;
            // A hidden window stays hidden (its maximized state is kept for when it shows); a shown one keeps the
            // state read back, so a maximized window stays maximized.
            // SAFETY: `hwnd` is this window's handle.
            if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                placement.showCmd = SW_HIDE.0 as u32;
            }
            // SAFETY: as above.
            let _ = unsafe { SetWindowPlacement(hwnd, &placement) };
        })
        .detach();
}

/// Other platforms place the window in one step.
#[cfg(not(windows))]
pub fn settle(_: &Window, _: Option<&Restored>, _: &App) {}

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
    fn a_window_hidden_while_maximized_comes_back_maximized_on_its_display() {
        let mut placement = saved("right", 2200., 100., 1440., 960.);
        placement.maximized = true;
        placement.minimized = true;
        let restored = restore(Some(&placement), &two_displays(), Some(DisplayId::new(1))).unwrap();
        assert!(matches!(restored.bounds, WindowBounds::Maximized(_)));
        assert_eq!(restored.display, Some(DisplayId::new(2)));
    }

    fn content(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(y)),
            size: size(px(width), px(height)),
        }
    }

    // #336: saved at 1000×700 on a 175% display, the window came back scaled twice (3062×2144 physical content).
    // Settling it gives the saved content size back and keeps the 8px frame on each side and the 31px title strip.
    #[test]
    fn a_window_scaled_twice_settles_at_its_saved_size() {
        let normal = Edges {
            left: 3667,
            top: 136,
            right: 3667 + 8 + 3062 + 8,
            bottom: 136 + 31 + 2144 + 8,
        };
        let now = content(3675. / 1.75, 167. / 1.75, 3062. / 1.75, 2144. / 1.75);
        let wanted = content(3675. / 1.75, 167. / 1.75, 1000., 700.);
        let settled = settled_normal(normal, now, wanted, 1.75).unwrap();
        let expected = Edges {
            left: 3667,
            top: 136,
            right: 3675 + 1750 + 8,
            bottom: 167 + 1225 + 8,
        };
        // At its saved origin already, so both choices are the same rectangle.
        assert_eq!(settled.at_saved, expected);
        assert_eq!(settled.in_place, Some(expected));
    }

    #[test]
    fn a_window_already_at_its_saved_bounds_is_left_alone() {
        let normal = Edges {
            left: 100,
            top: 100,
            right: 1116,
            bottom: 839,
        };
        let bounds = content(108., 131., 1000., 700.);
        assert_eq!(settled_normal(normal, bounds, bounds, 1.), None);
    }

    // GPUI centers a window whose saved bounds have their center off the display. Settling offers the saved bounds
    // back, and the saved size where the window is for when the saved bounds lie mostly on another display.
    #[test]
    fn a_window_gpui_placed_elsewhere_can_go_back_or_keep_its_place() {
        let normal = Edges {
            left: 0,
            top: 0,
            right: 1766,
            bottom: 1239,
        };
        let now = content(8., 31., 1750., 1200.);
        let wanted = content(208., 131., 1000., 700.);
        assert_eq!(
            settled_normal(normal, now, wanted, 1.),
            Some(Settled {
                at_saved: Edges {
                    left: 200,
                    top: 100,
                    right: 1216,
                    bottom: 839,
                },
                in_place: Some(Edges {
                    left: 0,
                    top: 0,
                    right: 1016,
                    bottom: 739,
                }),
            })
        );
    }

    // Only the origin is off: the saved bounds come back, and there is no size to correct in place.
    #[test]
    fn a_window_at_its_saved_size_elsewhere_only_moves_back() {
        let normal = Edges {
            left: 0,
            top: 0,
            right: 1016,
            bottom: 739,
        };
        let now = content(8., 31., 1000., 700.);
        let wanted = content(208., 131., 1000., 700.);
        let settled = settled_normal(normal, now, wanted, 1.).unwrap();
        assert_eq!(settled.in_place, None);
        assert_eq!(
            settled.at_saved,
            Edges {
                left: 200,
                top: 100,
                right: 1216,
                bottom: 839,
            }
        );
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn the_run_entry_s_startup_argument_is_a_start_with_windows() {
        let start = Start::from_args(&args(&["codexbar.exe", STARTUP_ARG]), false, false);
        assert!(start.with_windows && !start.asked_to_show);
        assert!(start.stays_hidden());
    }

    #[test]
    fn the_package_startup_task_is_a_start_with_windows() {
        assert!(Start::from_args(&args(&["codexbar.exe"]), true, false).stays_hidden());
    }

    #[test]
    fn a_start_with_windows_asked_for_settings_or_an_account_shows_the_window() {
        assert!(!Start::from_args(&args(&["codexbar.exe", STARTUP_ARG, "--settings"]), false, false).stays_hidden());
        assert!(!Start::from_args(&args(&["codexbar.exe"]), true, true).stays_hidden());
    }

    #[test]
    fn a_plain_start_shows_the_window() {
        let start = Start::from_args(&args(&["codexbar.exe"]), false, false);
        assert!(!start.with_windows && !start.asked_to_show);
    }

    #[test]
    fn a_start_the_user_made_shows_the_window() {
        let start = Start {
            with_windows: false,
            asked_to_show: false,
        };
        assert!(!start.stays_hidden());
    }

    #[test]
    fn a_start_with_windows_stays_in_the_notification_area() {
        let start = Start {
            with_windows: true,
            asked_to_show: false,
        };
        assert!(start.stays_hidden());
    }

    #[test]
    fn a_start_with_windows_asked_to_show_something_shows_the_window() {
        let start = Start {
            with_windows: true,
            asked_to_show: true,
        };
        assert!(!start.stays_hidden());
    }
}
