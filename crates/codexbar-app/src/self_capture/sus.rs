//! Deterministic SUS evidence through the running native demo, with isolated settings and no provider reads.
use super::{save, settle};
use crate::{dashboard::Dashboard, theme::Appearance};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    let base = "2026-10-23T14:00:00Z".parse().unwrap();
    let mut saved = Vec::new();
    let started = Instant::now();
    let mut timestamps = Vec::new();
    for (appearance, suffix) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        for (phase, name, expected) in [
            (0, "weighted", 63),
            (1, "downgraded", 65),
            (2, "unknown-price", 72),
            (3, "fresh-window", 64),
            (4, "verified-x", 74),
            (5, "two-codex-subscriptions", 70),
        ] {
            cx.update(|cx| dashboard.update(cx, |d, cx| d.capture_sus_phase(phase, base, cx)));
            settle(cx).await;
            cx.update(|cx| {
                cx.update_window(handle, |_, window, cx| {
                    window.render_frame(cx);
                    assert_eq!(
                        window.find("fleet-sus").label(),
                        Some(format!("Fleet SUS: SUS {expected}, red").as_str())
                    );
                    assert_eq!(
                        window.find("focus-metric-1-sus").label(),
                        Some(if phase == 3 {
                            "Argon weekly: SUS n/a, unavailable"
                        } else {
                            "Argon weekly: SUS 84, yellow"
                        })
                    );
                })
            })
            .expect("SUS fixture values must reach the actual rendered header and focus view");
            let name = format!("sus-{name}-{suffix}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
            for _ in 0..4 {
                assert!(save(cx, handle, dir, &format!("sus-frame-{:04}", timestamps.len())));
                timestamps.push(started.elapsed().as_secs_f64());
                cx.background_executor().timer(Duration::from_millis(150)).await;
            }
        }
    }
    cx.update(|cx| {
        use codexbar_store::settings::{AccountRecord, AuthMethod, names};
        let mut record = AccountRecord::new(names::CLAUDE, "Demo billing change", AuthMethod::Automatic);
        record.id = "sus-price-fixture".into();
        record.sus_price = Some(codexbar_core::sus::Price {
            monthly_cents: Some(20_000),
            scheduled: Some((Some(10_000), "2026-10-26T13:33:19Z".parse().unwrap())),
        });
        crate::settings_hub::SettingsHub::update(cx, |s| s.upsert(record)).unwrap();
        cx.update_window(handle, |_, window, cx| {
            crate::sus_price::open("sus-price-fixture", window, cx)
        })
        .unwrap();
    });
    settle(cx).await;
    assert!(save(cx, handle, dir, "sus-price-editor-dark"));
    saved.push("sus-price-editor-dark".into());
    std::fs::write(
        dir.join("sus-recording.json"),
        serde_json::to_string(&timestamps).unwrap(),
    )
    .unwrap();
    saved
}
