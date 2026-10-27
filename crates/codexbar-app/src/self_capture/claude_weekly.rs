//! A synthetic Claude week through the production parser and views: n/a, early from 1 hour, firm from 12 hours.
use super::{save, settle};
use crate::{dashboard::Dashboard, theme::Appearance};
use chrono::Duration;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::path::Path;

/// Each age into an 8%-used week, its screenshot name, its SUS label, and whether its bar forecasts.
pub(crate) fn states() -> [(Duration, &'static str, &'static str, bool); 6] {
    [
        (Duration::minutes(59), "59m", "Weekly: SUS n/a, unavailable", false),
        (Duration::hours(1), "1h", "Weekly: SUS 7 early, early estimate", false),
        (Duration::hours(6), "6h", "Weekly: SUS 45 early, early estimate", false),
        (
            Duration::hours(12) - Duration::minutes(1),
            "11h59m",
            "Weekly: SUS 89 early, early estimate",
            false,
        ),
        (Duration::hours(12), "12h", "Weekly: SUS 89, yellow", true),
        (
            Duration::hours(15) + Duration::minutes(55),
            "15h55m",
            "Weekly: SUS 84, yellow",
            true,
        ),
    ]
}

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
        for (age, name, sus, forecasts) in states() {
            cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_claude_weekly(age, cx)));
            settle(cx).await;
            cx.update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    let id = if wide {
                        "window-weekly-claude-bar-sus"
                    } else {
                        "focus-metric-1-sus"
                    };
                    assert_eq!(window.find(id).label(), Some(sus), "{name}");
                    let ring = window.find("focus-share").label().unwrap().to_owned();
                    assert_eq!(ring.contains("at reset"), forecasts, "{name}: {ring}");
                })
            })
            .expect("the Claude fixture must reach the actual table and weekly card");
            let name = format!("claude-weekly-{name}-{layout}-{theme}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
        }
    }
    saved
}
