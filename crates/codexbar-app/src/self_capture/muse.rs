//! Real macOS GPUI renders of the production Muse adapter over sanitized fixtures.
use super::{save, settle};
use crate::{
    dashboard::{Dashboard, DashboardView},
    theme::Appearance,
};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::path::Path;

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    // Wide, the 5-hour window's forecast sits in its own column (#204) and the primary one stays blank (#279).
    let at_reset = if std::env::var_os("CODEXBAR_CAPTURE_WIDE").is_some() {
        "window-5h-muse-demo-at-reset"
    } else {
        "at-reset-muse-demo"
    };
    let mut saved = Vec::new();
    for state in [
        "measured",
        "partial",
        "exhausted",
        "null",
        "timeout",
        "stale",
        "reset-before",
        "reset-after",
        "cached-alert",
    ] {
        cx.update(|cx| {
            crate::prefs_hub::PrefsHub::set_appearance(cx, Appearance::Light);
            dashboard.update(cx, |d, cx| d.capture_muse(state, cx));
        });
        settle(cx).await;
        cx.update(|cx| {
            cx.update_window(handle, |_, w, cx| {
                w.render_frame(cx);
                assert!(w.try_find("muse-quota-note").is_some());
                assert!(w.try_find("progress-cell-muse-demo-sus").is_none());
                assert!(w.try_find("focus-metric-1-sus").is_none());
                if state == "cached-alert" {
                    let alert = w.find("alert-strongest").label().unwrap().to_owned();
                    assert!(alert.starts_with("Limit soon (projected): 5-hour window"), "{alert}");
                    assert!(
                        alert.contains("resets in 1h 55m") && alert.contains("2m early"),
                        "{alert}"
                    );
                    let reset = w.find(at_reset).label().unwrap().to_owned();
                    assert!(reset.contains("runs out in 1h 53m"), "{reset}");
                }
                if state == "reset-after" {
                    let note = w.find("muse-quota-note");
                    assert!(note.label().unwrap().contains("5-hour: n/a"));
                    assert!(!note.label().unwrap().contains("Weekly: n/a"));
                    assert_eq!(w.find("used-muse-demo").label(), Some("Weekly: 5%"));
                }
                if state == "null" {
                    assert!(w.try_find("focus-card-allowance-unavailable").is_some());
                    assert!(w.try_find("focus-card-no-limits").is_none());
                }
                if matches!(state, "stale" | "timeout" | "null") {
                    assert!(w.try_find("state-muse-demo").is_some());
                }
            })
        })
        .unwrap();
        let name = format!("muse-{state}-table-detail");
        assert!(save(cx, handle, dir, &name));
        saved.push(name);
        // The same TileView consumer drives the packaged Windows card and this GPUI settings preview.
        cx.update(|cx| dashboard.update(cx, |d, cx| d.show_view(DashboardView::Settings, cx)));
        for entry in ["0-5", "0-5-3"] {
            cx.update(|cx| {
                cx.update_window(handle, |_, w, cx| {
                    w.render_frame(cx);
                    w.within("settings-sidebar").click(entry, cx);
                })
            })
            .unwrap();
        }
        settle(cx).await;
        let name = format!("muse-{state}-widget-preview");
        assert!(save(cx, handle, dir, &name));
        saved.push(name);
    }
    cx.update(|cx| dashboard.update(cx, |d, cx| d.show_view(DashboardView::Settings, cx)));
    let ix = crate::catalog::PROVIDERS
        .iter()
        .position(|p| p.id == codexbar_store::settings::names::MUSE_CODE)
        .unwrap();
    for entry in ["0-1".to_owned(), format!("0-1-{ix}")] {
        cx.update(|cx| {
            cx.update_window(handle, |_, w, cx| {
                w.render_frame(cx);
                w.within("settings-sidebar").click(entry, cx);
            })
        })
        .unwrap();
    }
    settle(cx).await;
    let name = "muse-settings";
    assert!(save(cx, handle, dir, name));
    saved.push(name.into());
    saved
}
