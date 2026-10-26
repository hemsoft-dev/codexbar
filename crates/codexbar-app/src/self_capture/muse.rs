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
    let mut saved = Vec::new();
    for state in ["measured", "partial", "exhausted", "null", "timeout", "stale"] {
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
