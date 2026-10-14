//! The notification-area icon: left click toggles the dashboard, right click opens the menu.
//!
//! GPUI has no hide/show for windows, so on Windows visibility goes through Win32 on the window's HWND; elsewhere the
//! window is minimized and restored. The window is never destroyed while the app runs; closing it hides it, which
//! keeps the last view and makes reopening instant.

use std::time::Duration;

use gpui_kit::{App, Global, Window};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, SW_HIDE, SW_RESTORE, SW_SHOW, ShowWindow};

/// How often queued tray and menu events are drained on the UI thread.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// What the user asked for through the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Toggle,
    Open,
    Refresh,
    Settings,
    Quit,
}

struct TrayState {
    icon: TrayIcon,
    open: MenuId,
    refresh: MenuId,
    settings: MenuId,
    quit: MenuId,
}

impl Global for TrayState {}

/// Creates the tray icon and starts draining its events into `on_command`. Must run on the UI thread.
pub fn init(cx: &mut App, on_command: impl Fn(TrayCommand, &mut App) + 'static) -> anyhow::Result<()> {
    let open = MenuItem::new("Open dashboard", true, None);
    let refresh = MenuItem::new("Refresh now", true, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit CodexBar", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &refresh, &settings, &PredefinedMenuItem::separator(), &quit])?;

    let icon = TrayIconBuilder::new()
        .with_icon(brand_icon()?)
        .with_tooltip("CodexBar")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()?;

    cx.set_global(TrayState {
        icon,
        open: open.id().clone(),
        refresh: refresh.id().clone(),
        settings: settings.id().clone(),
        quit: quit.id().clone(),
    });

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let commands = cx.update(|cx| drain_commands(cx));
            for command in commands {
                cx.update(|cx| on_command(command, cx));
            }
        }
    })
    .detach();
    Ok(())
}

fn drain_commands(cx: &App) -> Vec<TrayCommand> {
    let mut commands = Vec::new();
    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            commands.push(TrayCommand::Toggle);
        }
    }
    let state = cx.global::<TrayState>();
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        let command = match event.id() {
            id if *id == state.open => TrayCommand::Open,
            id if *id == state.refresh => TrayCommand::Refresh,
            id if *id == state.settings => TrayCommand::Settings,
            id if *id == state.quit => TrayCommand::Quit,
            _ => continue,
        };
        commands.push(command);
    }
    commands
}

/// True once the tray icon exists, so a tooltip set now is shown.
pub fn has_tooltip_target(cx: &App) -> bool {
    cx.try_global::<TrayState>().is_some()
}

/// Replaces the hover text, e.g. "CodexBar — ChatGPT · Codex 91%, resets in 38m".
pub fn set_tooltip(cx: &App, text: &str) {
    if let Some(state) = cx.try_global::<TrayState>() {
        let _ = state.icon.set_tooltip(Some(text));
    }
}

#[cfg(windows)]
fn hwnd(window: &Window) -> Option<HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// True when the window is on screen and not minimized.
#[cfg(windows)]
pub fn is_shown(window: &Window) -> bool {
    hwnd(window).is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() })
}

#[cfg(windows)]
pub fn hide(window: &Window) {
    if let Some(hwnd) = hwnd(window) {
        // The return value is the previous visibility, not an error.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

#[cfg(windows)]
pub fn show(window: &Window) {
    if let Some(hwnd) = hwnd(window) {
        let restore = unsafe { IsIconic(hwnd).as_bool() };
        let _ = unsafe { ShowWindow(hwnd, if restore { SW_RESTORE } else { SW_SHOW }) };
    }
    window.activate_window();
}

/// True while the platform presents the window: not minimized, hidden or fully covered. Unlike whether it is active,
/// this stays true when a click on the tray icon takes activation away, and it follows a restore from the Dock or
/// taskbar.
#[cfg(not(windows))]
pub fn is_shown(window: &Window) -> bool {
    window.is_visible()
}

#[cfg(not(windows))]
pub fn hide(window: &Window) {
    window.minimize_window();
}

#[cfg(not(windows))]
pub fn show(window: &Window) {
    window.activate_window();
}

/// A 32x32 gold-on-black mark: three usage bars of falling length, echoing the app icon.
fn brand_icon() -> anyhow::Result<Icon> {
    const SIZE: usize = 32;
    const BLACK: [u8; 4] = [0x0A, 0x0A, 0x0A, 0xFF];
    const GOLD: [u8; 4] = [0xD4, 0xAF, 0x37, 0xFF];
    const TRACK: [u8; 4] = [0x2A, 0x2A, 0x2A, 0xFF];
    let bars = [(6, 26), (14, 20), (22, 14)];

    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let corner = rounded_corner_cut(x, y, SIZE, 6);
            let mut pixel = if corner { [0, 0, 0, 0] } else { BLACK };
            for (top, end) in bars {
                if (top..top + 4).contains(&y) && (5..27).contains(&x) {
                    pixel = if x < end { GOLD } else { TRACK };
                }
            }
            rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&pixel);
        }
    }
    Ok(Icon::from_rgba(rgba, SIZE as u32, SIZE as u32)?)
}

/// Whether (x, y) falls outside a rounded square of the given corner radius.
fn rounded_corner_cut(x: usize, y: usize, size: usize, radius: usize) -> bool {
    let (x, y, r) = (x as f32 + 0.5, y as f32 + 0.5, radius as f32);
    let max = size as f32;
    let cx = x.clamp(r, max - r);
    let cy = y.clamp(r, max - r);
    (x - cx).powi(2) + (y - cy).powi(2) > r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_corner_cut_corners_transparent_center_opaque() {
        assert!(rounded_corner_cut(0, 0, 32, 6));
        assert!(rounded_corner_cut(31, 31, 32, 6));
        assert!(!rounded_corner_cut(16, 16, 32, 6));
        assert!(!rounded_corner_cut(0, 16, 32, 6));
    }

    #[test]
    fn brand_icon_builds() {
        assert!(brand_icon().is_ok());
    }
}
