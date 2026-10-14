//! Screenshots of the running app for pull requests, on a desktop where no screen-capture tool may run (#161).
//!
//! Built only with `--features capture`. With `CODEXBAR_CAPTURE_DIR=<folder>` set, the app renders each view of its
//! real window to a PNG through GPUI's offscreen rendering, exercises the tray's hide and show, records what it saw in
//! `report.json`, and quits. Run it with `--demo`, so it never reads real accounts.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, AsyncApp, Entity};

use crate::dashboard::{Dashboard, DashboardView};
use crate::theme::Appearance;

/// Starts the capture if `CODEXBAR_CAPTURE_DIR` is set and this is the demo.
pub fn start(cx: &mut App, handle: AnyWindowHandle, dashboard: Entity<Dashboard>) {
    let Some(dir) = std::env::var_os("CODEXBAR_CAPTURE_DIR").map(PathBuf::from) else {
        return;
    };
    // Real accounts must never end up in a screenshot, and the capture changes the appearance it shows.
    if !crate::is_demo() {
        eprintln!("codexbar: CODEXBAR_CAPTURE_DIR works only with --demo");
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    cx.spawn(async move |cx| {
        let report = run(cx, handle, &dashboard, &dir).await;
        let _ = std::fs::write(dir.join("report.json"), report.to_string());
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn settle(cx: &mut AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(800)).await;
}

fn save(cx: &mut AsyncApp, handle: AnyWindowHandle, dir: &std::path::Path, name: &str) -> bool {
    cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_to_image()
        })
    })
    .ok()
    .and_then(Result::ok)
    .is_some_and(|image| image.save(dir.join(format!("{name}.png"))).is_ok())
}

fn shown(cx: &mut AsyncApp, handle: AnyWindowHandle) -> Option<bool> {
    cx.update(|cx| cx.update_window(handle, |_, window, _| crate::tray::is_shown(window)))
        .ok()
}

async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &std::path::Path,
) -> serde_json::Value {
    cx.background_executor().timer(Duration::from_secs(3)).await;
    let mut saved = Vec::new();
    for (appearance, suffix) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        for (view, name) in [(DashboardView::Usage, "usage"), (DashboardView::Settings, "settings")] {
            cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(view, cx)));
            settle(cx).await;
            let name = format!("{name}-{suffix}");
            if save(cx, handle, dir, &name) {
                saved.push(name);
            }
        }
    }
    // The About page, the last Settings page.
    let _ = cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let last = if cfg!(windows) { 6 } else { 5 };
            window.within("settings-sidebar").click(format!("0-{last}"), cx);
        })
    });
    settle(cx).await;
    if save(cx, handle, dir, "settings-about-dark") {
        saved.push("settings-about-dark".into());
    }
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, Appearance::System));
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Usage, cx)));

    // The tray's own hide and show, as a click on its icon runs them.
    let before = shown(cx, handle);
    let _ = cx.update(|cx| cx.update_window(handle, |_, window, cx| crate::tray::hide(window, cx)));
    settle(cx).await;
    let hidden = shown(cx, handle);
    let _ = cx.update(|cx| cx.update_window(handle, |_, window, cx| crate::tray::show(window, cx)));
    settle(cx).await;
    let reshown = shown(cx, handle);
    let tray = cx.update(|cx| crate::tray::rect(cx));
    serde_json::json!({
        "saved": saved,
        "shownAtStart": before,
        "shownAfterHide": hidden,
        "shownAfterShow": reshown,
        "trayIconRect": tray.map(|(x, y, w, h)| serde_json::json!({ "x": x, "y": y, "width": w, "height": h })),
    })
}
