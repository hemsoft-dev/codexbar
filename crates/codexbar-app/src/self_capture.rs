//! Screenshots of the running app for pull requests, on a desktop where no screen-capture tool may run (#161).
//!
//! Built only with `--features capture`. With `CODEXBAR_CAPTURE_DIR=<folder>` set, the app renders each view of its
//! real window to a PNG through GPUI's offscreen rendering, exercises the tray's hide and show, records what it saw in
//! `report.json`, and quits. Run it with `--demo`, so it never reads real accounts.
//!
//! The demo's preferences live in `<folder>/prefs`, never the user's own, so a second run into the same folder starts
//! from what the first saved. With `CODEXBAR_CAPTURE_RESIZE=1` it also drags the usage table's column borders (#245)
//! and saves a frame per step (`resize-NNN.png`) for a recording.

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

/// Where a capture run keeps the demo's preferences: inside the capture folder, so the user's own are never touched.
pub fn prefs_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("CODEXBAR_CAPTURE_DIR").map(PathBuf::from)?;
    crate::is_demo().then(|| dir.join("prefs"))
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

/// Drags the usage table's column borders as a user would (#245): the account column down to its minimum and back
/// out, then the limit column narrower. Saves a frame per step and the `columns-minimum` and `columns-resized` states.
async fn resize_columns(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &std::path::Path,
) -> Vec<String> {
    use gpui_kit::test::TestEventExt as _;
    use gpui_kit::{MouseButton, point, px};

    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, Appearance::Light));
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Usage, cx)));
    settle(cx).await;
    let mut saved = Vec::new();
    let mut frame = 0;
    let mut save_frame = |cx: &mut AsyncApp| {
        save(cx, handle, dir, &format!("resize-{frame:03}"));
        frame += 1;
    };
    for _ in 0..6 {
        save_frame(cx);
    }
    let drags: [(usize, f32, Option<&str>); 3] = [
        (0, -400., Some("columns-minimum")),
        (0, 260., None),
        (1, -90., Some("columns-resized")),
    ];
    for (column, dx, state) in drags {
        // The border's grab band sits just inside the header's right edge.
        let start = cx
            .update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let header = window.find(("col-header", column)).bounds();
                    let start = point(header.right() - px(2.), header.center().y);
                    window.pointer_down(start, MouseButton::Left, cx);
                    start
                })
            })
            .ok();
        let Some(start) = start else {
            continue;
        };
        const STEPS: usize = 12;
        for step in 1..=STEPS {
            let at = start + point(px(dx * step as f32 / STEPS as f32), px(0.));
            let _ = cx.update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.pointer_move(at, Some(MouseButton::Left), cx)
                })
            });
            save_frame(cx);
        }
        let end = start + point(px(dx), px(0.));
        let _ = cx.update(|cx| cx.update_window(handle, |_, window, cx| window.pointer_up(end, MouseButton::Left, cx)));
        cx.background_executor().timer(Duration::from_millis(300)).await;
        for _ in 0..6 {
            save_frame(cx);
        }
        if let Some(state) = state
            && save(cx, handle, dir, state)
        {
            saved.push(state.to_owned());
        }
    }
    saved
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
    if std::env::var_os("CODEXBAR_CAPTURE_RESIZE").is_some() {
        saved.extend(resize_columns(cx, handle, dashboard, dir).await);
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
    // With CODEXBAR_CAPTURE_NOTIFY=1, the system notifier (never the demo's) says whether it can deliver here, and
    // posts the Settings test notification when it can.
    let notification = if std::env::var_os("CODEXBAR_CAPTURE_NOTIFY").is_some() {
        cx.update(|cx| {
            let notifier = crate::notifications::system_notifier();
            match notifier.status() {
                crate::notifications::NotifierStatus::Ready => {
                    match notifier.show(&crate::notifications::test_alert(), cx) {
                        Ok(()) => "posted".to_owned(),
                        Err(err) => format!("failed: {err}"),
                    }
                }
                crate::notifications::NotifierStatus::Blocked(reason) => format!("blocked: {reason}"),
            }
        })
    } else {
        "not asked".to_owned()
    };
    settle(cx).await;
    serde_json::json!({
        "saved": saved,
        "shownAtStart": before,
        "shownAfterHide": hidden,
        "shownAfterShow": reshown,
        "notification": notification,
        "trayIconRect": tray.map(|(x, y, w, h)| serde_json::json!({ "x": x, "y": y, "width": w, "height": h })),
    })
}
