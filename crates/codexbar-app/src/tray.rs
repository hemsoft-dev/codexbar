//! The notification-area icon: left click toggles the dashboard, right click opens the menu.
//!
//! GPUI has no hide/show for windows, so on Windows visibility goes through Win32 on the window's HWND; elsewhere the
//! window is minimized and restored. The window is never destroyed while the app runs; closing it hides it, which
//! keeps the last view and makes reopening instant.

use std::time::Duration;

use gpui_kit::{App, Global, Window};
#[cfg(any(windows, target_os = "linux"))]
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

    let builder = TrayIconBuilder::new();
    // The macOS menu bar draws template images in its own color, light or dark.
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(menu_bar_icon()?);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.with_icon(brand_icon()?);
    let icon = builder
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

/// Where the tray icon is on screen, as `(x, y, width, height)` in physical pixels; `None` without an icon or on Linux,
/// where tray-icon can't tell.
#[cfg(feature = "capture")]
pub fn rect(cx: &App) -> Option<(f64, f64, f64, f64)> {
    let rect = cx.try_global::<TrayState>()?.icon.rect()?;
    Some((
        rect.position.x,
        rect.position.y,
        f64::from(rect.size.width),
        f64::from(rect.size.height),
    ))
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
pub fn hide(window: &Window, _cx: &App) {
    if let Some(hwnd) = hwnd(window) {
        // The return value is the previous visibility, not an error.
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

#[cfg(windows)]
pub fn show(window: &Window, _cx: &App) {
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

/// macOS hides the whole app, the way a menu-bar app gets out of the way; Linux on X11 minimizes the window, since GPUI
/// has no hide for it there. On Wayland the window stays up: a tray click carries no input serial, so the compositor
/// may refuse to bring a minimized window back (xdg-activation), and it would stay out of reach.
#[cfg(not(windows))]
pub fn hide(window: &Window, cx: &App) {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        cx.hide();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cx;
        if !on_wayland(window) {
            window.minimize_window();
        }
    }
}

/// Whether GPUI drew the window through Wayland rather than X11.
#[cfg(target_os = "linux")]
fn on_wayland(window: &Window) -> bool {
    HasWindowHandle::window_handle(window).is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Wayland(_)))
}

/// Brings the window to the front; an accessory app (macOS) must be activated first or it stays behind others.
#[cfg(not(windows))]
pub fn show(window: &Window, cx: &App) {
    cx.activate(true);
    window.activate_window();
}

/// The menu-bar template image: the brand mark's three bars, opaque, with their empty tracks faint, on nothing.
#[cfg(any(target_os = "macos", test))]
fn menu_bar_icon() -> anyhow::Result<Icon> {
    const SIZE: usize = 32;
    let bars = [(6, 26), (14, 20), (22, 14)];
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            for (top, end) in bars {
                if (top..top + 5).contains(&y) && (3..29).contains(&x) {
                    let alpha = if x < end + 2 { 0xFF } else { 0x55 };
                    rgba[(y * SIZE + x) * 4..][..4].copy_from_slice(&[0, 0, 0, alpha]);
                }
            }
        }
    }
    Ok(Icon::from_rgba(rgba, SIZE as u32, SIZE as u32)?)
}

/// The 32x32 mark (`crate::mark`), as the notification area shows it.
#[cfg(any(not(target_os = "macos"), test))]
fn brand_icon() -> anyhow::Result<Icon> {
    Ok(Icon::from_rgba(crate::mark::rgba(32), 32, 32)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_icon_builds() {
        assert!(brand_icon().is_ok());
        assert!(menu_bar_icon().is_ok());
    }
}
