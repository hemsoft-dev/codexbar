//! Synthetic Antigravity CLI data rendered through the production parser and views.
use super::{save, settle};
use crate::{dashboard::Dashboard, theme::Appearance};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::path::Path;

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    let wide = std::env::var_os("CODEXBAR_CAPTURE_WIDE").is_some();
    let mut saved = Vec::new();
    for (appearance, theme) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        for (mature, name) in [(false, "young"), (true, "mature")] {
            cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_gemini_weekly(mature, cx)));
            settle(cx).await;
            cx.update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let id = if wide { "window-weekly-gemini" } else { "used-gemini" };
                    assert_eq!(window.find(id).label(), Some("Gemini Models weekly: 3%"));
                    let id = if wide {
                        "window-weekly-gemini-bar-sus"
                    } else {
                        "progress-cell-gemini-sus"
                    };
                    assert_eq!(window.find(id).label(), Some("Gemini Models weekly: SUS 42, red"));
                    let ring = window.find("focus-share");
                    assert!(ring.label().unwrap().contains("42% at reset"));
                })
            })
            .expect("CLI fixture must reach the actual table and selected weekly card");
            let name = format!("gemini-weekly-{name}-{theme}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
        }
    }
    saved
}
