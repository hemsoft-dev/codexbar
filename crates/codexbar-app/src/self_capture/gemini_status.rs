//! Status evidence from sanitized saved quota data, with missing Models simulated explicitly.
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
        for (present, state) in [(false, "missing"), (true, "present")] {
            cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_gemini_status(present, cx)));
            settle(cx).await;
            cx.update(|cx| {
                cx.update_window(handle, |_, w, cx| {
                    w.render_frame(cx);
                    let id = if wide {
                        "window-weekly-gemini-bar-sus"
                    } else {
                        "progress-cell-gemini-sus"
                    };
                    assert_eq!(
                        w.find(id).label(),
                        Some(if present {
                            "Gemini Models weekly: SUS 25, red"
                        } else {
                            "Quota unavailable: SUS n/a, unavailable"
                        })
                    );
                    assert!(w.try_find("severity-tag-gemini").is_none());
                    assert_eq!(w.find("fleet-sus").label().unwrap().contains("SUS 25"), present);
                    assert_eq!(w.try_find("gemini-models-weekly-unavailable").is_some(), !present);
                })
            })
            .unwrap();
            let name = format!("gemini-status-{state}-{theme}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
        }
    }
    saved
}
