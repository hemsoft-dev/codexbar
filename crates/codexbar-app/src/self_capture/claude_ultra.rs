//! Synthetic Claude transitions rendered by the native app through production parsing.
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
    let layout = if wide { "wide" } else { "compact" };
    let mut saved = Vec::new();
    for (appearance, theme) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        for state in ["finite", "weekly", "restored", "max", "unknown"] {
            cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_claude_ultra(state, cx)));
            settle(cx).await;
            cx.update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let ring = window.find("focus-share").label().unwrap().to_owned();
                    assert!(!ring.contains("No 5-hour limit"));
                    if matches!(state, "weekly" | "unknown") {
                        assert!(ring.contains("Weekly"), "saved session choice must fall back: {ring}");
                        assert!(
                            window
                                .find(("provider-message", 0usize))
                                .label()
                                .unwrap()
                                .contains("unknown")
                        );
                        if wide {
                            assert_eq!(
                                window.find("window-5h-claude-bar").label(),
                                Some("Claude 5-hour limit: unknown/unavailable")
                            );
                            assert_eq!(
                                window.find("window-5h-claude-resets").label(),
                                Some("Claude 5-hour limit: unknown/unavailable")
                            );
                        }
                    }
                })
            })
            .expect("native Claude semantic assertions");
            let name = format!("claude-ultra-{state}-{layout}-{theme}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
        }
    }
    saved
}
