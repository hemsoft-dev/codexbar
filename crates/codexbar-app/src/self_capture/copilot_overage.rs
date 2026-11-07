//! A Copilot account past its premium allowance (#318) in the real Usage table, beside one exactly at it, through the
//! production parser.
use super::{save, settle};
use crate::dashboard::Dashboard;
use crate::theme::Appearance;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::path::Path;

/// Each row's used cell and bar, as the table labels them.
const ROWS: [(&str, &str, &str); 2] = [
    (
        "copilot-over-allowance",
        "3,000 requests: 3,740",
        "100% used, 174% at reset · limit reached",
    ),
    (
        "copilot-at-allowance",
        "3,000 requests: 3,000",
        "100% used, 159% at reset · limit reached",
    ),
];

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    let mut saved = Vec::new();
    for (appearance, theme) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_copilot_overage(cx)));
        settle(cx).await;
        cx.update(|cx| {
            cx.update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                for (id, used, bar) in ROWS {
                    let label = |name: String| {
                        window
                            .try_find(gpui_kit::ElementId::Name(name.into()))
                            .and_then(|found| found.label().map(str::to_owned))
                    };
                    assert_eq!(label(format!("used-{id}")).as_deref(), Some(used), "{id}");
                    assert_eq!(label(format!("progress-cell-{id}")).as_deref(), Some(bar), "{id}");
                }
            })
        })
        .expect("the Copilot fixture must reach the actual table");
        let name = format!("copilot-overage-usage-{theme}");
        assert!(save(cx, handle, dir, &name));
        saved.push(name);
    }
    saved
}
