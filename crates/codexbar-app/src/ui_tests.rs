//! Headless UI integration tests: the real dashboard in an in-process test window, driven by GPUI events.
//! Nothing here opens a desktop window, sends OS input or changes focus outside the test.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codexbar_store::credentials::MemoryCredentialStore;
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, InputEvent as _, Modifiers, ScrollDelta, ScrollWheelEvent,
    TestAppContext, point, px, size,
};

use crate::dashboard::{Dashboard, DashboardView, DataSource};
use crate::notifications::RecordingNotifier;
use crate::settings_hub::SettingsHub;
use crate::{theme, zoom};

/// A settings folder for one test, removed when the test ends.
struct TempSettings(PathBuf);

impl TempSettings {
    fn new(name: &str, json: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("codexbar-ui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), json).unwrap();
        Self(dir)
    }

    fn zoom_on_disk(&self) -> Option<f64> {
        let text = std::fs::read_to_string(self.0.join("settings.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        json.get("zoomLevel").and_then(serde_json::Value::as_f64)
    }
}

impl Drop for TempSettings {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Opens the demo dashboard the way `main` does, with settings from `settings`.
fn open_dashboard(cx: &mut TestAppContext, settings: &TempSettings) -> (AnyWindowHandle, Entity<Dashboard>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default()));
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
        zoom::init(cx);
        if cx.try_global::<crate::notifications::Notifications>().is_none() {
            // Demo data never persists alert state; tests that check the file install their own notifier.
            crate::notifications::Notifications::init(cx, Arc::new(RecordingNotifier::default()), false);
        }
    });
    let mut dashboard = None;
    let handle = cx.open_window(size(px(1440.), px(960.)), |window, cx| {
        let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
        dashboard = Some(view.clone());
        Root::new(view, window, cx)
    });
    let handle: AnyWindowHandle = handle.into();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    (handle, dashboard.unwrap())
}

fn press(cx: &mut TestAppContext, handle: AnyWindowHandle, key: &str) {
    cx.update_window(handle, |_, window, cx| window.press(key, cx)).unwrap();
    cx.run_until_parked();
}

/// One Ctrl+wheel event at the middle of the window. The test helpers' `scroll` sends no modifiers.
fn wheel(cx: &mut TestAppContext, handle: AnyWindowHandle, delta: ScrollDelta, control: bool) {
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(
            ScrollWheelEvent {
                position: point(px(720.), px(480.)),
                delta,
                modifiers: Modifiers {
                    control,
                    ..Modifiers::default()
                },
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn level(cx: &mut TestAppContext) -> f64 {
    cx.update(|cx| zoom::level(cx))
}

#[gpui_kit::test]
fn keyboard_shortcuts_step_and_reset_zoom(cx: &mut TestAppContext) {
    let settings = TempSettings::new("keys", r#"{ "zoomLevel": 1.0 }"#);
    let (handle, _) = open_dashboard(cx, &settings);
    assert_eq!(level(cx), 1.0);

    press(cx, handle, "ctrl-=");
    press(cx, handle, "ctrl-=");
    assert_eq!(level(cx), 1.2);
    press(cx, handle, "ctrl--");
    assert_eq!(level(cx), 1.1);
    press(cx, handle, "ctrl-0");
    assert_eq!(level(cx), 1.0);
}

#[gpui_kit::test]
fn ctrl_wheel_zooms_and_plain_wheel_does_not(cx: &mut TestAppContext) {
    let settings = TempSettings::new("wheel", r#"{ "zoomLevel": 1.0 }"#);
    let (handle, _) = open_dashboard(cx, &settings);
    // Pixel deltas don't depend on the machine's lines-per-notch setting: 50 px is one step.
    let pixels = |y: f32| ScrollDelta::Pixels(point(px(0.), px(y)));

    wheel(cx, handle, pixels(50.), false);
    assert_eq!(level(cx), 1.0, "a wheel without Ctrl scrolls instead of zooming");

    wheel(cx, handle, pixels(50.), true);
    wheel(cx, handle, pixels(50.), true);
    assert_eq!(level(cx), 1.2);
    wheel(cx, handle, pixels(-50.), true);
    assert_eq!(level(cx), 1.1);
}

#[gpui_kit::test]
fn zoom_is_saved_after_input_settles(cx: &mut TestAppContext) {
    let settings = TempSettings::new("save", r#"{ "zoomLevel": 1.0 }"#);
    let (handle, _) = open_dashboard(cx, &settings);

    press(cx, handle, "ctrl-=");
    assert_eq!(settings.zoom_on_disk(), Some(1.0), "the save waits for input to settle");
    cx.executor().advance_clock(Duration::from_millis(700));
    cx.run_until_parked();
    assert_eq!(settings.zoom_on_disk(), Some(1.1));
}

#[gpui_kit::test]
fn pending_zoom_is_saved_on_quit(cx: &mut TestAppContext) {
    let settings = TempSettings::new("quit", r#"{ "zoomLevel": 1.0 }"#);
    let (handle, _) = open_dashboard(cx, &settings);

    press(cx, handle, "ctrl-=");
    cx.update(|cx| cx.shutdown());
    assert_eq!(settings.zoom_on_disk(), Some(1.1));
}

#[gpui_kit::test]
fn view_tabs_switch_views_by_click_and_keyboard(cx: &mut TestAppContext) {
    let settings = TempSettings::new("tabs", "{}");
    let (handle, dashboard) = open_dashboard(cx, &settings);
    let view = |cx: &mut TestAppContext| cx.update(|cx| dashboard.read(cx).view());

    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find(("view-tab", 0usize)).selected(), Some(true));
        assert_eq!(window.find(("view-tab", 1usize)).selected(), Some(false));
        window.click(("view-tab", 1usize), cx);
    })
    .unwrap();
    assert_eq!(view(cx), DashboardView::Spend);

    // Keyboard: the arrow keys move between tabs and switch views, wrapping at the ends; Home and End jump.
    let focused = |cx: &mut TestAppContext, ix: usize| {
        cx.update_window(handle, |_, window, _| window.find(("view-tab", ix)).focused())
            .unwrap()
    };
    assert_eq!(focused(cx, 1), Some(true), "clicking a tab focuses it");
    press(cx, handle, "right");
    assert_eq!(view(cx), DashboardView::History);
    assert_eq!(focused(cx, 2), Some(true));
    press(cx, handle, "right");
    assert_eq!(view(cx), DashboardView::Usage);
    press(cx, handle, "left");
    assert_eq!(view(cx), DashboardView::History);
    press(cx, handle, "home");
    assert_eq!(view(cx), DashboardView::Usage);
    assert_eq!(focused(cx, 0), Some(true));
    press(cx, handle, "end");
    assert_eq!(view(cx), DashboardView::History);
    press(cx, handle, "ctrl-left");
    assert_eq!(view(cx), DashboardView::History, "modified arrows don't switch views");

    // Tab leaves the tab row instead of visiting every tab.
    press(cx, handle, "tab");
    assert!((0..3).all(|ix| focused(cx, ix) == Some(false)));
    // Shift+Tab comes back to the selected tab, not the last one in the row.
    press(cx, handle, "shift-tab");
    assert_eq!(focused(cx, 2), Some(true));
}

#[gpui_kit::test]
fn failed_save_reloads_from_the_injected_folder(cx: &mut TestAppContext) {
    use std::os::windows::fs::OpenOptionsExt as _;

    let settings = TempSettings::new("reload", r#"{ "zoomLevel": 1.3 }"#);
    open_dashboard(cx, &settings);
    // Hold the shared write lock the way another writer would, so the save fails with Busy and the hub reloads.
    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(settings.0.join("settings.write.lock"))
        .unwrap();

    let result = cx.update(|cx| {
        SettingsHub::update(cx, |settings| {
            settings.set_zoom_level(2.0);
            Ok(())
        })
    });
    assert_eq!(result, Err(codexbar_store::settings::SettingsError::Busy));
    let reloaded = cx.update(|cx| SettingsHub::global(cx).settings().zoom_level());
    assert_eq!(reloaded, 1.3, "the reload reads the test's folder, not ~/.codexbar");
    assert_eq!(settings.zoom_on_disk(), Some(1.3));
}

fn label(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> Option<String> {
    cx.update_window(handle, |_, window, _| {
        window.try_find(id).and_then(|found| found.label().map(str::to_owned))
    })
    .unwrap()
}

fn visible(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> bool {
    cx.update_window(handle, |_, window, _| {
        window.try_find(id).is_some_and(|found| found.visible())
    })
    .unwrap()
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| window.click(id, cx)).unwrap();
    cx.run_until_parked();
}

fn open_history(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    click(cx, handle, ("view-tab", 2usize));
}

fn hidden_on_disk(settings: &TempSettings) -> serde_json::Value {
    let text = std::fs::read_to_string(settings.0.join("dashboard.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["hiddenHistory"].clone()
}

#[gpui_kit::test]
fn history_view_shows_summary_and_chart_for_the_selected_account(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history", "{}");
    let (handle, dashboard) = open_dashboard(cx, &settings);
    open_history(cx, handle);

    let (view, _) = cx.update(|cx| dashboard.read(cx).history_parts());
    let shown = cx.update(|cx| view.read(cx).shown()).unwrap();
    // The Usage view's selection (the most urgent account) carries over.
    assert_eq!(shown, ("codex-personal".to_owned(), "5-hour-window".to_owned()));

    let chart = label(cx, handle, "history-chart-region").unwrap();
    assert!(
        chart.starts_with("5-hour window history, last 7 days. Latest 91%"),
        "{chart}"
    );
    assert!(chart.contains("Low ") && chart.contains("samples"), "{chart}");
    let readout = label(cx, handle, "history-readout").unwrap();
    assert!(
        readout.ends_with("91%"),
        "the readout starts on the latest point: {readout}"
    );
}

#[gpui_kit::test]
fn history_chart_points_can_be_read_with_the_keyboard(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history-keys", "{}");
    let (handle, _) = open_dashboard(cx, &settings);
    open_history(cx, handle);

    let latest = label(cx, handle, "history-readout").unwrap();
    click(cx, handle, "history-chart-region");
    let focused = cx
        .update_window(handle, |_, window, _| window.find("history-chart-region").focused())
        .unwrap();
    assert_eq!(focused, Some(true));

    press(cx, handle, "left");
    let previous = label(cx, handle, "history-readout").unwrap();
    assert_ne!(previous, latest, "Left moves to the previous point");
    press(cx, handle, "home");
    let first = label(cx, handle, "history-readout").unwrap();
    assert_ne!(first, previous);
    press(cx, handle, "end");
    assert_eq!(label(cx, handle, "history-readout").unwrap(), latest);
    press(cx, handle, "ctrl-left");
    assert_eq!(
        label(cx, handle, "history-readout").unwrap(),
        latest,
        "modified keys are left alone"
    );
}

#[gpui_kit::test]
fn history_metric_and_range_can_be_changed(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history-controls", "{}");
    let (handle, dashboard) = open_dashboard(cx, &settings);
    open_history(cx, handle);
    let (view, _) = cx.update(|cx| dashboard.read(cx).history_parts());

    click(cx, handle, ("history-metric", 1usize));
    let shown = cx.update(|cx| view.read(cx).shown()).unwrap();
    assert_eq!(shown.1, "weekly");
    let week = label(cx, handle, "history-chart-region").unwrap();
    assert!(week.starts_with("Weekly history, last 7 days."), "{week}");

    click(cx, handle, ("history-range", 0usize));
    let day = label(cx, handle, "history-chart-region").unwrap();
    assert!(day.starts_with("Weekly history, last 24 hours."), "{day}");
}

#[gpui_kit::test]
fn hiding_history_keeps_the_data_and_is_saved(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history-hide", "{}");
    let (handle, dashboard) = open_dashboard(cx, &settings);
    open_history(cx, handle);
    let (_, history) = cx.update(|cx| dashboard.read(cx).history_parts());
    let before = history.lock().unwrap().len();

    click(cx, handle, "history-show");
    assert_eq!(hidden_on_disk(&settings), serde_json::json!(["codex-personal"]));
    assert!(
        label(cx, handle, "history-chart-region").is_none(),
        "the chart is replaced"
    );
    assert!(visible(cx, handle, "history-empty"));
    assert_eq!(history.lock().unwrap().len(), before, "hiding deletes nothing");

    click(cx, handle, "history-show");
    assert!(label(cx, handle, "history-chart-region").is_some());
    assert_eq!(hidden_on_disk(&settings), serde_json::json!([]));
}

#[gpui_kit::test]
fn history_view_handles_empty_and_single_sample_series(cx: &mut TestAppContext) {
    use codexbar_core::demo::demo_accounts;
    use codexbar_store::HistoryStore;
    use codexbar_store::summary::Point;
    use std::sync::Mutex;

    let settings = TempSettings::new("history-edge", "{}");
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
    });
    let now = chrono::Utc::now();
    let accounts = demo_accounts(now, &chrono::Local);
    let history = Arc::new(Mutex::new(HistoryStore::in_memory(chrono::Duration::days(30))));
    let mut view = None;
    let handle = cx.open_window(size(px(1200.), px(800.)), |window, cx| {
        let entity = cx.new(|cx| crate::history_view::HistoryView::new(history.clone(), cx));
        view = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    let handle: AnyWindowHandle = handle.into();
    let view = view.unwrap();
    cx.update(|cx| view.update(cx, |view, cx| view.set_accounts(accounts.clone(), None, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();

    // No samples: an explanation, no chart, no zeros.
    assert!(label(cx, handle, "history-chart-region").is_none());
    assert!(visible(cx, handle, "history-empty"));

    // One sample: shown as a value and described as the only sample.
    history.lock().unwrap().insert_points(
        "codex-personal",
        "5-hour-window",
        &[Point::new(now - chrono::Duration::minutes(10), 0.4)],
    );
    cx.update(|cx| view.update(cx, |_, cx| cx.notify()));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let chart = label(cx, handle, "history-chart-region").unwrap();
    assert!(chart.ends_with("Latest 40%. Only one sample so far."), "{chart}");
}

#[gpui_kit::test]
fn a_used_up_window_with_credits_shows_the_credits_with_no_pill(cx: &mut TestAppContext) {
    let settings = TempSettings::new("credits-row", "{}");
    let (handle, _) = open_dashboard(cx, &settings);
    assert!(
        label(cx, handle, "provider-badge-codex-team").is_some(),
        "the table shows the account"
    );
    // The demo's team account: weekly at 100%, 18,400 credits left of a 50,000 start.
    assert_eq!(
        label(cx, handle, "used-codex-team").as_deref(),
        Some("50,000 credits: 18,400 left")
    );
    assert_eq!(
        label(cx, handle, "progress-cell-codex-team").as_deref(),
        Some("63% used")
    );
    assert!(label(cx, handle, "severity-tag-codex-team").is_none(), "no pill");
}

#[gpui_kit::test]
fn a_used_up_window_without_credits_is_blocked_with_no_pill(cx: &mut TestAppContext) {
    let settings = TempSettings::new("credits-none", "{}");
    let (handle, _) = open_dashboard(cx, &settings);
    assert!(
        label(cx, handle, "provider-badge-codex-work").is_some(),
        "the table shows the account"
    );
    assert_eq!(label(cx, handle, "used-codex-work").as_deref(), Some("Weekly: 100%"));
    assert_eq!(
        label(cx, handle, "progress-cell-codex-work").as_deref(),
        Some("100% used")
    );
    assert!(
        label(cx, handle, "severity-tag-codex-work").is_none(),
        "no pill at 100%"
    );
    // Below 100% the pill stays: the personal account's 5-hour window at 91% is about to run out.
    assert_eq!(
        label(cx, handle, "severity-tag-codex-personal").as_deref(),
        Some("Limit soon")
    );
    let blocked = cx.update(|_| {
        codexbar_core::demo::demo_accounts(chrono::Utc::now(), &chrono::Local)
            .into_iter()
            .find(|account| account.id().as_str() == "codex-work")
            .unwrap()
            .assess(chrono::Utc::now())
    });
    assert_eq!(blocked.severity(), codexbar_core::Severity::LimitSoon, "still blocked");
}

#[gpui_kit::test]
fn table_trend_shows_compact_history_until_hidden(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history-compact", "{}");
    let (handle, _) = open_dashboard(cx, &settings);
    let cell = "trend-cell-codex-personal";

    let compact = label(cx, handle, cell).unwrap();
    assert!(
        compact.starts_with("ChatGPT · Codex (personal) trend. Last 14 days: Latest 91%"),
        "{compact}"
    );
    assert!(compact.contains("Low ") && compact.contains("samples"), "{compact}");

    open_history(cx, handle);
    click(cx, handle, "history-show");

    // Back on the Usage view, the row is there but its trend is gone.
    click(cx, handle, ("view-tab", 0usize));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert!(
        label(cx, handle, "provider-badge-codex-personal").is_some(),
        "the table is showing the account"
    );
    assert!(label(cx, handle, cell).is_none(), "a hidden account shows no trend");
}

#[gpui_kit::test]
fn history_follows_the_usage_selection_until_an_account_is_picked(cx: &mut TestAppContext) {
    let settings = TempSettings::new("history-follow", "{}");
    let (handle, dashboard) = open_dashboard(cx, &settings);
    let (view, _) = cx.update(|cx| dashboard.read(cx).history_parts());
    let shown = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).shown()).unwrap().0;
    let row_id = |cx: &mut TestAppContext, ix: usize| cx.update(|cx| dashboard.read(cx).account_id(ix)).unwrap();

    // Selecting another Usage row after the first load still carries over.
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.select_row(2, cx)));
    cx.run_until_parked();
    let third = row_id(cx, 2);
    open_history(cx, handle);
    assert_eq!(shown(cx), third);

    // Picking an account in History pins it; later Usage selections don't move it.
    click(cx, handle, ("history-account", 4usize));
    let picked = row_id(cx, 4);
    assert_eq!(shown(cx), picked);
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.select_row(1, cx)));
    cx.run_until_parked();
    assert_eq!(shown(cx), picked);
}

#[gpui_kit::test]
fn single_balance_account_preferences_move_to_its_configured_id(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "legacy-ids",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "or-1", "providerId": "OpenRouter", "displayLabel": "OpenRouter", "enabled": true,
              "authenticationMethod": "ApiKey" } ] }"#,
    );
    std::fs::write(
        settings.0.join("dashboard.json"),
        r#"{ "version": 1, "hiddenHistory": ["openrouter"] }"#,
    )
    .unwrap();
    cx.update(|cx| {
        SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default()));
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
    });

    // One configured OpenRouter account owns the legacy history; Moonshot has no records, so its implicit
    // account keeps reporting under the legacy id and nothing moves for it.
    let renames = cx.update(|cx| crate::providers::legacy_ids(SettingsHub::global(cx)));
    assert_eq!(renames, vec![("openrouter", "or-1".to_owned())]);
    let configured = renames[0].1.clone();

    cx.update(|cx| crate::prefs_hub::PrefsHub::rename_accounts(cx, &renames));
    let shows = |cx: &mut TestAppContext, id: &str| cx.update(|cx| crate::prefs_hub::PrefsHub::shows(cx, id));
    assert!(
        !shows(cx, &configured),
        "the hidden preference follows the account to its configured id"
    );
    assert!(shows(cx, "openrouter"));
    assert_eq!(hidden_on_disk(&settings), serde_json::json!([configured]));
}

#[gpui_kit::test]
fn legacy_history_is_not_moved_when_several_accounts_could_own_it(cx: &mut TestAppContext) {
    // Two OpenRouter accounts, only one enabled: the legacy history may belong to either, so it stays put.
    let settings = TempSettings::new(
        "legacy-ambiguous",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "old", "providerId": "OpenRouter", "displayLabel": "Old", "enabled": false,
              "authenticationMethod": "ApiKey" },
            { "id": "new", "providerId": "OpenRouter", "displayLabel": "New", "enabled": true,
              "authenticationMethod": "ApiKey" },
            { "id": "kimi", "providerId": "Moonshot", "displayLabel": "Kimi", "enabled": false,
              "authenticationMethod": "ApiKey" } ] }"#,
    );
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let renames = cx.update(|cx| crate::providers::legacy_ids(SettingsHub::global(cx)));
    // Moonshot's one account is disabled but still the only owner, so its legacy id moves to it.
    assert_eq!(renames, vec![("moonshot", "kimi".to_owned())]);
}

fn legacy_history_after_migration(cx: &mut TestAppContext, settings_json: &str, name: &str) -> (usize, usize) {
    use codexbar_store::HistoryStore;
    use codexbar_store::summary::Point;
    use std::sync::Mutex;

    let settings = TempSettings::new(name, settings_json);
    cx.update(|cx| {
        SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default()));
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
    });
    let mut store = HistoryStore::in_memory(chrono::Duration::days(30));
    store.insert_points("openrouter", "credits", &[Point::new(chrono::Utc::now(), 12.5)]);
    let history = Mutex::new(store);
    cx.update(|cx| crate::dashboard::migrate_legacy_ids(&history, cx));
    let store = history.lock().unwrap();
    let legacy = store.series("openrouter", "credits").count();
    (legacy, store.len() - legacy)
}

#[gpui_kit::test]
fn legacy_history_moves_only_when_settings_are_readable(cx: &mut TestAppContext) {
    // Readable settings with one configured OpenRouter account: it owns the legacy history.
    let one = r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "or-1", "providerId": "OpenRouter", "displayLabel": "OpenRouter", "enabled": true,
              "authenticationMethod": "ApiKey" } ] }"#;
    assert_eq!(legacy_history_after_migration(cx, one, "migrate-readable"), (0, 1));
}

#[gpui_kit::test]
fn legacy_history_stays_put_while_no_account_is_configured(cx: &mut TestAppContext) {
    // The implicit account still reports under the legacy id, so moving its history would orphan it once the first
    // account is configured with a new id.
    assert_eq!(legacy_history_after_migration(cx, "{}", "migrate-implicit"), (1, 0));
}

#[gpui_kit::test]
fn legacy_history_stays_put_when_settings_are_from_a_newer_version(cx: &mut TestAppContext) {
    // A newer schema can't be read, so the hub falls back to defaults; nothing may move on their say-so.
    let newer = r#"{ "accountConfigurationVersion": 2, "accounts": [] }"#;
    assert_eq!(legacy_history_after_migration(cx, newer, "migrate-newer"), (1, 0));
}

const ALERTS_ON: &str = r#"{ "version": 1, "alerts": { "enabled": true, "usageThreshold": 0.8, "balanceThreshold": 10,
    "warning": true, "critical": true } }"#;

/// Opens the demo dashboard with alert settings in `dashboard.json` and a recording notifier.
fn open_with_alerts(cx: &mut TestAppContext, settings: &TempSettings) -> (Entity<Dashboard>, Arc<RecordingNotifier>) {
    let notifier = Arc::new(RecordingNotifier::default());
    let recorder = notifier.clone();
    cx.update(|cx| crate::notifications::Notifications::init(cx, recorder, true));
    let (_, dashboard) = open_dashboard(cx, settings);
    (dashboard, notifier)
}

fn active_on_disk(settings: &TempSettings) -> Vec<String> {
    let text = std::fs::read_to_string(settings.0.join("dashboard.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["activeAlerts"]
        .as_array()
        .map(|keys| keys.iter().filter_map(|key| key.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn refresh(cx: &mut TestAppContext, dashboard: &Entity<Dashboard>) {
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.refresh(cx)));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn alerts_notify_once_per_condition_and_survive_a_restart(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (dashboard, notifier) = open_with_alerts(cx, &settings);

    let shown: Vec<String> = notifier
        .shown
        .lock()
        .unwrap()
        .iter()
        .map(|alert| alert.title.clone())
        .collect();
    // The demo's Codex 5-hour window is at 91% and about to run out; Moonshot has $7.85 left, under $10.
    let has = |title: &str| shown.iter().any(|shown| shown == title);
    assert!(has("ChatGPT · Codex (personal): 5-hour window at 91%"), "{shown:?}");
    assert!(has("ChatGPT · Codex (personal): 5-hour window limit soon"), "{shown:?}");
    assert!(
        !has("ChatGPT · Codex (personal): 5-hour window at risk"),
        "covered by limit soon"
    );
    assert!(has("Moonshot (Kimi): $7.85 left"), "{shown:?}");
    let active = active_on_disk(&settings);
    assert!(
        active.len() > shown.len(),
        "delivered alerts and the conditions they cover are remembered"
    );

    // Another refresh with the same conditions notifies nothing new.
    refresh(cx, &dashboard);
    assert_eq!(notifier.shown.lock().unwrap().len(), shown.len());

    // A restart reads the remembered alerts and stays quiet too.
    cx.update(|cx| crate::prefs_hub::PrefsHub::init(cx, &settings.0));
    refresh(cx, &dashboard);
    assert_eq!(notifier.shown.lock().unwrap().len(), shown.len());
}

#[gpui_kit::test]
fn alerts_are_off_until_enabled(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-off", "{}");
    let (dashboard, notifier) = open_with_alerts(cx, &settings);
    assert!(notifier.shown.lock().unwrap().is_empty());

    cx.update(|cx| crate::prefs_hub::PrefsHub::update_alert_settings(cx, |alerts| alerts.enabled = true));
    refresh(cx, &dashboard);
    assert!(!notifier.shown.lock().unwrap().is_empty());
}

#[gpui_kit::test]
fn blocked_notifications_are_retried_once_allowed(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-blocked", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let notifier = Arc::new(RecordingNotifier::default());
    *notifier.blocked.lock().unwrap() = Some("Notifications are off for CodexBar.".into());
    let recorder = notifier.clone();
    cx.update(|cx| crate::notifications::Notifications::init(cx, recorder, true));
    let (_, dashboard) = open_dashboard(cx, &settings);

    assert!(notifier.shown.lock().unwrap().is_empty());
    assert!(active_on_disk(&settings).is_empty(), "nothing is marked as delivered");
    let problem = cx.update(|cx| crate::notifications::Notifications::problem(cx));
    assert_eq!(problem.as_deref(), Some("Notifications are off for CodexBar."));

    *notifier.blocked.lock().unwrap() = None;
    refresh(cx, &dashboard);
    assert!(
        !notifier.shown.lock().unwrap().is_empty(),
        "allowed again, the alerts arrive"
    );
    assert_eq!(cx.update(|cx| crate::notifications::Notifications::problem(cx)), None);
}

#[gpui_kit::test]
fn reset_lets_active_conditions_notify_again(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-reset", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (dashboard, notifier) = open_with_alerts(cx, &settings);
    let first = notifier.shown.lock().unwrap().len();
    assert!(first > 0);

    cx.update(crate::notifications::Notifications::reset);
    assert!(active_on_disk(&settings).is_empty());
    refresh(cx, &dashboard);
    assert_eq!(notifier.shown.lock().unwrap().len(), first * 2);
}

#[gpui_kit::test]
fn alerts_settings_page_shows_status_and_actions(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-page", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (dashboard, _) = open_with_alerts(cx, &settings);
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    // The Alerts page is the third item in the settings sidebar.
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-2", cx)
    })
    .unwrap();
    cx.run_until_parked();
    let status = label(cx, handle, "alerts-status").unwrap();
    assert!(
        status.starts_with("Windows notifications are on.") && status.contains("active"),
        "{status}"
    );

    click(cx, handle, "alerts-reset");
    let status = label(cx, handle, "alerts-status").unwrap();
    assert_eq!(status, "Windows notifications are on. No alerts are active.");
}

#[gpui_kit::test]
fn demo_alerts_never_touch_dashboard_json(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-demo", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let notifier = Arc::new(RecordingNotifier::default());
    let recorder = notifier.clone();
    cx.update(|cx| crate::notifications::Notifications::init(cx, recorder, false));
    let (dashboard, _) = {
        let (handle, dashboard) = open_dashboard(cx, &settings);
        (dashboard, handle)
    };
    assert!(
        !notifier.shown.lock().unwrap().is_empty(),
        "the demo still evaluates and records alerts"
    );
    assert!(
        active_on_disk(&settings).is_empty(),
        "but its synthetic keys stay in memory"
    );
    // And remembers them in memory, so a second demo refresh doesn't repeat them.
    let shown = notifier.shown.lock().unwrap().len();
    refresh(cx, &dashboard);
    assert_eq!(notifier.shown.lock().unwrap().len(), shown);
}

#[gpui_kit::test]
fn a_notification_windows_failed_to_raise_is_sent_again(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-failed", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (dashboard, notifier) = open_with_alerts(cx, &settings);
    let first = notifier.shown.lock().unwrap().len();
    let lost = notifier.shown.lock().unwrap()[0].clone();

    // Windows accepted the first toast but later raised `Failed` for it; the clock resends it without a refresh.
    notifier.failed.lock().unwrap().extend(lost.keys().cloned());
    cx.executor().advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    let shown = notifier.shown.lock().unwrap();
    assert_eq!(shown.len(), first + 1, "only the failed one is sent again");
    assert_eq!(shown.last().unwrap().title, lost.title);
    let _ = dashboard;
}

#[gpui_kit::test]
fn changing_a_threshold_judges_held_alerts_afresh(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-threshold", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (_, _) = open_with_alerts(cx, &settings);
    let has_usage = |keys: &[String]| keys.iter().any(|key| key.ends_with("|usage"));
    assert!(has_usage(&active_on_disk(&settings)));

    cx.update(|cx| crate::prefs_hub::PrefsHub::update_alert_settings(cx, |alerts| alerts.usage_threshold = 0.95));
    let active = active_on_disk(&settings);
    assert!(!has_usage(&active), "usage alerts are re-judged against 95%");
    assert!(
        active.iter().any(|key| key.ends_with("|balance")),
        "other kinds keep theirs"
    );
}

#[gpui_kit::test]
fn alerts_of_an_account_that_disappears_stay_until_reset(cx: &mut TestAppContext) {
    use codexbar_core::alerts::{AlertKind, alert_key};
    let key = alert_key("removed-account", "weekly", AlertKind::Usage);
    let kept = cx.update(|cx| {
        crate::notifications::Notifications::init(cx, Arc::new(RecordingNotifier::default()), false);
        crate::notifications::Notifications::seed_for_test(cx, [key.clone()].into());
        // A refresh without that account (removed, disabled or its provider failed) leaves its key alone.
        crate::notifications::process(cx, &[], chrono::Utc::now());
        crate::notifications::Notifications::active(cx)
    });
    assert!(kept.contains(&key));
    cx.update(crate::notifications::Notifications::reset);
    assert!(
        cx.update(|cx| crate::notifications::Notifications::active(cx))
            .is_empty()
    );
}

#[gpui_kit::test]
fn a_test_notification_windows_failed_to_raise_shows_why(cx: &mut TestAppContext) {
    let notifier = Arc::new(RecordingNotifier::default());
    let recorder = notifier.clone();
    let problem = cx.update(|cx| {
        crate::notifications::Notifications::init(cx, recorder, false);
        crate::notifications::Notifications::send_test(cx);
        // Windows accepted the sample, then raised `Failed` for it (it has no alert key).
        notifier.failed.lock().unwrap().push(String::new());
        *notifier.reason.lock().unwrap() = Some("Windows couldn't show a notification: access denied".into());
        crate::notifications::retry_failed(cx, &[], chrono::Utc::now());
        crate::notifications::Notifications::problem(cx)
    });
    assert_eq!(
        problem.as_deref(),
        Some("Windows couldn't show a notification: access denied")
    );
    assert!(
        cx.update(|cx| crate::notifications::Notifications::active(cx))
            .is_empty(),
        "no empty key is kept"
    );
}

#[gpui_kit::test]
fn a_successful_resend_clears_the_failure_message(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-resend-clears", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let (_, notifier) = open_with_alerts(cx, &settings);
    let lost = notifier.shown.lock().unwrap()[0].clone();

    notifier.failed.lock().unwrap().extend(lost.keys().cloned());
    *notifier.reason.lock().unwrap() = Some("Windows couldn't show a notification: busy".into());
    cx.executor().advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| crate::notifications::Notifications::problem(cx)),
        None,
        "the resend went through"
    );
}

#[gpui_kit::test]
fn a_blocked_message_clears_once_notifications_are_on_again(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-unblock", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let notifier = Arc::new(RecordingNotifier::default());
    *notifier.blocked.lock().unwrap() = Some("Notifications are off for CodexBar.".into());
    let recorder = notifier.clone();
    cx.update(|cx| crate::notifications::Notifications::init(cx, recorder, true));
    let (_, dashboard) = open_dashboard(cx, &settings);
    assert!(
        cx.update(|cx| crate::notifications::Notifications::problem(cx))
            .is_some()
    );

    // The user turns alerts off, then notifications back on: nothing to send, but the message is out of date.
    cx.update(|cx| crate::prefs_hub::PrefsHub::update_alert_settings(cx, |alerts| alerts.enabled = false));
    *notifier.blocked.lock().unwrap() = None;
    refresh(cx, &dashboard);
    assert_eq!(cx.update(|cx| crate::notifications::Notifications::problem(cx)), None);
}

#[gpui_kit::test]
fn demo_preferences_stay_in_memory(cx: &mut TestAppContext) {
    let dir = TempSettings::new("prefs-demo", "{}");
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::init_in_memory(cx);
        crate::prefs_hub::PrefsHub::update_alert_settings(cx, |alerts| alerts.enabled = true);
    });
    assert!(
        cx.update(|cx| crate::prefs_hub::PrefsHub::alert_settings(cx)).enabled,
        "applies in memory"
    );
    assert!(!dir.0.join("dashboard.json").exists(), "nothing is written");
}

#[gpui_kit::test]
fn alerts_pause_while_preferences_cannot_be_saved(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alerts-readonly", "{}");
    std::fs::write(
        settings.0.join("dashboard.json"),
        r#"{ "version": 2, "alerts": { "enabled": true, "usageThreshold": 0.8, "balanceThreshold": 10 } }"#,
    )
    .unwrap();
    let (_, notifier) = open_with_alerts(cx, &settings);
    assert!(
        notifier.shown.lock().unwrap().is_empty(),
        "nothing is sent that couldn't be remembered"
    );
    let problem = cx
        .update(|cx| crate::notifications::Notifications::problem(cx))
        .unwrap();
    assert!(problem.starts_with("Alerts are paused"), "{problem}");
}

// --- Refresh lifecycle (#76): a live dashboard over fake providers.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider};
use codexbar_providers::{ProviderError, UsageProvider};

use crate::account_table::AccountState;

/// A provider whose next result the test controls: `Some(used)` returns one account, `None` fails.
struct FakeProvider {
    name: &'static str,
    /// The configured account this adapter serves, like a second OpenRouter account.
    account: Option<&'static str>,
    kind: Provider,
    id: &'static str,
    next: Mutex<Option<f64>>,
    calls: AtomicUsize,
    /// A credential the adapter holds; it must never reach snapshots.json.
    _key: &'static str,
}

impl FakeProvider {
    fn new(kind: Provider, id: &'static str, used: Option<f64>) -> Arc<Self> {
        Arc::new(Self {
            name: kind.display_name(),
            account: None,
            kind,
            id,
            next: Mutex::new(used),
            calls: AtomicUsize::new(0),
            _key: "sk-test-secret-key",
        })
    }

    /// An adapter for one configured account, reporting under that account's id.
    fn for_account(kind: Provider, id: &'static str, used: Option<f64>) -> Arc<Self> {
        let mut provider = Arc::try_unwrap(Self::new(kind, id, used)).ok().unwrap();
        provider.account = Some(id);
        Arc::new(provider)
    }

    fn set(&self, used: Option<f64>) {
        *self.next.lock().unwrap() = used;
    }

    /// Makes the next fetch succeed with an account that has no metrics (unlimited quotas).
    fn set_empty(&self) {
        *self.next.lock().unwrap() = Some(f64::NAN);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl UsageProvider for FakeProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn account_id(&self) -> Option<&str> {
        self.account
    }

    fn account_label(&self) -> Option<&str> {
        self.account.map(|id| if id == "or-1" { "Personal" } else { "Team" })
    }

    fn fetch(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match *self.next.lock().unwrap() {
            Some(used) if used.is_nan() => Ok(vec![AccountSnapshot::new(
                AccountId::new(self.id),
                self.kind,
                Vec::new(),
                now,
            )]),
            Some(used) => Ok(vec![AccountSnapshot::new(
                AccountId::new(self.id),
                self.kind,
                vec![Metric::Window {
                    label: "Weekly".into(),
                    used,
                    resets_at: fake_resets_at(),
                    pace: None,
                }],
                now,
            )]),
            None => Err(ProviderError::Network),
        }
    }
}

/// The reset time the fake providers report: three days ahead, fixed for the whole test run. Computing it per fetch
/// gave every fetch a different reset, so history recorded the same usage twice whenever two fetches fell in
/// different seconds, which made history-count tests fail at random on slower machines.
fn fake_resets_at() -> chrono::DateTime<chrono::Utc> {
    static RESETS_AT: std::sync::OnceLock<chrono::DateTime<chrono::Utc>> = std::sync::OnceLock::new();
    *RESETS_AT.get_or_init(|| chrono::Utc::now() + chrono::Duration::days(3))
}

/// Opens a live dashboard over `providers` without letting its first fetch run yet.
fn open_live(cx: &mut TestAppContext, settings: &TempSettings, providers: Vec<Arc<FakeProvider>>) -> Entity<Dashboard> {
    open_live_with(cx, settings, providers, MemoryCredentialStore::default())
}

/// `open_live` with a chosen credential store, such as one that always fails.
fn open_live_with(
    cx: &mut TestAppContext,
    settings: &TempSettings,
    providers: Vec<Arc<FakeProvider>>,
    store: MemoryCredentialStore,
) -> Entity<Dashboard> {
    let providers = providers
        .into_iter()
        .map(|provider| provider as Arc<dyn UsageProvider>)
        .collect();
    open_live_dyn(cx, settings, providers, store)
}

/// `open_live` with any providers, such as one returning a real parser's output.
fn open_live_fixed(
    cx: &mut TestAppContext,
    settings: &TempSettings,
    providers: Vec<Arc<dyn UsageProvider>>,
) -> Entity<Dashboard> {
    open_live_dyn(cx, settings, providers, MemoryCredentialStore::default())
}

fn open_live_dyn(
    cx: &mut TestAppContext,
    settings: &TempSettings,
    providers: Vec<Arc<dyn UsageProvider>>,
    store: MemoryCredentialStore,
) -> Entity<Dashboard> {
    use codexbar_store::HistoryStore;
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        SettingsHub::init_with(cx, &settings.0, Arc::new(store));
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
        zoom::init(cx);
        crate::notifications::Notifications::init(cx, Arc::new(RecordingNotifier::default()), false);
        // Never the real Codex CLI or browser; tests that sign in install their own fake.
        if cx.try_global::<crate::codex_sign_in::Service>().is_none() {
            crate::codex_sign_in::init_with(cx, Arc::new(codex_fixture::FakeCodexSignIn::default()));
        }
        if cx.try_global::<crate::cursor_sign_in::Service>().is_none() {
            crate::cursor_sign_in::init_with(cx, Arc::new(cursor_fixture::FakeCursor::default()));
        }
        if cx.try_global::<crate::claude_sign_in::Service>().is_none() {
            crate::claude_sign_in::init_with(cx, Arc::new(claude_fixture::FakeClaude::default()));
        }
        if cx.try_global::<crate::github_sign_in::Service>().is_none() {
            crate::github_sign_in::init_with(cx, Arc::new(github_fixture::FakeGitHub::default()));
        }
        if cx.try_global::<crate::opencode_sign_in::Service>().is_none() {
            crate::opencode_sign_in::init_with(cx, Arc::new(opencode_fixture::FakeOpenCode::default()));
        }
    });
    let factory: crate::dashboard::ProviderFactory = Arc::new(move |_| providers.clone());
    let mut dashboard = None;
    cx.open_window(size(px(1440.), px(960.)), |window, cx| {
        let source = DataSource::Live {
            history: Arc::new(Mutex::new(HistoryStore::in_memory(chrono::Duration::days(30)))),
            providers: factory,
        };
        let view = cx.new(|cx| Dashboard::new(source, window, cx));
        dashboard = Some(view.clone());
        Root::new(view, window, cx)
    });
    dashboard.unwrap()
}

fn ids(cx: &mut TestAppContext, dashboard: &Entity<Dashboard>) -> Vec<String> {
    cx.update(|cx| dashboard.read(cx).account_ids())
}

fn state(cx: &mut TestAppContext, dashboard: &Entity<Dashboard>, id: &str) -> AccountState {
    cx.update(|cx| dashboard.read(cx).state(id))
}

#[gpui_kit::test]
fn restored_snapshots_show_before_the_first_fetch(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-restore", "{}");
    let saved = AccountSnapshot::new(
        AccountId::new("claude-1"),
        Provider::Claude,
        Vec::new(),
        chrono::Utc::now(),
    );
    codexbar_store::snapshots::save_snapshots(&settings.0, &[saved]).unwrap();
    let claude = FakeProvider::new(Provider::Claude, "claude-1", Some(0.4));
    let dashboard = open_live(cx, &settings, vec![claude.clone()]);

    assert_eq!(
        ids(cx, &dashboard),
        vec!["claude-1"],
        "the saved account is shown at once"
    );
    assert_eq!(state(cx, &dashboard, "claude-1"), AccountState::Restored);
    cx.run_until_parked();
    assert_eq!(claude.calls(), 1);
    assert_eq!(state(cx, &dashboard, "claude-1"), AccountState::Fresh);
}

#[gpui_kit::test]
fn configured_providers_show_a_placeholder_until_their_first_result(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-placeholder", "{}");
    let cursor = FakeProvider::new(Provider::Cursor, "cursor-1", Some(0.2));
    let dashboard = open_live(cx, &settings, vec![cursor]);

    assert_eq!(ids(cx, &dashboard), vec!["pending:cursor"]);
    assert_eq!(state(cx, &dashboard, "pending:cursor"), AccountState::Loading);
    cx.run_until_parked();
    assert_eq!(
        ids(cx, &dashboard),
        vec!["cursor-1"],
        "the placeholder is replaced by the real account"
    );
}

#[gpui_kit::test]
fn overlapping_refreshes_run_one_at_a_time_with_one_follow_up(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-coalesce", "{}");
    let codex = FakeProvider::new(Provider::Codex, "codex-1", Some(0.3));
    let dashboard = open_live(cx, &settings, vec![codex.clone()]);

    // The startup fetch is running; three more requests coalesce into a single follow-up batch.
    for _ in 0..3 {
        cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.refresh(cx)));
    }
    cx.run_until_parked();
    assert_eq!(codex.calls(), 2, "the startup batch plus one queued follow-up");
}

#[gpui_kit::test]
fn a_failed_refresh_keeps_last_good_usage_marked_stale(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-failure", "{}");
    let claude = FakeProvider::new(Provider::Claude, "claude-1", Some(0.6));
    let dashboard = open_live(cx, &settings, vec![claude.clone()]);
    cx.run_until_parked();

    claude.set(None);
    refresh(cx, &dashboard);
    assert_eq!(ids(cx, &dashboard), vec!["claude-1"], "still shown");
    match state(cx, &dashboard, "claude-1") {
        AccountState::Failed(error) => assert!(!error.is_empty()),
        other => panic!("expected a stale account, got {other:?}"),
    }
    assert_eq!(cx.update(|cx| dashboard.read(cx).failed_providers()), vec!["Claude"]);
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let tag = label(cx, handle, "state-claude-1").unwrap();
    assert!(tag.starts_with("Stale. Refresh failed:"), "{tag}");
}

#[gpui_kit::test]
fn retry_fetches_only_the_failed_provider(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-retry", "{}");
    let claude = FakeProvider::new(Provider::Claude, "claude-1", None);
    let cursor = FakeProvider::new(Provider::Cursor, "cursor-1", Some(0.2));
    let dashboard = open_live(cx, &settings, vec![claude.clone(), cursor.clone()]);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| dashboard.read(cx).failed_providers()), vec!["Claude"]);

    claude.set(Some(0.5));
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    click(cx, handle, "retry-Claude");
    assert_eq!((claude.calls(), cursor.calls()), (2, 1), "only Claude is fetched again");
    assert!(cx.update(|cx| dashboard.read(cx).failed_providers()).is_empty());
    let mut shown = ids(cx, &dashboard);
    shown.sort();
    assert_eq!(
        shown,
        vec!["claude-1", "cursor-1"],
        "the other provider's account stays"
    );
    assert_eq!(state(cx, &dashboard, "claude-1"), AccountState::Fresh);
}

#[gpui_kit::test]
fn snapshots_are_saved_after_a_refresh_without_secrets(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-persist", "{}");
    let codex = FakeProvider::new(Provider::Codex, "codex-1", Some(0.3));
    let _dashboard = open_live(cx, &settings, vec![codex]);
    cx.run_until_parked();

    let text = std::fs::read_to_string(settings.0.join("snapshots.json")).unwrap();
    assert!(text.contains("\"codex-1\""));
    assert!(!text.contains("sk-test-secret"), "only usage is stored");
    assert!(!text.contains("pending:"), "placeholders aren't stored");
    let restored = codexbar_store::snapshots::load_snapshots(&settings.0);
    assert_eq!(restored.len(), 1);
}

#[gpui_kit::test]
fn the_widget_snapshot_follows_each_refresh_without_secrets(cx: &mut TestAppContext) {
    use codexbar_store::widgets::{WidgetHealth, load_widget_snapshot};
    let settings = TempSettings::new("widgets-feed", "{}");
    let codex = FakeProvider::new(Provider::Codex, "codex-1", Some(0.3));
    let _dashboard = open_live(cx, &settings, vec![codex]);
    cx.run_until_parked();

    // The widget provider reads this file (#94): the account, its display value and that it is current.
    let snapshot = load_widget_snapshot(&settings.0).expect("the dashboard wrote widgets.json");
    assert_eq!(snapshot.accounts.len(), 1);
    let account = &snapshot.accounts[0];
    assert_eq!(account.id, "codex-1");
    assert_eq!(account.health, WidgetHealth::Fresh);
    assert!(account.metrics.iter().any(|metric| metric.used_percent == Some(30.0)));
    let text = std::fs::read_to_string(settings.0.join("widgets.json")).unwrap();
    assert!(!text.contains("sk-test-secret"), "only display values are written");
}

#[gpui_kit::test]
fn each_configured_account_of_one_provider_loads_and_fails_on_its_own(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-siblings", "{}");
    let first = FakeProvider::for_account(Provider::OpenRouter, "or-1", Some(0.4));
    let second = FakeProvider::for_account(Provider::OpenRouter, "or-2", None);
    let dashboard = open_live(cx, &settings, vec![first, second]);

    let mut before = ids(cx, &dashboard);
    before.sort();
    assert_eq!(before, vec!["or-1", "or-2"], "a Loading row per configured account");
    cx.run_until_parked();

    let mut after = ids(cx, &dashboard);
    after.sort();
    assert_eq!(after, vec!["or-1", "or-2"], "no duplicates");
    assert_eq!(
        state(cx, &dashboard, "or-1"),
        AccountState::Fresh,
        "a sibling's failure doesn't touch it"
    );
    assert!(matches!(state(cx, &dashboard, "or-2"), AccountState::Unavailable(_)));
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let tag = label(cx, handle, "state-or-2").unwrap();
    assert!(
        tag.starts_with("Unavailable. Couldn't fetch usage:"),
        "a failed first fetch isn't stale: {tag}"
    );
}

#[gpui_kit::test]
fn snapshots_of_accounts_no_longer_enabled_are_not_restored(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-restore-filter", "{}");
    let now = chrono::Utc::now();
    let kept = AccountSnapshot::new(AccountId::new("claude-1"), Provider::Claude, Vec::new(), now);
    let gone = AccountSnapshot::new(AccountId::new("cursor-1"), Provider::Cursor, Vec::new(), now);
    codexbar_store::snapshots::save_snapshots(&settings.0, &[kept, gone]).unwrap();
    // Only Claude is configured now.
    let dashboard = open_live(
        cx,
        &settings,
        vec![FakeProvider::new(Provider::Claude, "claude-1", Some(0.2))],
    );
    assert_eq!(ids(cx, &dashboard), vec!["claude-1"]);
}

#[gpui_kit::test]
fn retrying_one_account_leaves_its_siblings_and_the_schedule_alone(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-retry-account", "{}");
    let first = FakeProvider::for_account(Provider::OpenRouter, "or-1", Some(0.4));
    let second = FakeProvider::for_account(Provider::OpenRouter, "or-2", None);
    let dashboard = open_live(cx, &settings, vec![first.clone(), second.clone()]);
    cx.run_until_parked();
    let refreshed_at = cx.update(|cx| dashboard.read(cx).last_refresh_for_test());

    second.set(Some(0.3));
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    click(cx, handle, "retry-OpenRouter-or-2");
    assert_eq!(
        (first.calls(), second.calls()),
        (1, 2),
        "only the failed account is fetched again"
    );
    assert_eq!(state(cx, &dashboard, "or-2"), AccountState::Fresh);
    assert_eq!(state(cx, &dashboard, "or-1"), AccountState::Fresh);
    assert_eq!(
        cx.update(|cx| dashboard.read(cx).last_refresh_for_test()),
        refreshed_at,
        "a retry doesn't push back the next full refresh"
    );
}

#[gpui_kit::test]
fn a_real_account_without_metrics_is_saved_like_any_other(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-empty-account", "{}");
    // Usage `None` makes the fake fail; an account with no metrics is a success with nothing to show.
    let copilot = FakeProvider::new(Provider::Copilot, "copilot-octocat", Some(0.0));
    copilot.set_empty();
    let _dashboard = open_live(cx, &settings, vec![copilot]);
    cx.run_until_parked();
    let saved = codexbar_store::snapshots::load_snapshots(&settings.0);
    assert_eq!(saved.len(), 1, "it is a real account, not a placeholder");
    assert_eq!(saved[0].id().as_str(), "copilot-octocat");
}

#[gpui_kit::test]
fn configured_accounts_fetched_successfully_are_saved(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-save-configured", "{}");
    let first = FakeProvider::for_account(Provider::OpenRouter, "or-1", Some(0.4));
    let _dashboard = open_live(cx, &settings, vec![first]);
    cx.run_until_parked();
    let saved = codexbar_store::snapshots::load_snapshots(&settings.0);
    let ids: Vec<&str> = saved.iter().map(|account| account.id().as_str()).collect();
    assert_eq!(
        ids,
        vec!["or-1"],
        "its placeholder shared the id, but it is a real account now"
    );
}

#[gpui_kit::test]
fn sibling_accounts_are_named_by_their_labels(cx: &mut TestAppContext) {
    let settings = TempSettings::new("lifecycle-labels", "{}");
    let first = FakeProvider::for_account(Provider::OpenRouter, "or-1", None);
    let second = FakeProvider::for_account(Provider::OpenRouter, "or-2", None);
    let dashboard = open_live(cx, &settings, vec![first, second]);
    let names = |cx: &mut TestAppContext| {
        let mut names = cx.update(|cx| dashboard.read(cx).display_names_for_test());
        names.sort();
        names
    };
    assert_eq!(
        names(cx),
        vec!["OpenRouter · Personal", "OpenRouter · Team"],
        "placeholders carry the labels"
    );
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let retry = cx
        .update_window(handle, |_, window, _| {
            window.find("retry-OpenRouter-or-2").label().map(str::to_owned)
        })
        .unwrap();
    assert_eq!(retry.as_deref(), Some("Retry OpenRouter · Team"));
}

/// A provider serving several users that reports each user's outcome, like Copilot with two GitHub accounts.
struct FakeMultiProvider {
    outcomes: Mutex<Vec<(&'static str, Option<f64>)>>,
}

impl UsageProvider for FakeMultiProvider {
    fn name(&self) -> &'static str {
        Provider::Copilot.display_name()
    }

    fn fetch(&self, _: chrono::DateTime<chrono::Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        unreachable!("the dashboard asks for per-account outcomes")
    }

    fn fetch_outcomes(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<codexbar_providers::AccountOutcome>, ProviderError> {
        use codexbar_providers::AccountOutcome;
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .iter()
            .map(|(user, used)| match used {
                Some(used) => AccountOutcome::Fresh(
                    AccountSnapshot::new(
                        AccountId::new(format!("copilot-{user}")),
                        Provider::Copilot,
                        vec![Metric::Window {
                            label: "Weekly".into(),
                            used: *used,
                            resets_at: fake_resets_at(),
                            pace: None,
                        }],
                        now,
                    )
                    .with_label(*user),
                ),
                None => AccountOutcome::Failed {
                    account: AccountId::new(format!("copilot-{user}")),
                    label: Some((*user).to_owned()),
                    error: ProviderError::Network,
                },
            })
            .collect())
    }
}

#[gpui_kit::test]
fn one_user_failing_keeps_its_last_good_usage_and_snapshot(cx: &mut TestAppContext) {
    use codexbar_store::HistoryStore;
    let settings = TempSettings::new("outcomes-partial", "{}");
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default()));
        crate::prefs_hub::PrefsHub::init(cx, &settings.0);
        zoom::init(cx);
        crate::notifications::Notifications::init(cx, Arc::new(RecordingNotifier::default()), false);
    });
    let provider = Arc::new(FakeMultiProvider {
        outcomes: Mutex::new(vec![("ada", Some(0.3)), ("bob", Some(0.6))]),
    });
    let factory_provider = provider.clone();
    let factory: crate::dashboard::ProviderFactory =
        Arc::new(move |_| vec![factory_provider.clone() as Arc<dyn UsageProvider>]);
    let mut dashboard = None;
    cx.open_window(size(px(1440.), px(960.)), |window, cx| {
        let source = DataSource::Live {
            history: Arc::new(Mutex::new(HistoryStore::in_memory(chrono::Duration::days(30)))),
            providers: factory,
        };
        let view = cx.new(|cx| Dashboard::new(source, window, cx));
        dashboard = Some(view.clone());
        Root::new(view, window, cx)
    });
    let dashboard = dashboard.unwrap();
    cx.run_until_parked();

    // Bob's next fetch fails while Ada's succeeds.
    *provider.outcomes.lock().unwrap() = vec![("ada", Some(0.4)), ("bob", None)];
    refresh(cx, &dashboard);
    let mut shown = ids(cx, &dashboard);
    shown.sort();
    assert_eq!(
        shown,
        vec!["copilot-ada", "copilot-bob"],
        "Bob stays with his last good usage"
    );
    assert_eq!(state(cx, &dashboard, "copilot-ada"), AccountState::Fresh);
    assert!(matches!(state(cx, &dashboard, "copilot-bob"), AccountState::Failed(_)));
    let saved: Vec<String> = codexbar_store::snapshots::load_snapshots(&settings.0)
        .iter()
        .map(|account| account.id().as_str().to_owned())
        .collect();
    assert!(
        saved.contains(&"copilot-bob".to_owned()),
        "his snapshot survives a restart"
    );
}

#[gpui_kit::test]
fn provider_messages_show_with_the_focused_account(cx: &mut TestAppContext) {
    let settings = TempSettings::new("messages", "{}");
    let saved = AccountSnapshot::new(
        AccountId::new("claude-1"),
        Provider::Claude,
        Vec::new(),
        chrono::Utc::now(),
    )
    .with_message("Usage is delayed by up to an hour");
    codexbar_store::snapshots::save_snapshots(&settings.0, &[saved]).unwrap();
    // The provider is down, so the restored snapshot (and its message) stays.
    let _dashboard = open_live(
        cx,
        &settings,
        vec![FakeProvider::new(Provider::Claude, "claude-1", None)],
    );
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let message = cx
        .update_window(handle, |_, window, _| {
            window
                .try_find(("provider-message", 0usize))
                .and_then(|found| found.label().map(str::to_owned))
        })
        .unwrap();
    assert_eq!(message.as_deref(), Some("Usage is delayed by up to an hour"));
}

/// The aria label of the element with `id` in the first window, after a fresh frame.
fn label_of(cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId> + Clone) -> Option<String> {
    let handle = cx.windows()[0];
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(handle, |_, window, _| {
        window
            .try_find(id.clone())
            .and_then(|found| found.label().map(str::to_owned))
    })
    .unwrap()
}

#[gpui_kit::test]
fn the_focused_account_shows_its_alert_and_keeps_it_after_notifying(cx: &mut TestAppContext) {
    let settings = TempSettings::new("alert-details", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let provider = FakeProvider::new(Provider::Claude, "claude-1", Some(0.86));
    let dashboard = open_live(cx, &settings, vec![provider.clone()]);
    cx.run_until_parked();
    let strongest = label_of(cx, "alert-strongest").expect("an alert is shown");
    assert!(
        strongest.starts_with("Usage alert: Weekly. 86% · Alert at 80% · resets in "),
        "{strongest}"
    );
    // A refresh that sends no new notification keeps the alert on screen.
    refresh(cx, &dashboard);
    assert!(label_of(cx, "alert-strongest").is_some());
    // Once usage falls below the recovery line, it goes.
    provider.set(Some(0.4));
    refresh(cx, &dashboard);
    assert_eq!(label_of(cx, "alert-strongest"), None);
    assert_eq!(
        label_of(cx, "projected-status"),
        None,
        "observed usage, not a projection"
    );
}

const MANUAL_ORDER: &str = r#"{ "version": 1, "orderMode": "manual" }"#;

fn shown_ids(cx: &mut TestAppContext, dashboard: &Entity<Dashboard>) -> Vec<String> {
    cx.update(|cx| dashboard.read(cx).account_ids())
}

#[gpui_kit::test]
fn groups_arrange_the_table_and_are_saved(cx: &mut TestAppContext) {
    let settings = TempSettings::new("groups", "{}");
    std::fs::write(settings.0.join("dashboard.json"), MANUAL_ORDER).unwrap();
    let dashboard = open_live(
        cx,
        &settings,
        vec![
            FakeProvider::new(Provider::Cursor, "cursor", Some(0.9)),
            FakeProvider::new(Provider::Claude, "claude-1", Some(0.2)),
            FakeProvider::new(Provider::Codex, "codex", Some(0.5)),
        ],
    );
    cx.run_until_parked();
    // Without groups: provider order, not fetch order or urgency.
    assert_eq!(shown_ids(cx, &dashboard), ["codex", "claude-1", "cursor"]);
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_layout(cx, |layout| {
            let work = layout.create_group("Work")?;
            layout.assign("cursor", Some(&work))
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        shown_ids(cx, &dashboard),
        ["cursor", "codex", "claude-1"],
        "Work, then Ungrouped"
    );
    let saved = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert_eq!(saved.layout().group_of("cursor").map(|g| g.name.as_str()), Some("Work"));
}

#[gpui_kit::test]
fn move_down_reorders_the_focused_account_within_its_group(cx: &mut TestAppContext) {
    let settings = TempSettings::new("groups-move", "{}");
    std::fs::write(settings.0.join("dashboard.json"), MANUAL_ORDER).unwrap();
    let dashboard = open_live(
        cx,
        &settings,
        vec![
            FakeProvider::new(Provider::Codex, "codex", Some(0.5)),
            FakeProvider::new(Provider::Claude, "claude-1", Some(0.2)),
        ],
    );
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.select_row(0, cx)));
    cx.run_until_parked();
    click(cx, handle, "move-down");
    assert_eq!(shown_ids(cx, &dashboard), ["claude-1", "codex"]);
    let saved = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert_eq!(saved.layout().order(), ["claude-1".to_owned(), "codex".to_owned()]);
}

#[gpui_kit::test]
fn smart_order_puts_urgent_accounts_first_and_manual_comes_back(cx: &mut TestAppContext) {
    let settings = TempSettings::new("smart-order", "{}");
    let dashboard = open_live(
        cx,
        &settings,
        vec![
            FakeProvider::new(Provider::Codex, "codex", Some(0.2)),
            FakeProvider::new(Provider::Claude, "claude-1", Some(0.97)),
            FakeProvider::new(Provider::Cursor, "cursor", Some(0.85)),
        ],
    );
    cx.run_until_parked();
    // Smart by default: Limit soon, then Watch, then calm.
    assert_eq!(shown_ids(cx, &dashboard), ["claude-1", "cursor", "codex"]);
    // Manual shows the default order; Smart never wrote to it.
    let handle = cx.windows()[0];
    click(cx, handle, ("order-mode", 1usize));
    assert_eq!(shown_ids(cx, &dashboard), ["codex", "claude-1", "cursor"]);
    let saved = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert_eq!(saved.layout().mode(), codexbar_core::layout::OrderMode::Manual);
    assert!(saved.layout().order().is_empty());
    click(cx, handle, ("order-mode", 0usize));
    assert_eq!(shown_ids(cx, &dashboard), ["claude-1", "cursor", "codex"]);
}

#[gpui_kit::test]
fn smart_order_keeps_failed_accounts_visible_after_current_ones(cx: &mut TestAppContext) {
    let settings = TempSettings::new("smart-failed", "{}");
    let codex = FakeProvider::new(Provider::Codex, "codex", Some(0.1));
    let claude = FakeProvider::new(Provider::Claude, "claude-1", Some(0.1));
    let dashboard = open_live(cx, &settings, vec![codex.clone(), claude.clone()]);
    cx.run_until_parked();
    assert_eq!(shown_ids(cx, &dashboard), ["codex", "claude-1"]);
    // Codex fails: its last known usage stays visible, below the account whose usage is current.
    codex.set(None);
    refresh(cx, &dashboard);
    assert_eq!(shown_ids(cx, &dashboard), ["claude-1", "codex"]);
}

#[gpui_kit::test]
fn smart_order_re_ranks_when_held_alerts_change(cx: &mut TestAppContext) {
    let settings = TempSettings::new("smart-alerts", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    // Both under the 80% alert; Codex is busier, so it leads.
    let dashboard = open_live(
        cx,
        &settings,
        vec![
            FakeProvider::new(Provider::Claude, "claude-1", Some(0.77)),
            FakeProvider::new(Provider::Codex, "codex", Some(0.78)),
        ],
    );
    cx.run_until_parked();
    assert_eq!(shown_ids(cx, &dashboard), ["codex", "claude-1"]);
    // Claude's usage alert is held (it dipped inside the recovery margin): it now outranks Codex, without a refresh.
    let key = cx.update(|cx| {
        let account = dashboard.read(cx).account("claude-1").unwrap();
        codexbar_core::alerts::alert_key(
            "claude-1",
            &codexbar_core::alerts::metric_slot(&account.metrics()[0]),
            codexbar_core::alerts::AlertKind::Usage,
        )
    });
    cx.update(|cx| crate::notifications::Notifications::seed_for_test(cx, [key].into_iter().collect()));
    cx.run_until_parked();
    assert_eq!(shown_ids(cx, &dashboard), ["claude-1", "codex"]);
}

// Settings (#91): navigation, refresh choices, the account dialog by keyboard, cancellation, removal, reset and
// secure-store failures, all headless.

fn open_settings_page(
    cx: &mut TestAppContext,
    settings: &TempSettings,
    store: MemoryCredentialStore,
    page: usize,
) -> AnyWindowHandle {
    let dashboard = open_live_with(cx, settings, vec![], store);
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click(format!("0-{page}"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    handle
}

fn type_text(cx: &mut TestAppContext, handle: AnyWindowHandle, text: &str) {
    cx.update_window(handle, |_, window, cx| window.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}

fn saved_settings(settings: &TempSettings) -> codexbar_store::settings::Settings {
    codexbar_store::settings::Settings::load_or_default(&settings.0)
}

fn secret_of(cx: &mut TestAppContext, id: &str) -> Option<String> {
    cx.update(|cx| SettingsHub::global(cx).credentials().read(id)).unwrap()
}

/// Adds an OpenRouter account through the dialog, by keyboard: the Name field has focus with its text selected, and
/// Tab moves past the sign-in method to the key.
fn add_openrouter(cx: &mut TestAppContext, handle: AnyWindowHandle, name: &str, key: &str) {
    click(cx, handle, "add-OpenRouter");
    type_text(cx, handle, name);
    press(cx, handle, "tab");
    press(cx, handle, "tab");
    type_text(cx, handle, key);
    click(cx, handle, "account-save");
}

fn exists(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) -> bool {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .unwrap()
}

#[gpui_kit::test]
fn every_settings_section_can_be_opened(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-nav", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 0);
    // General, Accounts, Alerts, Groups, Appearance, Widgets, About: each page shows its own controls.
    let marks: [(usize, Option<&'static str>); 7] = [
        (0, None),
        (1, Some("add-OpenRouter")),
        (2, Some("alerts-status")),
        (3, Some("group-new")),
        (4, None),
        (5, None),
        (6, Some("open-repo")),
    ];
    for (page, mark) in marks {
        cx.update_window(handle, |_, window, cx| {
            window.within("settings-sidebar").click(format!("0-{page}"), cx)
        })
        .unwrap();
        cx.run_until_parked();
        if let Some(mark) = mark {
            assert!(exists(cx, handle, mark), "page {page} shows {mark}");
        }
    }
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-0", cx)
    })
    .unwrap();
    cx.run_until_parked();
    let refresh = cx
        .update_window(handle, |_, window, _| {
            window
                .within("group-0")
                .within("item-0")
                .within("field")
                .find("btn")
                .label()
                .map(str::to_owned)
        })
        .unwrap();
    assert_eq!(
        refresh.as_deref(),
        Some("Every 2 minutes"),
        "General shows auto refresh"
    );
}

fn update_status(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Option<String> {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find("update-status")
            .and_then(|found| found.label().map(str::to_owned))
    })
    .unwrap()
}

// The widget builder (#95): Settings › Widgets, page 5.

fn open_widget_builder(cx: &mut TestAppContext, settings: &TempSettings) -> (Entity<Dashboard>, AnyWindowHandle) {
    let codex = FakeProvider::new(Provider::Codex, "codex-1", Some(0.3));
    let claude = FakeProvider::new(Provider::Claude, "claude-1", Some(0.8));
    let dashboard = open_live(cx, settings, vec![codex, claude]);
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-5", cx)
    })
    .unwrap();
    cx.run_until_parked();
    (dashboard, handle)
}

/// Opens the dropdown of `item` in `group` and picks the option `downs` places down (the first Down is the first).
fn choose(cx: &mut TestAppContext, handle: AnyWindowHandle, group: usize, item: usize, downs: usize) {
    cx.update_window(handle, |_, window, cx| {
        window
            .within(format!("group-{group}"))
            .within(format!("item-{item}"))
            .within("field")
            .click("btn", cx)
    })
    .unwrap();
    cx.run_until_parked();
    for _ in 0..downs {
        press(cx, handle, "down");
    }
    press(cx, handle, "enter");
}

fn builder(cx: &mut TestAppContext) -> codexbar_store::widgets::WidgetBuilder {
    cx.update(|cx| crate::prefs_hub::PrefsHub::widget_builder(cx))
}

#[gpui_kit::test]
fn the_widget_builder_adds_tiles_sets_modes_and_saves(cx: &mut TestAppContext) {
    use codexbar_store::widgets::TileMode;
    let settings = TempSettings::new("widget-builder", "{}");
    let (_dashboard, handle) = open_widget_builder(cx, &settings);

    // My tiles starts with only Add a tile; its second option is the first limit (after the placeholder).
    choose(cx, handle, 1, 0, 2);
    let tiles = builder(cx).tiles;
    assert_eq!(tiles.len(), 1, "one tile added");
    assert_eq!(tiles[0].mode, TileMode::Automatic);
    let first = tiles[0].clone();

    // The tile's dropdown: Automatic, Compact percentage, Full bar, Balance only, Urgent status, Remove tile.
    choose(cx, handle, 1, 0, 3);
    assert_eq!(builder(cx).tiles[0].mode, TileMode::Bar);

    // A second tile, then move it up.
    choose(cx, handle, 1, 1, 2);
    assert_eq!(builder(cx).tiles.len(), 2);
    assert_ne!(builder(cx).tiles[1], first);
    choose(cx, handle, 1, 1, 6);
    assert_eq!(
        builder(cx).tiles[1],
        codexbar_store::widgets::WidgetTile {
            mode: TileMode::Bar,
            ..first.clone()
        }
    );

    // Saved in dashboard.json, and sent to the widgets in widgets.json.
    let saved = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert_eq!(saved.widget_builder().tiles.len(), 2);
    let snapshot = codexbar_store::widgets::load_widget_snapshot(&settings.0).unwrap();
    assert_eq!(
        snapshot.builder.tiles,
        builder(cx).tiles,
        "the widgets get the tiles at once"
    );

    // Remove the first tile.
    choose(cx, handle, 1, 0, 6);
    assert_eq!(builder(cx).tiles.len(), 1);
}

#[gpui_kit::test]
fn the_widget_preview_shows_each_layout(cx: &mut TestAppContext) {
    use codexbar_store::widgets::{TileMode, WidgetTile};
    let settings = TempSettings::new("widget-preview", "{}");
    let (_dashboard, handle) = open_widget_builder(cx, &settings);
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_widget_builder(cx, |builder| {
            for (account, mode) in [
                ("codex-1", TileMode::Percent),
                ("claude-1", TileMode::Status),
                ("gone", TileMode::Bar),
            ] {
                let snapshot = cx_metric(account);
                builder.add(WidgetTile {
                    account: account.into(),
                    metric: snapshot,
                    mode,
                });
            }
        })
    });
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-5-3", cx)
    })
    .unwrap();
    cx.run_until_parked();

    // Automatic previews all three sizes: one tile on small, two on medium, every tile on large.
    let label = |cx: &mut TestAppContext, id: (&'static str, usize)| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).and_then(|found| found.label().map(str::to_owned))
        })
        .unwrap()
    };
    assert_eq!(
        label(cx, ("widget-preview", 0)).as_deref(),
        Some("Small widget preview, 1 tiles")
    );
    assert_eq!(
        label(cx, ("widget-preview", 1)).as_deref(),
        Some("Medium widget preview, 2 tiles")
    );
    assert_eq!(
        label(cx, ("widget-preview", 2)).as_deref(),
        Some("Large widget preview, 3 tiles")
    );
    let removed = label(cx, ("preview-tile", 22)).unwrap();
    assert!(removed.starts_with("Removed account"), "{removed}");

    // Four tiles: one medium preview.
    click(cx, handle, ("preview-layout", 3usize));
    assert_eq!(
        label(cx, ("widget-preview", 1)).as_deref(),
        Some("Medium widget preview, 3 tiles")
    );
    assert!(label(cx, ("widget-preview", 0)).is_none());
    click(cx, handle, ("preview-layout", 1usize));
    assert_eq!(
        label(cx, ("widget-preview", 1)).as_deref(),
        Some("Medium widget preview, 1 tiles")
    );
}

/// The metric key the fake providers report (their Weekly window).
fn cx_metric(_account: &str) -> String {
    "weekly".to_owned()
}

#[gpui_kit::test]
fn widget_settings_reset_after_confirming(cx: &mut TestAppContext) {
    use codexbar_store::widgets::{WidgetRefresh, WidgetTile};
    let settings = TempSettings::new("widget-reset", "{}");
    let (_dashboard, handle) = open_widget_builder(cx, &settings);
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_widget_builder(cx, |builder| {
            builder.add(WidgetTile {
                account: "codex-1".into(),
                metric: cx_metric("codex-1"),
                mode: Default::default(),
            });
            builder.refresh = WidgetRefresh::FifteenMinutes;
        })
    });
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-5-4", cx)
    })
    .unwrap();
    cx.run_until_parked();
    click(cx, handle, "reset-widgets");
    press(cx, handle, "escape");
    assert_eq!(builder(cx).tiles.len(), 1, "cancel keeps the tiles");
    click(cx, handle, "reset-widgets");
    press(cx, handle, "enter");
    assert_eq!(builder(cx), codexbar_store::widgets::WidgetBuilder::default());
    assert_eq!(
        codexbar_store::prefs::DashboardPrefs::load(&settings.0).widget_builder(),
        &codexbar_store::widgets::WidgetBuilder::default()
    );
}

#[gpui_kit::test]
fn a_widget_tile_opens_its_account_and_metric(cx: &mut TestAppContext) {
    use crate::handoff::FocusRequest;
    let settings = TempSettings::new("widget-focus", "{}");
    let (dashboard, _handle) = open_widget_builder(cx, &settings);
    let request = FocusRequest {
        account: "claude-1".into(),
        metric: Some(cx_metric("claude-1")),
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    let (selected, view, history) = cx.update(|cx| {
        let dashboard = dashboard.read(cx);
        (dashboard.selected_id(), dashboard.view(), dashboard.history_shown(cx))
    });
    assert_eq!(selected.as_deref(), Some("claude-1"));
    assert_eq!(view, DashboardView::Usage, "Settings gives way to the account");
    assert_eq!(history, Some(("claude-1".to_owned(), cx_metric("claude-1"))));

    // A tile for a removed account still opens CodexBar, on Usage.
    let gone = FocusRequest {
        account: "gone".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&gone, cx)));
    assert_eq!(cx.update(|cx| dashboard.read(cx).view()), DashboardView::Usage);
    assert_eq!(
        cx.update(|cx| dashboard.read(cx).selected_id()).as_deref(),
        Some("claude-1")
    );
}

#[gpui_kit::test]
fn about_shows_the_build_and_where_updates_come_from(cx: &mut TestAppContext) {
    use crate::package::{UpdateState, Updates};
    let settings = TempSettings::new("settings-about", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 6);
    // Tests run unpackaged: no channel to check, so Check now does nothing and there is nothing to install.
    let source = UpdateState::NotPackaged.label();
    assert_eq!(update_status(cx, handle).as_deref(), Some(source.as_str()));
    assert!(!exists(cx, handle, "install-update"));
    click(cx, handle, "check-updates");
    assert_eq!(
        update_status(cx, handle).as_deref(),
        Some(source.as_str()),
        "nothing to check"
    );

    // A package from the channel with a newer version offers to install it.
    cx.update(|cx| Updates::set_for_test(cx, UpdateState::Available { required: false }));
    let available = UpdateState::Available { required: false }.label();
    assert_eq!(update_status(cx, handle).as_deref(), Some(available.as_str()));
    assert!(exists(cx, handle, "install-update"));

    // A failed check says why, and can be tried again.
    cx.update(|cx| Updates::set_for_test(cx, UpdateState::Failed("The network path was not found.".into())));
    assert_eq!(
        update_status(cx, handle).as_deref(),
        Some("Couldn't update: The network path was not found.")
    );
    assert!(!exists(cx, handle, "install-update"));
    assert!(exists(cx, handle, "report-problem"));
}

#[gpui_kit::test]
fn auto_refresh_offers_the_standard_choices_and_saves_them(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-refresh", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 0);
    let choose = |cx: &mut TestAppContext, downs: usize| {
        cx.update_window(handle, |_, window, cx| {
            window
                .within("group-0")
                .within("item-0")
                .within("field")
                .click("btn", cx)
        })
        .unwrap();
        cx.run_until_parked();
        for _ in 0..downs {
            press(cx, handle, "down");
        }
        press(cx, handle, "enter");
    };
    // The WPF default of 2 minutes is kept as its own choice: Off, 1, 2, 5, 15, 30, 60. The first is Off.
    choose(cx, 1);
    assert_eq!(saved_settings(&settings).refresh_interval_secs(), None);
    // Now the standard six: Off, 1, 5, 15, 30, 60.
    choose(cx, 6);
    assert_eq!(saved_settings(&settings).refresh_interval_secs(), Some(3600));
    choose(cx, 3);
    assert_eq!(saved_settings(&settings).refresh_interval_secs(), Some(300));
}

#[gpui_kit::test]
fn an_account_added_by_keyboard_is_saved_with_its_key(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-add", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    add_openrouter(cx, handle, "Work", "sk-or-test");
    let saved = saved_settings(&settings);
    let record = saved
        .accounts()
        .iter()
        .find(|a| a.provider == "OpenRouter")
        .expect("saved");
    assert_eq!(record.label, "Work");
    assert_eq!(secret_of(cx, &record.id).as_deref(), Some("sk-or-test"));
    let text = std::fs::read_to_string(settings.0.join("settings.json")).unwrap();
    assert!(
        !text.contains("sk-or-test"),
        "the key is never written to settings.json"
    );
    let detail = label(
        cx,
        handle,
        Box::leak(format!("account-detail-{}", record.id).into_boxed_str()),
    );
    // An OPENROUTER_API_KEY in the environment wins over the saved key, and the detail says so. Never the key.
    let expected = if std::env::var_os("OPENROUTER_API_KEY").is_some() {
        "API key · From the OPENROUTER_API_KEY environment variable"
    } else {
        "API key · Saved in Windows Credential Manager"
    };
    assert_eq!(detail.as_deref(), Some(expected));
}

#[gpui_kit::test]
fn cancelling_the_account_dialog_saves_nothing(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-cancel", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    click(cx, handle, "add-OpenRouter");
    type_text(cx, handle, "Draft");
    press(cx, handle, "escape");
    assert!(!exists(cx, handle, "account-save"), "the dialog closed");
    assert!(saved_settings(&settings).accounts().is_empty());
}

#[gpui_kit::test]
fn accounts_can_be_renamed_disabled_and_removed_after_confirming(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-edit", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    add_openrouter(cx, handle, "Work", "sk-or-test");
    let id = saved_settings(&settings).accounts()[0].id.clone();

    click(cx, handle, format!("edit-{id}"));
    type_text(cx, handle, "Personal");
    click(cx, handle, "account-save");
    assert_eq!(saved_settings(&settings).accounts()[0].label, "Personal");
    assert_eq!(
        secret_of(cx, &id).as_deref(),
        Some("sk-or-test"),
        "a blank key keeps the saved one"
    );

    click(cx, handle, format!("enabled-{id}"));
    assert!(!saved_settings(&settings).accounts()[0].enabled);

    // Removing asks first: Escape keeps the account, Enter removes it and its key.
    click(cx, handle, format!("remove-{id}"));
    press(cx, handle, "escape");
    assert_eq!(saved_settings(&settings).accounts().len(), 1);
    click(cx, handle, format!("remove-{id}"));
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    assert_eq!(secret_of(cx, &id), None);
}

#[gpui_kit::test]
fn reset_accounts_asks_first_then_removes_accounts_and_keys(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-reset", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    add_openrouter(cx, handle, "Work", "sk-one");
    add_openrouter(cx, handle, "Team", "sk-two");
    let ids: Vec<String> = saved_settings(&settings)
        .accounts()
        .iter()
        .map(|a| a.id.clone())
        .collect();
    assert_eq!(ids.len(), 2);
    // Reset is the last group on the Accounts page; its sidebar entry scrolls it into view.
    let show_reset = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            window.within("settings-sidebar").click("0-1-8", cx)
        })
        .unwrap();
        cx.run_until_parked();
    };
    show_reset(cx);
    click(cx, handle, "reset-accounts");
    press(cx, handle, "escape");
    assert_eq!(saved_settings(&settings).accounts().len(), 2, "cancelled");
    show_reset(cx);
    click(cx, handle, "reset-accounts");
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    for id in ids {
        assert_eq!(secret_of(cx, &id), None);
    }
}

#[gpui_kit::test]
fn secure_store_failures_are_shown_and_nothing_is_saved_without_its_key(cx: &mut TestAppContext) {
    let settings = TempSettings::new("settings-store-fail", "{}");
    let handle = open_settings_page(cx, &settings, MemoryCredentialStore::failing(), 1);
    add_openrouter(cx, handle, "Work", "sk-secret-value");
    let error = label(cx, handle, "account-error").expect("the dialog says why");
    assert_eq!(error, "Windows Credential Manager failed (error 5).");
    assert!(!error.contains("sk-secret-value"));
    assert!(exists(cx, handle, "account-save"), "the dialog stays open");
    assert!(
        saved_settings(&settings).accounts().is_empty(),
        "no account without its key"
    );
}

/// A provider that returns fixed accounts, such as a provider's real parser output.
struct FixedProvider(Vec<AccountSnapshot>);

impl UsageProvider for FixedProvider {
    fn name(&self) -> &'static str {
        "Claude"
    }

    fn fetch(&self, _: chrono::DateTime<chrono::Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        Ok(self.0.clone())
    }
}

#[gpui_kit::test]
fn every_claude_limit_and_credit_shows_on_the_dashboard(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-extra", "{}");
    std::fs::write(settings.0.join("dashboard.json"), ALERTS_ON).unwrap();
    let now = chrono::Utc::now();
    let resets = (now + chrono::Duration::days(3)).to_rfc3339();
    let payload = format!(
        r#"{{"limits":[
            {{"kind":"session","percent":10,"resets_at":"{resets}"}},
            {{"kind":"weekly_all","percent":20,"resets_at":"{resets}"}},
            {{"kind":"weekly_scoped","percent":30,"resets_at":"{resets}","scope":{{"model":{{"display_name":"Fable"}}}}}}],
          "spend":{{"enabled":true,"used":{{"amount_minor":5760,"currency":"SGD","exponent":2}},
            "limit":{{"amount_minor":6000,"currency":"SGD","exponent":2}},
            "balance":{{"amount_minor":10000,"currency":"SGD","exponent":2}}}}}}"#
    );
    let account = codexbar_providers::claude::parse_usage(&payload, Some("max"), now).unwrap();
    let _dashboard = open_live_fixed(cx, &settings, vec![Arc::new(FixedProvider(vec![account]))]);
    cx.run_until_parked();
    // Every metric is listed with its value; windows also with their reset.
    let listed: Vec<String> = (0..5usize)
        .map(|ix| label_of(cx, ("account-metric", ix)).unwrap_or_default())
        .collect();
    assert!(listed[0].starts_with("5-hour window: 10%, resets in "), "{listed:?}");
    assert!(listed[1].starts_with("Weekly: 20%, resets in "), "{listed:?}");
    assert!(listed[2].starts_with("Weekly Fable: 30%, resets in "), "{listed:?}");
    assert_eq!(listed[3], "Extra usage: S$57.60 of S$60.00, S$2.40 left to the limit");
    assert_eq!(listed[4], "Credit balance: S$100.00 left");
    // The credits near their cap are what raised the status, and the alert says so in the account's currency.
    let strongest = label_of(cx, "alert-strongest").expect("an alert is shown");
    assert!(
        strongest.starts_with("Limit soon: Extra usage. S$57.60 of S$60.00"),
        "{strongest}"
    );
}

#[gpui_kit::test]
fn removing_an_account_deletes_its_history_and_preferences(cx: &mut TestAppContext) {
    use codexbar_store::summary::Point;
    let settings = TempSettings::new("remove-history", "{}");
    let dashboard = open_live_with(cx, &settings, vec![], MemoryCredentialStore::default());
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-1", cx)
    })
    .unwrap();
    cx.run_until_parked();
    add_openrouter(cx, handle, "Work", "sk-or-test");
    let id = saved_settings(&settings).accounts()[0].id.clone();

    // The account has history, a hidden-history choice and a group; another account (Codex, which keeps showing
    // through its own sign-in) has history too.
    let now = chrono::Utc::now();
    let history = cx.update(|cx| dashboard.read(cx).history());
    {
        let mut store = history.lock().unwrap();
        store.insert_points(&id, "credits", &[Point::new(now - chrono::Duration::hours(1), 12.0)]);
        store.insert_points(
            "codex-chatgpt",
            "weekly",
            &[Point::new(now - chrono::Duration::hours(1), 0.4)],
        );
    }
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set(cx, &id, false);
        crate::prefs_hub::PrefsHub::update_layout(cx, |layout| {
            let work = layout.create_group("Work")?;
            layout.assign(&id, Some(&work))
        })
        .unwrap();
    });

    click(cx, handle, format!("remove-{id}"));
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());

    let since = now - chrono::Duration::days(1);
    let store = history.lock().unwrap();
    assert!(
        store.points(&id, "credits", since, now).is_empty(),
        "its history is deleted"
    );
    assert_eq!(
        store.points("codex-chatgpt", "weekly", since, now).len(),
        1,
        "others are kept"
    );
    drop(store);
    let prefs = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert!(prefs.shows_history(&id));
    assert_eq!(prefs.layout().group_of(&id), None);
}

fn history_points(cx: &mut TestAppContext, dashboard: &Entity<Dashboard>, id: &str, metric: &str) -> usize {
    let history = cx.update(|cx| dashboard.read(cx).history());
    let now = chrono::Utc::now();
    let store = history.lock().unwrap();
    store
        .points(
            id,
            metric,
            now - chrono::Duration::days(1),
            now + chrono::Duration::minutes(1),
        )
        .len()
}

#[gpui_kit::test]
fn a_refresh_after_removal_does_not_bring_the_account_or_its_history_back(cx: &mut TestAppContext) {
    use codexbar_store::settings::{AccountRecord, AuthMethod, names};
    let settings = TempSettings::new("remove-stale", "{}");
    let record = AccountRecord::new(names::OPENROUTER, "Work", AuthMethod::ApiKey);
    let id: &'static str = Box::leak(record.id.clone().into_boxed_str());
    // The provider keeps answering for the account, like a fetch that started before it was removed.
    let provider = FakeProvider::for_account(Provider::OpenRouter, id, Some(0.5));
    let dashboard = open_live(cx, &settings, vec![provider]);
    cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(record.clone())))
        .unwrap();
    refresh(cx, &dashboard);
    assert!(shown_ids(cx, &dashboard).contains(&id.to_owned()));
    assert_eq!(history_points(cx, &dashboard, id, "weekly"), 1);

    cx.update(|cx| {
        SettingsHub::update(cx, |settings| {
            settings.remove(id);
            Ok(())
        })
    })
    .unwrap();
    cx.run_until_parked();
    refresh(cx, &dashboard);
    assert!(!shown_ids(cx, &dashboard).contains(&id.to_owned()), "not shown again");
    assert_eq!(
        history_points(cx, &dashboard, id, "weekly"),
        0,
        "and its history stays deleted"
    );
}

#[gpui_kit::test]
fn copilot_history_is_kept_while_discovery_still_shows_the_account(cx: &mut TestAppContext) {
    use codexbar_store::settings::{AccountRecord, AuthMethod, names};
    let settings = TempSettings::new("remove-copilot", "{}");
    let dashboard = open_live(cx, &settings, vec![]);
    cx.run_until_parked();
    let mut alice = AccountRecord::new(names::COPILOT, "Alice", AuthMethod::CommandLine);
    alice.external_id = Some("alice".into());
    let mut bob = AccountRecord::new(names::COPILOT, "Bob", AuthMethod::CommandLine);
    bob.external_id = Some("bob".into());
    cx.update(|cx| {
        SettingsHub::update(cx, |settings| {
            settings.upsert(alice.clone())?;
            settings.upsert(bob.clone())
        })
    })
    .unwrap();
    cx.run_until_parked();
    let history = cx.update(|cx| dashboard.read(cx).history());
    let now = chrono::Utc::now();
    for user in ["copilot-alice", "copilot-bob"] {
        history.lock().unwrap().insert_points(
            user,
            "premium-requests",
            &[codexbar_store::summary::Point::new(
                now - chrono::Duration::hours(1),
                0.3,
            )],
        );
    }
    // Clearing Alice's username makes Copilot show every signed-in account again: nothing is deleted.
    alice.external_id = None;
    cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(alice.clone())))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(history_points(cx, &dashboard, "copilot-bob", "premium-requests"), 1);
    // Back to usernames only, then Bob is removed: only his history goes.
    alice.external_id = Some("alice".into());
    cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(alice.clone())))
        .unwrap();
    cx.update(|cx| {
        SettingsHub::update(cx, |settings| {
            settings.remove(&bob.id);
            Ok(())
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(history_points(cx, &dashboard, "copilot-bob", "premium-requests"), 0);
    assert_eq!(history_points(cx, &dashboard, "copilot-alice", "premium-requests"), 1);
}

const PLAINTEXT_KEY: &str = r#"{
    "accountConfigurationVersion": 1,
    "accounts": [],
    "providers": { "OpenRouter": { "enabled": true, "apiKey": "sk-or-plaintext" } }
}"#;

#[gpui_kit::test]
fn plaintext_keys_move_to_credential_manager_at_start(cx: &mut TestAppContext) {
    let settings = TempSettings::new("keys-move", PLAINTEXT_KEY);
    let _dashboard = open_live_with(cx, &settings, vec![], MemoryCredentialStore::default());
    cx.run_until_parked();
    let text = std::fs::read_to_string(settings.0.join("settings.json")).unwrap();
    assert!(!text.contains("sk-or-plaintext"), "the file no longer holds the key");
    let implicit = codexbar_store::settings::legacy_id("OpenRouter", "");
    assert_eq!(secret_of(cx, &implicit).as_deref(), Some("sk-or-plaintext"));
    assert_eq!(cx.update(|cx| SettingsHub::global(cx).notice()), None);
}

#[gpui_kit::test]
fn a_key_that_cannot_move_stays_usable_and_says_so(cx: &mut TestAppContext) {
    let settings = TempSettings::new("keys-stay", PLAINTEXT_KEY);
    let _dashboard = open_live_with(cx, &settings, vec![], MemoryCredentialStore::failing());
    cx.run_until_parked();
    let text = std::fs::read_to_string(settings.0.join("settings.json")).unwrap();
    assert!(text.contains("sk-or-plaintext"), "kept until it can move");
    let notice = cx.update(|cx| SettingsHub::global(cx).notice()).expect("a notice");
    assert_eq!(
        notice.as_ref(),
        "Keys for OpenRouter are still in the settings file: Windows Credential Manager failed (error 5). \
         CodexBar tries again at the next start."
    );
    assert!(!notice.contains("sk-or"), "never the key itself");
}

#[gpui_kit::test]
fn a_moved_key_still_works_after_the_first_account_is_added(cx: &mut TestAppContext) {
    use codexbar_store::settings::{AccountRecord, AuthMethod, names};
    let settings = TempSettings::new("keys-first-account", PLAINTEXT_KEY);
    let _dashboard = open_live_with(cx, &settings, vec![], MemoryCredentialStore::default());
    cx.run_until_parked();
    // The key moved under the implicit account; now the first explicit account is added without a key.
    let record = AccountRecord::new(names::OPENROUTER, "Work", AuthMethod::ApiKey);
    cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(record.clone())))
        .unwrap();
    // An OPENROUTER_API_KEY in the environment would win over every stored key; this case needs it unset.
    if std::env::var_os("OPENROUTER_API_KEY").is_some() {
        return;
    }
    let (secret, source) = cx.update(|cx| SettingsHub::global(cx).secret_for(&record));
    // Compared without printing: a failure must never put a secret in the test output.
    assert!(secret.as_deref() == Some("sk-or-plaintext"), "the moved key is found");
    assert!(matches!(
        source,
        codexbar_store::credentials::SecretSource::CredentialManager
    ));
}

#[gpui_kit::test]
fn removing_the_first_account_also_deletes_the_moved_key(cx: &mut TestAppContext) {
    use codexbar_store::settings::{AccountRecord, AuthMethod, names};
    let settings = TempSettings::new("keys-remove-first", PLAINTEXT_KEY);
    let dashboard = open_live_with(cx, &settings, vec![], MemoryCredentialStore::default());
    cx.run_until_parked();
    let implicit = codexbar_store::settings::legacy_id(names::OPENROUTER, "");
    assert!(secret_of(cx, &implicit).is_some(), "moved under the implicit account");
    let record = AccountRecord::new(names::OPENROUTER, "Work", AuthMethod::ApiKey);
    cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(record.clone())))
        .unwrap();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-1", cx)
    })
    .unwrap();
    cx.run_until_parked();
    click(cx, handle, format!("remove-{}", record.id));
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    assert!(
        secret_of(cx, &implicit).is_none(),
        "the provider doesn't go on signing in with it"
    );
}

#[gpui_kit::test]
fn the_appearance_choice_applies_at_once_and_is_saved(cx: &mut TestAppContext) {
    use crate::theme::{Appearance, applied, apply_with};
    use gpui_kit::component::ActiveTheme as _;
    let settings = TempSettings::new("appearance", "{}");
    let dashboard = open_live(cx, &settings, vec![]);
    cx.run_until_parked();
    let handle = cx.windows()[0];
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    // Appearance is the fifth page: General, Accounts, Alerts, Groups, Appearance.
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-4", cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window
            .within("group-0")
            .within("item-0")
            .within("field")
            .click("btn", cx)
    })
    .unwrap();
    cx.run_until_parked();
    // System, Light, Dark: the second is Light.
    press(cx, handle, "down");
    press(cx, handle, "down");
    press(cx, handle, "enter");
    assert_eq!(cx.update(|cx| applied(cx)).unwrap().theme, "CodexBar Light");
    assert!(!cx.update(|cx| cx.theme().is_dark()));
    let saved = codexbar_store::prefs::DashboardPrefs::load(&settings.0);
    assert_eq!(saved.appearance(), Some("light"));

    // System follows Windows' app mode.
    cx.update(|cx| apply_with(cx, Appearance::System, true, None));
    assert_eq!(cx.update(|cx| applied(cx)).unwrap().theme, "CodexBar Dark");
    cx.update(|cx| apply_with(cx, Appearance::System, false, None));
    assert_eq!(cx.update(|cx| applied(cx)).unwrap().theme, "CodexBar Light");
}

#[gpui_kit::test]
fn windows_high_contrast_overrides_the_choice_and_hands_back(cx: &mut TestAppContext) {
    use crate::theme::{Appearance, SystemColors, applied, apply_with};
    use gpui_kit::component::ActiveTheme as _;
    let settings = TempSettings::new("appearance-hc", "{}");
    let _dashboard = open_live(cx, &settings, vec![]);
    cx.run_until_parked();
    let scheme = SystemColors {
        window: "#000000".into(),
        text: "#FFFFFF".into(),
        highlight: "#1AEBFF".into(),
        highlight_text: "#000000".into(),
        disabled_text: "#3FF23F".into(),
        hotlight: "#FFFF00".into(),
    };
    cx.update(|cx| apply_with(cx, Appearance::Light, false, Some(scheme.clone())));
    let shown = cx.update(|cx| applied(cx)).unwrap();
    assert!(shown.high_contrast);
    assert_eq!(shown.theme, "CodexBar High Contrast");
    let (background, primary) = cx.update(|cx| (cx.theme().background, cx.theme().primary));
    assert_eq!(background, gpui_kit::rgb(0x000000).into());
    assert_eq!(primary, gpui_kit::rgb(0x1AEBFF).into());
    // High contrast off again: back to the user's choice.
    cx.update(|cx| apply_with(cx, Appearance::Light, false, None));
    assert_eq!(cx.update(|cx| applied(cx)).unwrap().theme, "CodexBar Light");
}

#[gpui_kit::test]
fn changing_themes_keeps_the_zoom_and_follows_a_new_contrast_scheme(cx: &mut TestAppContext) {
    use crate::theme::{Appearance, SystemColors, apply_with};
    use gpui_kit::component::ActiveTheme as _;
    let settings = TempSettings::new("appearance-zoom", r#"{ "zoomLevel": 1.5 }"#);
    let _dashboard = open_live(cx, &settings, vec![]);
    cx.run_until_parked();
    let zoomed = cx.update(|cx| cx.theme().font_size);
    assert_eq!(zoomed, gpui_kit::px(24.0), "150% of 16");
    cx.update(|cx| apply_with(cx, Appearance::Light, false, None));
    assert_eq!(
        cx.update(|cx| cx.theme().font_size),
        zoomed,
        "a theme change keeps the zoom"
    );

    let black = SystemColors {
        window: "#000000".into(),
        text: "#FFFFFF".into(),
        highlight: "#1AEBFF".into(),
        highlight_text: "#000000".into(),
        disabled_text: "#3FF23F".into(),
        hotlight: "#FFFF00".into(),
    };
    let white = SystemColors {
        window: "#FFFFFF".into(),
        text: "#000000".into(),
        highlight: "#37006E".into(),
        highlight_text: "#FFFFFF".into(),
        disabled_text: "#600000".into(),
        hotlight: "#00009F".into(),
    };
    cx.update(|cx| apply_with(cx, Appearance::Light, false, Some(black)));
    assert_eq!(cx.update(|cx| cx.theme().background), gpui_kit::rgb(0x000000).into());
    // The user switches high-contrast schemes while CodexBar runs.
    cx.update(|cx| apply_with(cx, Appearance::Light, false, Some(white)));
    assert_eq!(cx.update(|cx| cx.theme().background), gpui_kit::rgb(0xFFFFFF).into());
    assert_eq!(cx.update(|cx| cx.theme().primary), gpui_kit::rgb(0x37006E).into());
    assert_eq!(cx.update(|cx| cx.theme().font_size), zoomed);
}

#[gpui_kit::test]
fn accounts_carry_a_provider_badge_named_for_screen_readers(cx: &mut TestAppContext) {
    let settings = TempSettings::new("brand-badges", "{}");
    let _dashboard = open_live(
        cx,
        &settings,
        vec![FakeProvider::new(Provider::Claude, "claude-1", Some(0.3))],
    );
    cx.run_until_parked();
    // In the table row and the focused account's heading.
    assert_eq!(label_of(cx, "provider-badge-claude-1").as_deref(), Some("Claude"));
    assert_eq!(label_of(cx, "provider-badge-focused").as_deref(), Some("Claude"));
}

#[gpui_kit::test]
fn the_keyboard_starts_on_the_view_tabs_and_tab_cycles_through_the_usage_view(cx: &mut TestAppContext) {
    let settings = TempSettings::new("tab-order", "{}");
    let _dashboard = open_live(
        cx,
        &settings,
        vec![FakeProvider::new(Provider::Claude, "claude-1", Some(0.3))],
    );
    cx.run_until_parked();
    let handle = cx.windows()[0];
    let focused = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).and_then(|found| found.focused()) == Some(true)
        })
        .unwrap()
    };
    // Focus starts on the selected view tab, so the first key press does something.
    assert!(focused(cx, ("view-tab", 0usize).into()));
    let order: [gpui_kit::ElementId; 4] = [
        "refresh".into(),
        ("order-mode", 0usize).into(),
        ("order-mode", 1usize).into(),
        "group-menu".into(),
    ];
    for (step, id) in order.iter().enumerate() {
        press(cx, handle, "tab");
        // After the Order toggle, Tab stops on the table (its arrow keys pick rows), and the next Tab leaves it.
        if step == 3 {
            assert!(!focused(cx, id.clone()), "the table takes one stop");
            press(cx, handle, "tab");
        }
        assert!(focused(cx, id.clone()), "step {step}: {id:?}");
    }
    // And back around: the table no longer traps Tab.
    press(cx, handle, "tab");
    assert!(focused(cx, ("view-tab", 0usize).into()));
    press(cx, handle, "shift-tab");
    assert!(focused(cx, "group-menu".into()), "Shift+Tab goes back the same way");
}

#[gpui_kit::test]
fn sign_in_tokens_are_kept_in_and_replaced_through_credential_manager(cx: &mut TestAppContext) {
    use codexbar_providers::oauth::TokenSet;
    let settings = TempSettings::new("oauth-tokens", "{}");
    let _dashboard = open_live(cx, &settings, vec![]);
    cx.run_until_parked();
    let first = TokenSet {
        access_token: "at-1".into(),
        refresh_token: Some("rt-1".into()),
        expires_at: None,
    };
    let renewed = TokenSet {
        access_token: "at-2".into(),
        refresh_token: Some("rt-1".into()),
        expires_at: None,
    };
    cx.update(|cx| SettingsHub::global(cx).save_tokens("acct", &first))
        .unwrap();
    cx.update(|cx| SettingsHub::global(cx).save_tokens("acct", &renewed))
        .unwrap();
    let stored = cx.update(|cx| SettingsHub::global(cx).tokens_for("acct")).unwrap();
    assert!(stored == Some(renewed), "the renewal replaced the first tokens");
    // Long tokens (JWTs) are split across credentials and read back whole.
    let long = TokenSet {
        access_token: "j".repeat(3000),
        refresh_token: Some("r".repeat(2000)),
        expires_at: None,
    };
    cx.update(|cx| SettingsHub::global(cx).save_tokens("acct", &long))
        .unwrap();
    let stored = cx.update(|cx| SettingsHub::global(cx).tokens_for("acct")).unwrap();
    assert!(stored == Some(long), "long tokens round-trip");
    // Removing the account's secret removes every part.
    cx.update(|cx| codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), "acct"))
        .unwrap();
    assert!(
        cx.update(|cx| SettingsHub::global(cx).tokens_for("acct"))
            .unwrap()
            .is_none()
    );
    assert!(secret_of(cx, "acct#part1.0").is_none() && secret_of(cx, "acct#part2.0").is_none());
    // Nothing goes to the settings file.
    let text = std::fs::read_to_string(settings.0.join("settings.json")).unwrap_or_default();
    assert!(!text.contains("at-2") && !text.contains("rt-1"));
}

/// A Cursor-like provider that knows which account it's signed in to, and whose fetch hasn't returned yet.
struct SignedInCursor(AccountId);

impl UsageProvider for SignedInCursor {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn fetch(&self, _: chrono::DateTime<chrono::Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        Err(ProviderError::Network)
    }

    fn signed_in_account(&self) -> Option<AccountId> {
        Some(self.0.clone())
    }
}

#[gpui_kit::test]
fn a_saved_cursor_account_is_not_restored_after_a_switch(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-switch-restore", "{}");
    let saved = |id: &str, provider| AccountSnapshot::new(AccountId::new(id), provider, Vec::new(), chrono::Utc::now());
    codexbar_store::snapshots::save_snapshots(
        &settings.0,
        &[
            saved("cursor-aaaaaaaaaaaa", Provider::Cursor),
            saved("claude-1", Provider::Claude),
        ],
    )
    .unwrap();
    // The Cursor app is now signed in to another account.
    let dashboard = open_live_dyn(
        cx,
        &settings,
        vec![
            Arc::new(SignedInCursor(AccountId::new("cursor-bbbbbbbbbbbb"))),
            FakeProvider::new(Provider::Claude, "claude-1", Some(0.4)),
        ],
        MemoryCredentialStore::default(),
    );
    let shown = ids(cx, &dashboard);
    assert!(
        shown.contains(&"claude-1".to_owned()),
        "other saved accounts are restored: {shown:?}"
    );
    assert!(
        !shown.contains(&"cursor-aaaaaaaaaaaa".to_owned()),
        "the previous Cursor account isn't shown: {shown:?}"
    );
}

// Codex accounts CodexBar signs in (#78): browser sign-in, failure, cancellation, sign-out and removal, with a fake
// standing in for the Codex CLI, so no test starts Codex or a browser.

mod codex_fixture {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codexbar_providers::codex_app_server::AppServerError;

    use crate::codex_sign_in::{PendingSignIn, SignInService};

    /// How a fake sign-in ends.
    #[derive(Clone, Default)]
    pub enum Outcome {
        /// Writes a sign-in for this user and email to the home.
        SignsIn(&'static str, &'static str),
        /// Fails with Codex's message.
        Fails(&'static str),
        /// Still waiting for the browser: the dialog stays open until it is closed.
        #[default]
        Waits,
    }

    #[derive(Default)]
    pub struct State {
        pub outcome: Mutex<Outcome>,
        pub begun: Mutex<Vec<PathBuf>>,
        pub signed_out: Mutex<Vec<PathBuf>>,
        pub cancel: Mutex<Option<Arc<AtomicBool>>>,
    }

    #[derive(Default, Clone)]
    pub struct FakeCodexSignIn(pub Arc<State>);

    impl FakeCodexSignIn {
        pub fn new(outcome: Outcome) -> Self {
            let fake = Self::default();
            *fake.0.outcome.lock().unwrap() = outcome;
            fake
        }
    }

    impl SignInService for FakeCodexSignIn {
        fn begin(&self, home: &Path, _: &AtomicBool) -> Result<Box<dyn PendingSignIn>, AppServerError> {
            self.0.begun.lock().unwrap().push(home.to_owned());
            Ok(Box::new(Pending {
                state: self.0.clone(),
                home: home.to_owned(),
            }))
        }

        fn sign_out(&self, home: &Path) -> Result<(), AppServerError> {
            self.0.signed_out.lock().unwrap().push(home.to_owned());
            let _ = std::fs::remove_file(home.join("auth.json"));
            Ok(())
        }
    }

    struct Pending {
        state: Arc<State>,
        home: PathBuf,
    }

    impl PendingSignIn for Pending {
        fn url(&self) -> String {
            "https://auth.example/start".to_owned()
        }

        fn finish(self: Box<Self>, _: Duration, cancel: Arc<AtomicBool>) -> Result<(), AppServerError> {
            *self.state.cancel.lock().unwrap() = Some(cancel.clone());
            match self.state.outcome.lock().unwrap().clone() {
                Outcome::SignsIn(user, email) => {
                    std::fs::write(self.home.join("auth.json"), sign_in(user, email)).unwrap();
                    Ok(())
                }
                Outcome::Fails(message) => Err(AppServerError::Refused(message.to_owned())),
                // The real sign-in ends this way once the dialog sets `cancel`; until then the dialog keeps waiting.
                Outcome::Waits => {
                    assert!(!cancel.load(Ordering::SeqCst));
                    Err(AppServerError::Cancelled)
                }
            }
        }
    }

    /// A Codex `auth.json` for this user and email, with an unsigned ID token.
    pub fn sign_in(user: &str, email: &str) -> String {
        let claims = serde_json::json!({"sub": user, "email": email}).to_string();
        let id_token = format!("e30.{}.sig", base64url(claims.as_bytes()));
        serde_json::json!({"tokens": {"access_token": "access", "account_id": "workspace-1", "id_token": id_token}})
            .to_string()
    }

    fn base64url(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (ix, byte)| acc | u32::from(*byte) << (16 - 8 * ix));
            for ix in 0..=chunk.len() {
                out.push(DIGITS[(n >> (18 - 6 * ix) & 63) as usize] as char);
            }
        }
        out
    }
}

use codex_fixture::{FakeCodexSignIn, Outcome};

const MANAGED_CODEX: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "cx-1", "providerId": "Codex", "displayLabel": "Work", "enabled": true,
      "authenticationMethod": "OAuth" } ] }"#;

/// The Accounts page with `fake` standing in for the Codex CLI.
fn open_codex_accounts(cx: &mut TestAppContext, settings: &TempSettings, fake: &FakeCodexSignIn) -> AnyWindowHandle {
    let fake = fake.clone();
    cx.update(|cx| crate::codex_sign_in::init_with(cx, Arc::new(fake)));
    open_settings_page(cx, settings, MemoryCredentialStore::default(), 1)
}

fn codex_home(settings: &TempSettings, id: &str) -> PathBuf {
    settings.0.join("codex").join(id)
}

#[gpui_kit::test]
fn a_codex_account_signs_in_through_the_browser_and_shows_who_it_is(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-sign-in", MANAGED_CODEX);
    let fake = FakeCodexSignIn::new(Outcome::SignsIn("user-1", "dev@example.com"));
    let handle = open_codex_accounts(cx, &settings, &fake);
    assert_eq!(
        label_of(cx, "account-detail-cx-1").as_deref(),
        Some("OAuth · Not signed in")
    );
    assert!(!exists(cx, handle, "sign-out-cx-1"));
    let requests = cx.update(|cx| SettingsHub::refresh_requests(cx));

    click(cx, handle, "sign-in-cx-1");
    let home = codex_home(&settings, "cx-1");
    assert_eq!(*fake.0.begun.lock().unwrap(), std::slice::from_ref(&home));
    // The home keeps its sign-in in a file the usage fetch reads.
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("cli_auth_credentials_store = \"file\""));
    // Signed in: the dialog closes, the account holds that identity, and the dashboard fetches again.
    assert!(!exists(cx, handle, "sign-in-status"));
    let identity = codexbar_providers::codex::signed_in_account(&home.join("auth.json")).unwrap();
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(
        label_of(cx, "account-detail-cx-1").as_deref(),
        Some("OAuth · dev@example.com")
    );
    let after_sign_in = cx.update(|cx| SettingsHub::refresh_requests(cx));
    assert!(after_sign_in > requests);
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("cx-1"), Some(&identity.as_str().to_owned()));

    // Signing out keeps the account and its identity, so its history waits for the next sign-in.
    click(cx, handle, "sign-out-cx-1");
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&home));
    assert_eq!(
        label_of(cx, "account-detail-cx-1").as_deref(),
        Some("OAuth · Not signed in")
    );
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(identity.as_str())
    );
    assert!(cx.update(|cx| SettingsHub::refresh_requests(cx)) > after_sign_in);
}

#[gpui_kit::test]
fn signing_an_account_in_to_another_identity_replaces_the_one_it_held(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-switch", MANAGED_CODEX);
    let fake = FakeCodexSignIn::new(Outcome::SignsIn("user-1", "one@example.com"));
    let handle = open_codex_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cx-1");
    let first = saved_settings(&settings).accounts()[0].external_id.clone().unwrap();
    *fake.0.outcome.lock().unwrap() = Outcome::SignsIn("user-2", "two@example.com");
    click(cx, handle, "sign-in-cx-1");
    let second = saved_settings(&settings).accounts()[0].external_id.clone().unwrap();
    assert_ne!(first, second);
    assert_eq!(
        label_of(cx, "account-detail-cx-1").as_deref(),
        Some("OAuth · two@example.com")
    );
}

#[gpui_kit::test]
fn a_failed_codex_sign_in_says_why_and_changes_nothing(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-sign-in-fails", MANAGED_CODEX);
    let fake = FakeCodexSignIn::new(Outcome::Fails("Login was cancelled in the browser"));
    let handle = open_codex_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cx-1");
    assert_eq!(
        label_of(cx, "sign-in-error").as_deref(),
        Some("Codex: Login was cancelled in the browser")
    );
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
    press(cx, handle, "escape");
    assert!(!exists(cx, handle, "sign-in-error"));
}

#[gpui_kit::test]
fn closing_the_sign_in_dialog_cancels_the_sign_in(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-sign-in-cancel", MANAGED_CODEX);
    let fake = FakeCodexSignIn::new(Outcome::Waits);
    let handle = open_codex_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cx-1");
    let status = label_of(cx, "sign-in-status").unwrap();
    assert!(
        status.starts_with("Finish signing in to ChatGPT in your browser"),
        "{status}"
    );
    assert!(exists(cx, handle, "sign-in-copy"));
    let cancel = fake.0.cancel.lock().unwrap().clone().unwrap();
    assert!(!cancel.load(std::sync::atomic::Ordering::SeqCst));
    press(cx, handle, "escape");
    assert!(!exists(cx, handle, "sign-in-status"));
    assert!(
        cancel.load(std::sync::atomic::Ordering::SeqCst),
        "closing the dialog cancels the sign-in"
    );
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
}

#[gpui_kit::test]
fn removing_a_codex_account_signs_it_out_and_deletes_its_folder(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-remove", MANAGED_CODEX);
    let home = codex_home(&settings, "cx-1");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        codex_fixture::sign_in("user-1", "dev@example.com"),
    )
    .unwrap();
    let fake = FakeCodexSignIn::default();
    let handle = open_codex_accounts(cx, &settings, &fake);
    assert!(exists(cx, handle, "sign-out-cx-1"));
    click(cx, handle, "remove-cx-1");
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&home));
    assert!(!home.exists());
}

#[gpui_kit::test]
fn each_codex_account_gets_its_own_adapter(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "codex-adapters",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cx-1", "providerId": "Codex", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "codex-aaaaaaaaaaaa" },
            { "id": "cx-2", "providerId": "Codex", "displayLabel": "Personal", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "cx-3", "providerId": "Codex", "displayLabel": "Off", "enabled": false,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let codex: Vec<(Option<String>, Option<String>)> = adapters
        .iter()
        .filter(|provider| provider.name() == "ChatGPT · Codex")
        .map(|provider| {
            (
                provider.account_id().map(str::to_owned),
                provider.account_label().map(str::to_owned),
            )
        })
        .collect();
    // The identity a signed-in account holds; the record itself before its first sign-in. Disabled accounts don't run.
    assert_eq!(
        codex,
        [
            (Some("codex-aaaaaaaaaaaa".to_owned()), Some("Work".to_owned())),
            (Some("cx-2".to_owned()), Some("Personal".to_owned())),
        ]
    );
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("cx-1").map(String::as_str), Some("codex-aaaaaaaaaaaa"));
    assert!(!owned.contains_key("cx-2"));
}

#[gpui_kit::test]
fn a_refresh_request_from_settings_fetches_again(cx: &mut TestAppContext) {
    let settings = TempSettings::new("refresh-request", "{}");
    let codex = FakeProvider::new(Provider::Codex, "codex-1", Some(0.3));
    let _dashboard = open_live(cx, &settings, vec![codex.clone()]);
    cx.run_until_parked();
    assert_eq!(codex.calls(), 1);
    cx.update(SettingsHub::request_refresh);
    cx.run_until_parked();
    assert_eq!(
        codex.calls(),
        2,
        "a sign-in is fetched at once, not at the next scheduled refresh"
    );
}

#[gpui_kit::test]
fn adding_a_codex_account_with_oauth_signs_it_in_right_away(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-add", "{}");
    let fake = FakeCodexSignIn::new(Outcome::SignsIn("user-1", "dev@example.com"));
    let handle = open_codex_accounts(cx, &settings, &fake);
    click(cx, handle, "add-Codex");
    type_text(cx, handle, "Work");
    // Tab to the sign-in method and choose OAuth: from Automatic, four Downs (the first opens the list) reach it.
    press(cx, handle, "tab");
    for _ in 0..4 {
        press(cx, handle, "down");
    }
    press(cx, handle, "enter");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    let record = saved
        .accounts()
        .iter()
        .find(|record| record.method == codexbar_store::settings::AuthMethod::OAuth)
        .unwrap();
    assert_eq!(record.label, "Work");
    assert_eq!(*fake.0.begun.lock().unwrap(), [codex_home(&settings, &record.id)]);
    assert!(record.external_id.is_some(), "the finished sign-in is remembered");
    // The Codex CLI's own sign-in, shown until now as the implicit account, stays as an account of its own.
    let methods: Vec<_> = saved.accounts().iter().map(|record| record.method).collect();
    assert_eq!(
        methods,
        [
            codexbar_store::settings::AuthMethod::Automatic,
            codexbar_store::settings::AuthMethod::OAuth
        ]
    );
}

#[gpui_kit::test]
fn switching_a_codex_account_to_automatic_signs_out_its_own_home(cx: &mut TestAppContext) {
    let settings = TempSettings::new("codex-to-automatic", MANAGED_CODEX);
    let home = codex_home(&settings, "cx-1");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        codex_fixture::sign_in("user-1", "dev@example.com"),
    )
    .unwrap();
    let fake = FakeCodexSignIn::default();
    let handle = open_codex_accounts(cx, &settings, &fake);
    click(cx, handle, "edit-cx-1");
    // From OAuth back to Automatic.
    press(cx, handle, "tab");
    for _ in 0..4 {
        press(cx, handle, "up");
    }
    press(cx, handle, "enter");
    click(cx, handle, "account-save");
    assert_eq!(
        saved_settings(&settings).accounts()[0].method,
        codexbar_store::settings::AuthMethod::Automatic
    );
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&home));
    assert!(!home.exists(), "no sign-in is left behind in the old home");
}

#[gpui_kit::test]
fn homes_signed_in_to_one_identity_show_it_once(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "codex-same-identity",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cx-1", "providerId": "Codex", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "codex-aaaaaaaaaaaa" },
            { "id": "cx-2", "providerId": "Codex", "displayLabel": "Again", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "codex-aaaaaaaaaaaa" } ] }"#,
    );
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let codex: Vec<Option<String>> = adapters
        .iter()
        .filter(|provider| provider.name() == "ChatGPT · Codex")
        .map(|provider| provider.account_id().map(str::to_owned))
        .collect();
    assert_eq!(codex, [Some("codex-aaaaaaaaaaaa".to_owned())]);
}

/// Answers every request with 503, so a test's fetch fails without touching the network.
struct Unavailable;

impl codexbar_providers::HttpClient for Unavailable {
    fn get(
        &self,
        _: &str,
        _: &[(&str, &str)],
    ) -> Result<codexbar_providers::HttpResponse, codexbar_providers::ProviderError> {
        Ok(codexbar_providers::HttpResponse::new(503, ""))
    }
}

#[gpui_kit::test]
fn saved_codex_usage_restores_per_account(cx: &mut TestAppContext) {
    use codexbar_providers::codex::CodexProvider;
    let settings = TempSettings::new(
        "codex-restore",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cx-3", "providerId": "Codex", "displayLabel": "Off", "enabled": false,
              "authenticationMethod": "OAuth", "externalAccountId": "codex-cccccccccccc" } ] }"#,
    );
    let signed_in = settings.0.join("codex").join("cx-2");
    std::fs::create_dir_all(&signed_in).unwrap();
    std::fs::write(
        signed_in.join("auth.json"),
        codex_fixture::sign_in("user-2", "two@example.com"),
    )
    .unwrap();
    let identity = codexbar_providers::codex::signed_in_account(&signed_in.join("auth.json")).unwrap();
    let now = chrono::Utc::now();
    let snapshot = |id: &str| AccountSnapshot::new(AccountId::new(id), Provider::Codex, Vec::new(), now);
    codexbar_store::snapshots::save_snapshots(
        &settings.0,
        &[
            snapshot("codex-aaaaaaaaaaaa"),
            snapshot(identity.as_str()),
            snapshot("codex-cccccccccccc"),
        ],
    )
    .unwrap();
    // A signed out (its home is empty) but still enabled, B signed in, C switched off.
    let signed_out = CodexProvider::new(Unavailable, settings.0.join("codex").join("cx-1").join("auth.json"))
        .managed()
        .with_account("codex-aaaaaaaaaaaa", "Work");
    let current = CodexProvider::new(Unavailable, signed_in.join("auth.json"))
        .managed()
        .with_account(identity.as_str(), "Personal");
    let dashboard = open_live_fixed(cx, &settings, vec![Arc::new(signed_out), Arc::new(current)]);
    let mut shown = ids(cx, &dashboard);
    shown.sort();
    let mut expected = vec!["codex-aaaaaaaaaaaa".to_owned(), identity.as_str().to_owned()];
    expected.sort();
    assert_eq!(
        shown, expected,
        "a signed-out account keeps its usage; a switched-off one stays hidden"
    );
}

// Copilot accounts CodexBar signs in (#79): the GitHub CLI's device sign-in, its token kept in Credential Manager,
// sign-out, duplicates, cancellation and org billing settings, with a fake standing in for the GitHub CLI.

mod github_fixture {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codexbar_providers::gh_login::{DeviceCode, GhAccount, GhLoginError};

    use crate::github_sign_in::{PendingSignIn, SignInService};

    #[derive(Clone, Default)]
    pub enum Outcome {
        SignsIn(&'static str),
        /// GitHub approves, and the user closes the dialog in the same moment.
        SignsInAsCancelled(&'static str),
        Fails,
        #[default]
        Waits,
    }

    #[derive(Default)]
    pub struct State {
        pub outcome: Mutex<Outcome>,
        pub parents: Mutex<Vec<PathBuf>>,
        pub scopes: Mutex<Vec<String>>,
        pub cancel: Mutex<Option<Arc<AtomicBool>>>,
    }

    #[derive(Default, Clone)]
    pub struct FakeGitHub(pub Arc<State>);

    impl FakeGitHub {
        pub fn new(outcome: Outcome) -> Self {
            let fake = Self::default();
            *fake.0.outcome.lock().unwrap() = outcome;
            fake
        }
    }

    impl SignInService for FakeGitHub {
        fn begin(
            &self,
            parent: &Path,
            scopes: &[&str],
            _: &AtomicBool,
        ) -> Result<Box<dyn PendingSignIn>, GhLoginError> {
            self.0.parents.lock().unwrap().push(parent.to_owned());
            *self.0.scopes.lock().unwrap() = scopes.iter().map(|scope| (*scope).to_owned()).collect();
            Ok(Box::new(Pending(self.0.clone())))
        }
    }

    struct Pending(Arc<State>);

    impl PendingSignIn for Pending {
        fn device(&self) -> DeviceCode {
            DeviceCode {
                code: "ABCD-1234".to_owned(),
                url: "https://github.com/login/device".to_owned(),
            }
        }

        fn finish(self: Box<Self>, _: Duration, cancel: Arc<AtomicBool>) -> Result<GhAccount, GhLoginError> {
            *self.0.cancel.lock().unwrap() = Some(cancel.clone());
            match self.0.outcome.lock().unwrap().clone() {
                Outcome::SignsIn(user) => Ok(GhAccount {
                    username: user.to_owned(),
                    token: format!("gho_token_for_{user}"),
                }),
                Outcome::SignsInAsCancelled(user) => {
                    cancel.store(true, Ordering::SeqCst);
                    Ok(GhAccount {
                        username: user.to_owned(),
                        token: format!("gho_token_for_{user}"),
                    })
                }
                Outcome::Fails => Err(GhLoginError::Failed),
                Outcome::Waits => {
                    assert!(!cancel.load(Ordering::SeqCst));
                    Err(GhLoginError::Cancelled)
                }
            }
        }
    }
}

use github_fixture::FakeGitHub;

const MANAGED_COPILOT: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "gh-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
      "authenticationMethod": "OAuth" } ] }"#;

fn open_copilot_accounts(cx: &mut TestAppContext, settings: &TempSettings, fake: &FakeGitHub) -> AnyWindowHandle {
    let fake = fake.clone();
    cx.update(|cx| crate::github_sign_in::init_with(cx, Arc::new(fake)));
    open_settings_page(cx, settings, MemoryCredentialStore::default(), 1)
}

#[gpui_kit::test]
fn a_copilot_account_signs_in_with_a_github_code_and_keeps_its_own_token(cx: &mut TestAppContext) {
    let settings = TempSettings::new("github-sign-in", MANAGED_COPILOT);
    let fake = FakeGitHub::new(github_fixture::Outcome::SignsIn("octocat"));
    let handle = open_copilot_accounts(cx, &settings, &fake);
    assert_eq!(
        label_of(cx, "account-detail-gh-1").as_deref(),
        Some("OAuth · Not signed in")
    );
    click(cx, handle, "sign-in-gh-1");
    // The private GitHub CLI folder goes in CodexBar's settings folder, never the user's own gh config.
    assert_eq!(*fake.0.parents.lock().unwrap(), std::slice::from_ref(&settings.0));
    assert!(
        !exists(cx, handle, "github-sign-in-status"),
        "the dialog closes once signed in"
    );
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some("octocat")
    );
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1"));
    assert!(
        stored.ok().flatten().as_deref() == Some("gho_token_for_octocat"),
        "the token is kept for the account"
    );
    assert_eq!(label_of(cx, "account-detail-gh-1").as_deref(), Some("OAuth · octocat"));

    // The account is fetched with that token, and the GitHub CLI isn't asked for anyone else.
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    assert_eq!(adapters.iter().filter(|a| a.name() == "Copilot").count(), 1);
    assert!(!cx.update(|cx| crate::providers::copilot_discovers_all(SettingsHub::global(cx))));
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("gh-1").map(String::as_str), Some("copilot-octocat"));

    // Signing out deletes CodexBar's token and keeps the account.
    click(cx, handle, "sign-out-gh-1");
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1"));
    assert_eq!(stored.ok().flatten(), None);
    assert_eq!(
        label_of(cx, "account-detail-gh-1").as_deref(),
        Some("OAuth · octocat · Not signed in")
    );
    assert_eq!(saved_settings(&settings).accounts().len(), 1);
}

#[gpui_kit::test]
fn the_github_sign_in_dialog_shows_the_code_and_cancels_when_closed(cx: &mut TestAppContext) {
    let settings = TempSettings::new("github-sign-in-cancel", MANAGED_COPILOT);
    let fake = FakeGitHub::new(github_fixture::Outcome::Waits);
    let handle = open_copilot_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-gh-1");
    assert_eq!(
        label_of(cx, "github-sign-in-code").as_deref(),
        Some("One-time code ABCD-1234")
    );
    assert!(exists(cx, handle, "github-sign-in-copy") && exists(cx, handle, "github-sign-in-open"));
    let cancel = fake.0.cancel.lock().unwrap().clone().unwrap();
    press(cx, handle, "escape");
    assert!(!exists(cx, handle, "github-sign-in-code"));
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
}

#[gpui_kit::test]
fn a_failed_github_sign_in_says_so(cx: &mut TestAppContext) {
    let settings = TempSettings::new("github-sign-in-fails", MANAGED_COPILOT);
    let fake = FakeGitHub::new(github_fixture::Outcome::Fails);
    let handle = open_copilot_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-gh-1");
    assert_eq!(
        label_of(cx, "github-sign-in-error").as_deref(),
        Some("The GitHub CLI didn't complete the sign-in.")
    );
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
}

#[gpui_kit::test]
fn a_github_user_already_added_is_not_added_twice(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-duplicate",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "gh-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "cli-1", "providerId": "Copilot", "displayLabel": "CLI", "enabled": true,
              "authenticationMethod": "CommandLine", "externalAccountId": "OctoCat" } ] }"#,
    );
    let fake = FakeGitHub::new(github_fixture::Outcome::SignsIn("octocat"));
    let handle = open_copilot_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-gh-1");
    assert_eq!(
        label_of(cx, "github-sign-in-error").as_deref(),
        Some("octocat is already added as “CLI”.")
    );
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1"));
    assert_eq!(stored.ok().flatten(), None, "no token is kept for the refused sign-in");
}

#[gpui_kit::test]
fn org_billing_settings_are_saved_per_copilot_account(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-billing",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cli-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "CommandLine", "externalAccountId": "dev" } ] }"#,
    );
    let handle = open_copilot_accounts(cx, &settings, &FakeGitHub::default());
    click(cx, handle, "edit-cli-1");
    // Name, method, username, then the three billing fields.
    for _ in 0..3 {
        press(cx, handle, "tab");
    }
    type_text(cx, handle, "acme");
    press(cx, handle, "tab");
    type_text(cx, handle, "acme-eng");
    press(cx, handle, "tab");
    type_text(cx, handle, "lots");
    click(cx, handle, "account-save");
    assert_eq!(
        label_of(cx, "account-error").as_deref(),
        Some("The pool total is a whole number of AI credits.")
    );
    cx.update_window(handle, |_, window, cx| window.input("\u{8}\u{8}\u{8}\u{8}", cx))
        .unwrap();
    press(cx, handle, "ctrl-a");
    type_text(cx, handle, "12,000");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    let record = &saved.accounts()[0];
    assert_eq!(record.copilot_enterprise.as_deref(), Some("acme"));
    assert_eq!(record.copilot_organization.as_deref(), Some("acme-eng"));
    assert_eq!(record.copilot_pool_total, Some(12_000));
    assert_eq!(record.external_id.as_deref(), Some("dev"));
}

#[gpui_kit::test]
fn a_copilot_account_with_org_billing_asks_github_for_billing_access(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-billing-scope",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "gh-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth", "copilotEnterprise": "acme", "copilotOrganization": "acme-eng" },
            { "id": "gh-2", "providerId": "Copilot", "displayLabel": "Home", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    let fake = FakeGitHub::new(github_fixture::Outcome::SignsIn("octocat"));
    let handle = open_copilot_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-gh-1");
    assert_eq!(*fake.0.scopes.lock().unwrap(), ["manage_billing:enterprise"]);
    *fake.0.outcome.lock().unwrap() = github_fixture::Outcome::SignsIn("homeuser");
    click(cx, handle, "sign-in-gh-2");
    assert!(
        fake.0.scopes.lock().unwrap().is_empty(),
        "no extra scope without org billing"
    );
}

#[gpui_kit::test]
fn a_sign_in_that_cant_be_saved_keeps_the_previous_token(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-rollback",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "gh-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "olduser" } ] }"#,
    );
    let fake = FakeGitHub::new(github_fixture::Outcome::SignsIn("newuser"));
    let handle = open_copilot_accounts(cx, &settings, &fake);
    cx.update(|cx| {
        codexbar_store::credentials::write_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1", "gho_old")
    })
    .unwrap();
    // The settings file can't be written while the sign-in runs.
    let file = settings.0.join("settings.json");
    let writable = std::fs::metadata(&file).unwrap().permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&file, read_only).unwrap();
    click(cx, handle, "sign-in-gh-1");
    std::fs::set_permissions(&file, writable).unwrap();
    let error = label_of(cx, "github-sign-in-error").unwrap_or_default();
    assert!(error.starts_with("The sign-in couldn't be saved"), "{error}");
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1"));
    assert!(
        stored.ok().flatten().as_deref() == Some("gho_old"),
        "the account keeps its previous user's token"
    );
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some("olduser")
    );
}

#[gpui_kit::test]
fn an_unreadable_copilot_token_is_shown_as_such(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-unreadable",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "gh-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "octocat" } ] }"#,
    );
    cx.update(|cx| crate::github_sign_in::init_with(cx, Arc::new(FakeGitHub::default())));
    let _handle = open_settings_page(cx, &settings, MemoryCredentialStore::failing(), 1);
    assert_eq!(
        label_of(cx, "account-detail-gh-1").as_deref(),
        Some("OAuth · Its token couldn't be read from Windows Credential Manager")
    );
}

#[gpui_kit::test]
fn a_sign_in_cancelled_as_github_approves_keeps_nothing(cx: &mut TestAppContext) {
    let settings = TempSettings::new("github-cancel-late", MANAGED_COPILOT);
    let fake = FakeGitHub::new(github_fixture::Outcome::SignsInAsCancelled("octocat"));
    let handle = open_copilot_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-gh-1");
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "gh-1"));
    assert_eq!(stored.ok().flatten(), None, "a cancelled sign-in keeps no token");
}

#[gpui_kit::test]
fn org_billing_on_a_github_cli_account_needs_its_username(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-billing-username",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cli-1", "providerId": "Copilot", "displayLabel": "Every gh account", "enabled": true,
              "authenticationMethod": "CommandLine" } ] }"#,
    );
    let handle = open_copilot_accounts(cx, &settings, &FakeGitHub::default());
    click(cx, handle, "edit-cli-1");
    // Name, method, username (left blank), then Enterprise.
    for _ in 0..3 {
        press(cx, handle, "tab");
    }
    type_text(cx, handle, "acme");
    click(cx, handle, "account-save");
    assert_eq!(
        label_of(cx, "account-error").as_deref(),
        Some("Org billing needs this account's GitHub username.")
    );
    assert_eq!(saved_settings(&settings).accounts()[0].copilot_enterprise, None);
}

#[gpui_kit::test]
fn an_organization_card_is_owned_while_an_account_bills_it(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "github-org-owned",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cli-1", "providerId": "Copilot", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "CommandLine", "externalAccountId": "dev",
              "copilotEnterprise": "acme", "copilotOrganization": "Acme-Eng" },
            { "id": "cli-2", "providerId": "Copilot", "displayLabel": "Off", "enabled": false,
              "authenticationMethod": "CommandLine", "externalAccountId": "other",
              "copilotEnterprise": "acme", "copilotOrganization": "other-org" } ] }"#,
    );
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("cli-1").map(String::as_str), Some("copilot-dev"));
    assert_eq!(owned.get("cli-1#org").map(String::as_str), Some("copilotorg-acme-eng"));
    assert!(!owned.contains_key("cli-2#org"), "a switched-off account bills nothing");
}

// Claude accounts CodexBar signs in (#80): a Claude Code window for the account's own folder, its identity, sign-out,
// cancellation and removal, with a fake standing in for Claude Code.

mod claude_fixture {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codexbar_providers::claude_cli::ClaudeCliError;

    use crate::claude_sign_in::{PendingSignIn, SignInService};

    #[derive(Clone, Default)]
    pub enum Outcome {
        SignsIn(&'static str),
        Fails,
        #[default]
        Waits,
    }

    #[derive(Default)]
    pub struct State {
        pub outcome: Mutex<Outcome>,
        pub opened: Mutex<Vec<PathBuf>>,
        pub signed_out: Mutex<Vec<PathBuf>>,
        pub cancel: Mutex<Option<Arc<AtomicBool>>>,
    }

    #[derive(Default, Clone)]
    pub struct FakeClaude(pub Arc<State>);

    impl FakeClaude {
        pub fn new(outcome: Outcome) -> Self {
            let fake = Self::default();
            *fake.0.outcome.lock().unwrap() = outcome;
            fake
        }
    }

    impl SignInService for FakeClaude {
        fn begin(&self, config: &Path, _: &AtomicBool) -> Result<Box<dyn PendingSignIn>, ClaudeCliError> {
            self.0.opened.lock().unwrap().push(config.to_owned());
            Ok(Box::new(Pending {
                state: self.0.clone(),
                config: config.to_owned(),
            }))
        }

        fn sign_out(&self, config: &Path) -> Result<(), ClaudeCliError> {
            self.0.signed_out.lock().unwrap().push(config.to_owned());
            let _ = std::fs::remove_file(config.join(".credentials.json"));
            Ok(())
        }
    }

    struct Pending {
        state: Arc<State>,
        config: PathBuf,
    }

    impl PendingSignIn for Pending {
        fn finish(self: Box<Self>, _: Duration, cancel: Arc<AtomicBool>) -> Result<(), ClaudeCliError> {
            *self.state.cancel.lock().unwrap() = Some(cancel.clone());
            match self.state.outcome.lock().unwrap().clone() {
                Outcome::SignsIn(user) => {
                    sign_in(&self.config, user);
                    Ok(())
                }
                Outcome::Fails => Err(ClaudeCliError::Failed),
                Outcome::Waits => {
                    assert!(!cancel.load(Ordering::SeqCst));
                    Err(ClaudeCliError::Cancelled)
                }
            }
        }
    }

    /// What Claude Code leaves in a folder after signing `user` in.
    pub fn sign_in(config: &Path, user: &str) {
        std::fs::create_dir_all(config).unwrap();
        std::fs::write(
            config.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-test","refreshToken":"r","expiresAt":4102444800000,"subscriptionType":"max"}}"#,
        )
        .unwrap();
        std::fs::write(
            config.join(".claude.json"),
            format!(
                r#"{{"oauthAccount":{{"accountUuid":"{user}","organizationUuid":"org-1","emailAddress":"{user}@example.com"}}}}"#
            ),
        )
        .unwrap();
    }
}

use claude_fixture::FakeClaude;

const MANAGED_CLAUDE: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "cl-1", "providerId": "Claude", "displayLabel": "Work", "enabled": true,
      "authenticationMethod": "BrowserSession" } ] }"#;

fn open_claude_accounts(cx: &mut TestAppContext, settings: &TempSettings, fake: &FakeClaude) -> AnyWindowHandle {
    let fake = fake.clone();
    cx.update(|cx| crate::claude_sign_in::init_with(cx, Arc::new(fake)));
    open_settings_page(cx, settings, MemoryCredentialStore::default(), 1)
}

fn claude_folder(settings: &TempSettings, id: &str) -> PathBuf {
    settings.0.join("claude").join(id)
}

#[gpui_kit::test]
fn a_claude_account_signs_in_through_claude_code_and_shows_who_it_is(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-sign-in", MANAGED_CLAUDE);
    let fake = FakeClaude::new(claude_fixture::Outcome::SignsIn("dev"));
    let handle = open_claude_accounts(cx, &settings, &fake);
    assert_eq!(
        label_of(cx, "account-detail-cl-1").as_deref(),
        Some("Browser session · Not signed in")
    );
    let requests = cx.update(|cx| SettingsHub::refresh_requests(cx));
    click(cx, handle, "sign-in-cl-1");
    let folder = claude_folder(&settings, "cl-1");
    assert_eq!(*fake.0.opened.lock().unwrap(), std::slice::from_ref(&folder));
    assert!(
        !exists(cx, handle, "claude-sign-in-status"),
        "the dialog closes once signed in"
    );
    let identity = codexbar_providers::claude::signed_in_identity(&folder.join(".claude.json"))
        .unwrap()
        .0;
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(
        label_of(cx, "account-detail-cl-1").as_deref(),
        Some("Browser session · dev@example.com")
    );
    assert!(cx.update(|cx| SettingsHub::refresh_requests(cx)) > requests);
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("cl-1"), Some(&identity.as_str().to_owned()));

    click(cx, handle, "sign-out-cl-1");
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&folder));
    assert_eq!(
        label_of(cx, "account-detail-cl-1").as_deref(),
        Some("Browser session · Not signed in")
    );
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(identity.as_str())
    );
}

#[gpui_kit::test]
fn closing_the_claude_dialog_closes_its_window(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-cancel", MANAGED_CLAUDE);
    let fake = FakeClaude::new(claude_fixture::Outcome::Waits);
    let handle = open_claude_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cl-1");
    let status = label_of(cx, "claude-sign-in-status").unwrap();
    assert!(status.starts_with("A Claude Code window opened"), "{status}");
    let cancel = fake.0.cancel.lock().unwrap().clone().unwrap();
    press(cx, handle, "escape");
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
}

#[gpui_kit::test]
fn a_failed_claude_sign_in_says_so(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-fails", MANAGED_CLAUDE);
    let handle = open_claude_accounts(cx, &settings, &FakeClaude::new(claude_fixture::Outcome::Fails));
    click(cx, handle, "sign-in-cl-1");
    assert_eq!(
        label_of(cx, "claude-sign-in-error").as_deref(),
        Some("Claude Code didn't complete the sign-in.")
    );
}

#[gpui_kit::test]
fn removing_a_claude_account_signs_it_out_and_deletes_its_folder(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-remove", MANAGED_CLAUDE);
    let folder = claude_folder(&settings, "cl-1");
    claude_fixture::sign_in(&folder, "dev");
    let fake = FakeClaude::default();
    let handle = open_claude_accounts(cx, &settings, &fake);
    click(cx, handle, "remove-cl-1");
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&folder));
    assert!(!folder.exists());
}

#[gpui_kit::test]
fn adding_a_claude_account_with_a_browser_session_signs_it_in(cx: &mut TestAppContext) {
    let settings = TempSettings::new("claude-add", "{}");
    let fake = FakeClaude::new(claude_fixture::Outcome::SignsIn("dev"));
    let handle = open_claude_accounts(cx, &settings, &fake);
    click(cx, handle, "add-Claude");
    type_text(cx, handle, "Work");
    // From OAuth (Claude's default) one step down to Browser session; the first Down opens the list.
    press(cx, handle, "tab");
    for _ in 0..2 {
        press(cx, handle, "down");
    }
    press(cx, handle, "enter");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    let record = saved
        .accounts()
        .iter()
        .find(|record| record.method == codexbar_store::settings::AuthMethod::BrowserSession)
        .unwrap();
    // Claude Code's own sign-in, shown until now as the implicit account, stays as an account of its own.
    let methods: Vec<_> = saved.accounts().iter().map(|record| record.method).collect();
    assert_eq!(
        methods,
        [
            codexbar_store::settings::AuthMethod::OAuth,
            codexbar_store::settings::AuthMethod::BrowserSession
        ]
    );
    assert_eq!(*fake.0.opened.lock().unwrap(), [claude_folder(&settings, &record.id)]);
    assert!(record.external_id.is_some());
}

#[gpui_kit::test]
fn each_claude_account_gets_its_own_adapter_and_shares_none(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "claude-adapters",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cl-1", "providerId": "Claude", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "BrowserSession", "externalAccountId": "claude-aaaaaaaaaaaa" },
            { "id": "cl-2", "providerId": "Claude", "displayLabel": "Again", "enabled": true,
              "authenticationMethod": "BrowserSession", "externalAccountId": "claude-aaaaaaaaaaaa" },
            { "id": "cl-3", "providerId": "Claude", "displayLabel": "Home", "enabled": true,
              "authenticationMethod": "BrowserSession" } ] }"#,
    );
    // Both folders are signed in to the same Claude account.
    claude_fixture::sign_in(&claude_folder(&settings, "cl-1"), "same");
    claude_fixture::sign_in(&claude_folder(&settings, "cl-2"), "same");
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let claude: Vec<Option<String>> = adapters
        .iter()
        .filter(|provider| provider.name() == "Claude")
        .map(|provider| provider.account_id().map(str::to_owned))
        .collect();
    // One identity shows once; an account not signed in yet reports under its record.
    assert_eq!(
        claude,
        [Some("claude-aaaaaaaaaaaa".to_owned()), Some("cl-3".to_owned())]
    );
}

#[gpui_kit::test]
fn a_signed_out_claude_account_doesnt_hide_a_signed_in_one(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "claude-signed-out-claim",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cl-1", "providerId": "Claude", "displayLabel": "Old", "enabled": true,
              "authenticationMethod": "BrowserSession", "externalAccountId": "claude-aaaaaaaaaaaa" },
            { "id": "cl-2", "providerId": "Claude", "displayLabel": "Current", "enabled": true,
              "authenticationMethod": "BrowserSession", "externalAccountId": "claude-aaaaaaaaaaaa" } ] }"#,
    );
    // Only the second folder is signed in.
    claude_fixture::sign_in(&claude_folder(&settings, "cl-2"), "same");
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let labels: Vec<Option<String>> = adapters
        .iter()
        .filter(|provider| provider.name() == "Claude")
        .map(|provider| provider.account_label().map(str::to_owned))
        .collect();
    // Only the signed-in folder shows the account; the signed-out one would only add a failure for it.
    assert_eq!(labels, [Some("Current".to_owned())]);
}

#[gpui_kit::test]
fn claude_codes_own_folder_keeps_the_account_neutral_id(cx: &mut TestAppContext) {
    // Other Claude apps can rename the account in its .claude.json without changing the CLI's credentials, so it is
    // never named by that file.
    let settings = TempSettings::new(
        "claude-default-id",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cl-own", "providerId": "Claude", "displayLabel": "Mine", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "cl-1", "providerId": "Claude", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "BrowserSession" } ] }"#,
    );
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let own = adapters
        .iter()
        .find(|provider| provider.name() == "Claude" && provider.account_label() == Some("Mine"))
        .expect("Claude Code's own folder has an adapter");
    // Configured among several, it reports under its record, never an identity read from the shared profile.
    assert_eq!(own.account_id(), Some("cl-own"));
}

// Cursor accounts CodexBar signs in (#81): the Cursor CLI's sign-in in the account's own folder, the confirmation
// before replacing a sign-in, cancellation, failure and removal, with a fake standing in for the Cursor CLI.

mod cursor_fixture {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codexbar_providers::cursor_cli::CursorCliError;

    use crate::cursor_sign_in::{PendingSignIn, SignInService};

    #[derive(Clone, Default)]
    pub enum Outcome {
        SignsIn(&'static str),
        Fails,
        #[default]
        Waits,
    }

    #[derive(Default)]
    pub struct State {
        pub outcome: Mutex<Outcome>,
        pub begun: Mutex<Vec<PathBuf>>,
        pub signed_out: Mutex<Vec<PathBuf>>,
        pub cancel: Mutex<Option<Arc<AtomicBool>>>,
        pub user: Mutex<Option<String>>,
    }

    #[derive(Default, Clone)]
    pub struct FakeCursor(pub Arc<State>);

    impl FakeCursor {
        pub fn new(outcome: Outcome) -> Self {
            let fake = Self::default();
            *fake.0.outcome.lock().unwrap() = outcome;
            fake
        }
    }

    impl SignInService for FakeCursor {
        fn begin(&self, home: &Path, _: &AtomicBool) -> Result<Box<dyn PendingSignIn>, CursorCliError> {
            self.0.begun.lock().unwrap().push(home.to_owned());
            Ok(Box::new(Pending {
                state: self.0.clone(),
                home: home.to_owned(),
            }))
        }

        fn sign_out(&self, home: &Path) -> Result<(), CursorCliError> {
            self.0.signed_out.lock().unwrap().push(home.to_owned());
            let _ = std::fs::remove_file(codexbar_providers::cursor_cli::auth_path(home));
            Ok(())
        }

        fn email(&self, _: &Path) -> Option<String> {
            self.0
                .user
                .lock()
                .unwrap()
                .clone()
                .map(|user| format!("{user}@example.com"))
        }
    }

    struct Pending {
        state: Arc<State>,
        home: PathBuf,
    }

    impl PendingSignIn for Pending {
        fn url(&self) -> String {
            "https://cursor.com/loginDeepControl?challenge=x".to_owned()
        }

        fn finish(self: Box<Self>, _: Duration, cancel: Arc<AtomicBool>) -> Result<(), CursorCliError> {
            *self.state.cancel.lock().unwrap() = Some(cancel.clone());
            match self.state.outcome.lock().unwrap().clone() {
                Outcome::SignsIn(user) => {
                    sign_in(&self.home, user);
                    *self.state.user.lock().unwrap() = Some(user.to_owned());
                    Ok(())
                }
                Outcome::Fails => Err(CursorCliError::Failed),
                Outcome::Waits => {
                    assert!(!cancel.load(Ordering::SeqCst));
                    Err(CursorCliError::Cancelled)
                }
            }
        }
    }

    /// What the Cursor CLI leaves in a folder after signing `user` in: a token whose subject is that user.
    pub fn sign_in(home: &Path, user: &str) {
        let path = codexbar_providers::cursor_cli::auth_path(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let claims = serde_json::json!({"sub": format!("auth0|{user}"), "type": "session"}).to_string();
        let token = format!("e30.{}.sig", base64url(claims.as_bytes()));
        std::fs::write(
            path,
            serde_json::json!({"accessToken": token, "refreshToken": "r"}).to_string(),
        )
        .unwrap();
    }

    fn base64url(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (ix, byte)| acc | u32::from(*byte) << (16 - 8 * ix));
            for ix in 0..=chunk.len() {
                out.push(DIGITS[(n >> (18 - 6 * ix) & 63) as usize] as char);
            }
        }
        out
    }
}

use cursor_fixture::FakeCursor;

const MANAGED_CURSOR: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "cu-1", "providerId": "Cursor", "displayLabel": "Work", "enabled": true,
      "authenticationMethod": "OAuth" } ] }"#;

fn open_cursor_accounts(cx: &mut TestAppContext, settings: &TempSettings, fake: &FakeCursor) -> AnyWindowHandle {
    let fake = fake.clone();
    cx.update(|cx| crate::cursor_sign_in::init_with(cx, Arc::new(fake)));
    open_settings_page(cx, settings, MemoryCredentialStore::default(), 1)
}

fn cursor_home(settings: &TempSettings, id: &str) -> PathBuf {
    settings.0.join("cursor").join(id)
}

#[gpui_kit::test]
fn a_cursor_account_signs_in_through_the_cursor_cli_and_shows_who_it_is(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-sign-in", MANAGED_CURSOR);
    let fake = FakeCursor::new(cursor_fixture::Outcome::SignsIn("dev"));
    let handle = open_cursor_accounts(cx, &settings, &fake);
    assert_eq!(
        label_of(cx, "account-detail-cu-1").as_deref(),
        Some("OAuth · Not signed in")
    );
    click(cx, handle, "sign-in-cu-1");
    let home = cursor_home(&settings, "cu-1");
    assert_eq!(*fake.0.begun.lock().unwrap(), std::slice::from_ref(&home));
    assert!(
        !exists(cx, handle, "cursor-sign-in-status"),
        "the dialog closes once signed in"
    );
    let identity =
        codexbar_providers::cursor::signed_in_account(&codexbar_providers::cursor_cli::auth_path(&home)).unwrap();
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(
        label_of(cx, "account-detail-cu-1").as_deref(),
        Some("OAuth · dev@example.com")
    );
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("cu-1"), Some(&identity.as_str().to_owned()));

    click(cx, handle, "sign-out-cu-1");
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&home));
    assert_eq!(
        label_of(cx, "account-detail-cu-1").as_deref(),
        Some("OAuth · Not signed in")
    );
}

#[gpui_kit::test]
fn replacing_a_cursor_sign_in_asks_first_and_updates_who_it_is(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-replace", MANAGED_CURSOR);
    let fake = FakeCursor::new(cursor_fixture::Outcome::SignsIn("first"));
    let handle = open_cursor_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cu-1");
    let first = saved_settings(&settings).accounts()[0].external_id.clone().unwrap();
    assert_eq!(
        label_of(cx, "account-detail-cu-1").as_deref(),
        Some("OAuth · first@example.com")
    );

    // Signing in again asks first; Escape keeps the sign-in as it is.
    *fake.0.outcome.lock().unwrap() = cursor_fixture::Outcome::SignsIn("second");
    click(cx, handle, "sign-in-cu-1");
    assert_eq!(
        fake.0.begun.lock().unwrap().len(),
        1,
        "nothing starts before the confirmation"
    );
    press(cx, handle, "escape");
    assert_eq!(fake.0.begun.lock().unwrap().len(), 1);
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some(first.as_str())
    );

    // Confirmed, the new account replaces it, and the row says who it is now.
    click(cx, handle, "sign-in-cu-1");
    press(cx, handle, "enter");
    assert_eq!(fake.0.begun.lock().unwrap().len(), 2);
    let second = saved_settings(&settings).accounts()[0].external_id.clone().unwrap();
    assert_ne!(first, second);
    assert_eq!(
        label_of(cx, "account-detail-cu-1").as_deref(),
        Some("OAuth · second@example.com")
    );
}

#[gpui_kit::test]
fn closing_the_cursor_dialog_cancels_and_a_failure_says_so(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-cancel", MANAGED_CURSOR);
    let fake = FakeCursor::new(cursor_fixture::Outcome::Waits);
    let handle = open_cursor_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-cu-1");
    assert!(exists(cx, handle, "cursor-sign-in-open") && exists(cx, handle, "cursor-sign-in-copy"));
    let cancel = fake.0.cancel.lock().unwrap().clone().unwrap();
    press(cx, handle, "escape");
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);

    *fake.0.outcome.lock().unwrap() = cursor_fixture::Outcome::Fails;
    click(cx, handle, "sign-in-cu-1");
    assert_eq!(
        label_of(cx, "cursor-sign-in-error").as_deref(),
        Some("The Cursor CLI didn't complete the sign-in.")
    );
}

#[gpui_kit::test]
fn removing_a_cursor_account_signs_it_out_and_deletes_its_folder(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-remove", MANAGED_CURSOR);
    let home = cursor_home(&settings, "cu-1");
    cursor_fixture::sign_in(&home, "dev");
    let fake = FakeCursor::default();
    let handle = open_cursor_accounts(cx, &settings, &fake);
    click(cx, handle, "remove-cu-1");
    press(cx, handle, "enter");
    assert!(saved_settings(&settings).accounts().is_empty());
    assert_eq!(*fake.0.signed_out.lock().unwrap(), std::slice::from_ref(&home));
    assert!(!home.exists());
}

#[gpui_kit::test]
fn adding_a_cursor_account_with_oauth_keeps_the_cursor_apps_sign_in(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-add", "{}");
    let fake = FakeCursor::new(cursor_fixture::Outcome::SignsIn("dev"));
    let handle = open_cursor_accounts(cx, &settings, &fake);
    click(cx, handle, "add-Cursor");
    type_text(cx, handle, "Work");
    // From Automatic, four Downs (the first opens the list) reach OAuth.
    press(cx, handle, "tab");
    for _ in 0..4 {
        press(cx, handle, "down");
    }
    press(cx, handle, "enter");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    let methods: Vec<_> = saved.accounts().iter().map(|record| record.method).collect();
    assert_eq!(
        methods,
        [
            codexbar_store::settings::AuthMethod::Automatic,
            codexbar_store::settings::AuthMethod::OAuth
        ]
    );
    let record = &saved.accounts()[1];
    assert_eq!(*fake.0.begun.lock().unwrap(), [cursor_home(&settings, &record.id)]);
}

#[gpui_kit::test]
fn each_cursor_account_gets_its_own_adapter(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "cursor-adapters",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cu-1", "providerId": "Cursor", "displayLabel": "Work", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "cu-2", "providerId": "Cursor", "displayLabel": "Again", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "cu-3", "providerId": "Cursor", "displayLabel": "Home", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    // Work and Again are signed in to the same Cursor account; Home isn't signed in.
    cursor_fixture::sign_in(&cursor_home(&settings, "cu-1"), "same");
    cursor_fixture::sign_in(&cursor_home(&settings, "cu-2"), "same");
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let labels: Vec<Option<String>> = adapters
        .iter()
        .filter(|provider| provider.name() == "Cursor")
        .map(|provider| provider.account_label().map(str::to_owned))
        .collect();
    assert_eq!(labels, [Some("Work".to_owned()), Some("Home".to_owned())]);
}

#[gpui_kit::test]
fn a_signed_out_cursor_account_doesnt_hide_a_signed_in_one(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "cursor-signed-out-first",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "cu-1", "providerId": "Cursor", "displayLabel": "Old", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "cursor-aaaaaaaaaaaa" },
            { "id": "cu-2", "providerId": "Cursor", "displayLabel": "Current", "enabled": true,
              "authenticationMethod": "OAuth", "externalAccountId": "cursor-aaaaaaaaaaaa" } ] }"#,
    );
    cursor_fixture::sign_in(&cursor_home(&settings, "cu-2"), "same");
    cx.update(|cx| SettingsHub::init_with(cx, &settings.0, Arc::new(MemoryCredentialStore::default())));
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let labels: Vec<Option<String>> = adapters
        .iter()
        .filter(|provider| provider.name() == "Cursor")
        .map(|provider| provider.account_label().map(str::to_owned))
        .collect();
    assert_eq!(labels, [Some("Current".to_owned())], "the live sign-in wins");
}

#[gpui_kit::test]
fn a_cursor_sign_in_that_cant_be_saved_says_so(cx: &mut TestAppContext) {
    let settings = TempSettings::new("cursor-save-fails", MANAGED_CURSOR);
    let fake = FakeCursor::new(cursor_fixture::Outcome::SignsIn("dev"));
    let handle = open_cursor_accounts(cx, &settings, &fake);
    let file = settings.0.join("settings.json");
    let writable = std::fs::metadata(&file).unwrap().permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&file, read_only).unwrap();
    click(cx, handle, "sign-in-cu-1");
    std::fs::set_permissions(&file, writable).unwrap();
    let error = label_of(cx, "cursor-sign-in-error").unwrap_or_default();
    assert!(error.starts_with("The sign-in couldn't be saved"), "{error}");
    assert_eq!(saved_settings(&settings).accounts()[0].external_id, None);
}

// OpenCode accounts on the Console's device approval (#102). The fake never reaches OpenCode or opens a browser.

mod opencode_fixture {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use codexbar_providers::opencode_console::{ConsoleCredential, SignInError};

    use crate::opencode_sign_in::{Approval, PendingApproval, SignInService};

    #[derive(Clone, Default)]
    pub enum Outcome {
        /// Approved for this workspace and user.
        SignsIn(&'static str, &'static str),
        Denied,
        #[default]
        Waits,
    }

    #[derive(Default)]
    pub struct State {
        pub outcome: Mutex<Outcome>,
        pub begun: AtomicUsize,
        pub cancel: Mutex<Option<Arc<AtomicBool>>>,
    }

    #[derive(Default, Clone)]
    pub struct FakeOpenCode(pub Arc<State>);

    impl FakeOpenCode {
        pub fn new(outcome: Outcome) -> Self {
            let fake = Self::default();
            *fake.0.outcome.lock().unwrap() = outcome;
            fake
        }
    }

    pub fn credential(workspace: &str, user: &str) -> ConsoleCredential {
        ConsoleCredential {
            access_token: format!("at-{workspace}"),
            refresh_token: format!("rt-{workspace}"),
            expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            workspace_id: workspace.to_owned(),
            user_id: user.to_owned(),
        }
    }

    impl SignInService for FakeOpenCode {
        fn begin(&self, _: &AtomicBool) -> Result<Box<dyn PendingApproval>, SignInError> {
            self.0.begun.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Pending(self.0.clone())))
        }
    }

    struct Pending(Arc<State>);

    impl PendingApproval for Pending {
        fn approval(&self) -> Approval {
            Approval {
                code: Some("HSPH-FPQV".to_owned()),
                url: "https://opencode.ai/console/device?user_code=HSPH-FPQV".to_owned(),
            }
        }

        fn finish(self: Box<Self>, cancel: Arc<AtomicBool>) -> Result<ConsoleCredential, SignInError> {
            *self.0.cancel.lock().unwrap() = Some(cancel.clone());
            match self.0.outcome.lock().unwrap().clone() {
                Outcome::SignsIn(workspace, user) => Ok(credential(workspace, user)),
                Outcome::Denied => Err(SignInError::Denied),
                Outcome::Waits => {
                    assert!(!cancel.load(Ordering::SeqCst));
                    Err(SignInError::Cancelled)
                }
            }
        }
    }
}

use opencode_fixture::FakeOpenCode;

const CONSOLE_OPENCODE: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
      "authenticationMethod": "OAuth" } ] }"#;

fn open_opencode_accounts(cx: &mut TestAppContext, settings: &TempSettings, fake: &FakeOpenCode) -> AnyWindowHandle {
    let fake = fake.clone();
    cx.update(|cx| crate::opencode_sign_in::init_with(cx, Arc::new(fake)));
    let handle = open_settings_page(cx, settings, MemoryCredentialStore::default(), 1);
    // OpenCode Go is the sixth group; Settings renders only groups in view, so scroll it in.
    cx.update_window(handle, |_, window, cx| {
        window.within("settings-sidebar").click("0-1-5", cx)
    })
    .unwrap();
    cx.run_until_parked();
    handle
}

fn saved_console_sign_in(
    cx: &mut TestAppContext,
    id: &str,
) -> Option<codexbar_providers::opencode_console::ConsoleCredential> {
    cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), id))
        .ok()
        .flatten()
        .and_then(|secret| codexbar_providers::opencode_console::ConsoleCredential::from_secret(&secret))
}

#[gpui_kit::test]
fn an_opencode_account_signs_in_through_the_console_and_tracks_its_workspace(cx: &mut TestAppContext) {
    let settings = TempSettings::new("opencode-sign-in", CONSOLE_OPENCODE);
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::SignsIn("wrk_01ABC", "usr_1"));
    let handle = open_opencode_accounts(cx, &settings, &fake);
    assert_eq!(
        label_of(cx, "account-detail-oc-1").as_deref(),
        Some("OAuth · Not signed in")
    );
    click(cx, handle, "sign-in-oc-1");
    assert!(
        !exists(cx, handle, "opencode-sign-in-status"),
        "the dialog closes once signed in"
    );
    let record = saved_settings(&settings).accounts()[0].clone();
    assert_eq!(record.workspace_id.as_deref(), Some("wrk_01ABC"));
    assert_eq!(record.external_id.as_deref(), Some("usr_1"));
    let saved = saved_console_sign_in(cx, "oc-1").expect("the sign-in is kept for the account");
    assert_eq!(
        (saved.workspace_id.as_str(), saved.user_id.as_str()),
        ("wrk_01ABC", "usr_1")
    );
    assert_eq!(
        label_of(cx, "account-detail-oc-1").as_deref(),
        Some("OAuth · Workspace wrk_01ABC")
    );

    // The account reports under its own id, and no cookie account is added beside it.
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let opencode: Vec<_> = adapters.iter().filter(|a| a.name() == "OpenCode Go + Zen").collect();
    assert_eq!(opencode.len(), 1);
    assert_eq!(opencode[0].account_id(), Some("oc-1"));
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("oc-1").map(String::as_str), Some("oc-1"));

    // Signing out deletes CodexBar's copy and keeps the account bound to its workspace.
    click(cx, handle, "sign-out-oc-1");
    assert!(saved_console_sign_in(cx, "oc-1").is_none());
    assert_eq!(
        label_of(cx, "account-detail-oc-1").as_deref(),
        Some("OAuth · Workspace wrk_01ABC · Not signed in")
    );
    assert_eq!(
        saved_settings(&settings).accounts()[0].workspace_id.as_deref(),
        Some("wrk_01ABC")
    );
}

#[gpui_kit::test]
fn signing_an_opencode_account_in_again_cant_move_it_to_another_workspace(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-rebind",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth", "workspaceId": "wrk_01ABC", "externalAccountId": "usr_1" } ] }"#,
    );
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::SignsIn("wrk_02OTHER", "usr_1"));
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("This account tracks workspace wrk_01ABC. To track wrk_02OTHER, add another OpenCode account.")
    );
    assert!(saved_console_sign_in(cx, "oc-1").is_none(), "nothing is kept");

    press(cx, handle, "escape");
    *fake.0.outcome.lock().unwrap() = opencode_fixture::Outcome::SignsIn("wrk_01ABC", "usr_2");
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("This account was signed in by a different OpenCode user. Add another account to track this one.")
    );
    assert!(saved_console_sign_in(cx, "oc-1").is_none());
    assert_eq!(
        saved_settings(&settings).accounts()[0].external_id.as_deref(),
        Some("usr_1")
    );
}

#[gpui_kit::test]
fn an_opencode_workspace_already_added_is_not_added_twice(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-duplicate",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth", "workspaceId": "wrk_01ABC", "externalAccountId": "usr_1" },
            { "id": "oc-2", "providerId": "OpenCodeGo", "displayLabel": "Second", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::SignsIn("wrk_01ABC", "usr_1"));
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-oc-2");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("Workspace wrk_01ABC is already added as “Team”.")
    );
    assert!(saved_console_sign_in(cx, "oc-2").is_none());
}

#[gpui_kit::test]
fn the_opencode_dialog_shows_the_code_and_cancels_when_closed(cx: &mut TestAppContext) {
    let settings = TempSettings::new("opencode-cancel", CONSOLE_OPENCODE);
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::Waits);
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-code").as_deref(),
        Some("Approval code HSPH-FPQV")
    );
    assert!(exists(cx, handle, "opencode-sign-in-open"));
    let cancel = fake.0.cancel.lock().unwrap().clone().unwrap();
    press(cx, handle, "escape");
    assert!(!exists(cx, handle, "opencode-sign-in-code"));
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(saved_settings(&settings).accounts()[0].workspace_id, None);
}

#[gpui_kit::test]
fn a_declined_opencode_approval_says_so_and_keeps_nothing(cx: &mut TestAppContext) {
    let settings = TempSettings::new("opencode-denied", CONSOLE_OPENCODE);
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::Denied);
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("The approval was declined on the OpenCode page. Nothing was saved.")
    );
    assert!(saved_console_sign_in(cx, "oc-1").is_none());
    assert_eq!(saved_settings(&settings).accounts()[0].workspace_id, None);
}

#[gpui_kit::test]
fn a_new_opencode_account_starts_with_the_browser_approval(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-add",
        r#"{ "accountConfigurationVersion": 1, "accounts": [] }"#,
    );
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::Waits);
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "add-OpenCodeGo");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    let record = &saved.accounts()[0];
    assert_eq!(record.method, codexbar_store::settings::AuthMethod::OAuth);
    assert_eq!(record.workspace_id, None, "the workspace comes from the sign-in");
    assert!(
        exists(cx, handle, "opencode-sign-in-status"),
        "the sign-in starts right away"
    );
    assert_eq!(fake.0.begun.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[gpui_kit::test]
fn an_opencode_cookie_account_keeps_its_legacy_dashboard_id(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-cookie",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "go-1", "providerId": "OpenCodeGo", "displayLabel": "Go", "enabled": true,
              "authenticationMethod": "BrowserSession", "workspaceId": "wrk_01ABC" } ] }"#,
    );
    let _handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let opencode: Vec<_> = adapters.iter().filter(|a| a.name() == "OpenCode Go + Zen").collect();
    assert_eq!(opencode.len(), 1);
    assert_eq!(
        opencode[0].account_id(),
        Some("opencode"),
        "the cookie account keeps its legacy dashboard id"
    );
}

#[gpui_kit::test]
fn an_opencode_workspace_a_cookie_account_tracks_is_not_added_again(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-cookie-duplicate",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "go-1", "providerId": "OpenCodeGo", "displayLabel": "Go", "enabled": true,
              "authenticationMethod": "BrowserSession", "workspaceId": "wrk_01ABC" },
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::SignsIn("wrk_01ABC", "usr_1"));
    let handle = open_opencode_accounts(cx, &settings, &fake);
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("Workspace wrk_01ABC is already added as “Go”.")
    );
    assert!(saved_console_sign_in(cx, "oc-1").is_none());
}

/// Picks "Browser session" in the open account dialog when OAuth, the method before it, is selected and the name
/// field has the focus. The open list starts on the selected method, so the first Down stays there.
fn choose_browser_session(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    press(cx, handle, "tab");
    for _ in 0..2 {
        press(cx, handle, "down");
    }
    press(cx, handle, "enter");
}

#[gpui_kit::test]
fn only_one_opencode_account_reads_a_cookie(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-one-cookie",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "go-1", "providerId": "OpenCodeGo", "displayLabel": "Go", "enabled": true,
              "authenticationMethod": "BrowserSession", "workspaceId": "wrk_01ABC" } ] }"#,
    );
    let handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    click(cx, handle, "add-OpenCodeGo");
    choose_browser_session(cx, handle);
    click(cx, handle, "account-save");
    assert_eq!(
        label_of(cx, "account-error").as_deref(),
        Some("Only one OpenCode Go account can use a dashboard cookie. Use OAuth to add another workspace.")
    );
    assert_eq!(saved_settings(&settings).accounts().len(), 1);
}

#[gpui_kit::test]
fn an_opencode_account_leaving_oauth_keeps_the_cookie_it_was_given(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-leave-oauth",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth", "workspaceId": "wrk_01ABC", "externalAccountId": "usr_1" } ] }"#,
    );
    let handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    let sign_in = opencode_fixture::credential("wrk_01ABC", "usr_1").to_secret();
    cx.update(|cx| {
        codexbar_store::credentials::write_long(SettingsHub::global(cx).credentials().as_ref(), "oc-1", &sign_in)
    })
    .unwrap();
    click(cx, handle, "edit-oc-1");
    choose_browser_session(cx, handle);
    // The cookie field follows the method.
    press(cx, handle, "tab");
    type_text(cx, handle, "Fe26.2**new-cookie");
    click(cx, handle, "account-save");
    let saved = saved_settings(&settings);
    assert_eq!(
        saved.accounts()[0].method,
        codexbar_store::settings::AuthMethod::BrowserSession
    );
    let stored =
        cx.update(|cx| codexbar_store::credentials::read_long(SettingsHub::global(cx).credentials().as_ref(), "oc-1"));
    assert!(
        stored.ok().flatten().as_deref() == Some("Fe26.2**new-cookie"),
        "the new cookie is kept, not deleted with the old sign-in"
    );
}

#[gpui_kit::test]
fn a_cookie_account_for_a_workspace_an_oauth_account_tracks_is_refused(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-cookie-for-oauth-workspace",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth", "workspaceId": "wrk_01ABC", "externalAccountId": "usr_1" } ] }"#,
    );
    let handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    click(cx, handle, "add-OpenCodeGo");
    choose_browser_session(cx, handle);
    // The cookie, then the workspace.
    press(cx, handle, "tab");
    press(cx, handle, "tab");
    type_text(cx, handle, "wrk_01ABC");
    click(cx, handle, "account-save");
    assert_eq!(
        label_of(cx, "account-error").as_deref(),
        Some("Workspace wrk_01ABC is already added as “Team”.")
    );
    assert_eq!(saved_settings(&settings).accounts().len(), 1);
}

#[gpui_kit::test]
fn an_implicit_cookie_setup_keeps_running_beside_a_first_oauth_account(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-implicit-cookie",
        r#"{ "accountConfigurationVersion": 1, "openCodeGoWorkspaceId": "wrk_01OLD",
            "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    let _handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    let implicit = codexbar_store::settings::legacy_id(codexbar_store::settings::names::OPENCODE_GO, "");
    cx.update(|cx| {
        SettingsHub::global(cx)
            .credentials()
            .write(&implicit, "Fe26.2**old-cookie")
    })
    .unwrap();
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let ids: Vec<Option<&str>> = adapters
        .iter()
        .filter(|a| a.name() == "OpenCode Go + Zen")
        .map(|a| a.account_id())
        .collect();
    assert_eq!(ids, [Some("oc-1"), Some("opencode")], "the cookie setup still runs");
}

fn keep_implicit_cookie(cx: &mut TestAppContext) {
    let implicit = codexbar_store::settings::legacy_id(codexbar_store::settings::names::OPENCODE_GO, "");
    cx.update(|cx| {
        SettingsHub::global(cx)
            .credentials()
            .write(&implicit, "Fe26.2**old-cookie")
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_switched_off_cookie_account_is_not_stood_in_for(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-disabled-cookie",
        r#"{ "accountConfigurationVersion": 1, "openCodeGoWorkspaceId": "wrk_01OLD", "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth" },
            { "id": "go-1", "providerId": "OpenCodeGo", "displayLabel": "Old", "enabled": false,
              "authenticationMethod": "BrowserSession", "workspaceId": "wrk_01OLD" } ] }"#,
    );
    let _handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    keep_implicit_cookie(cx);
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let ids: Vec<Option<&str>> = adapters
        .iter()
        .filter(|a| a.name() == "OpenCode Go + Zen")
        .map(|a| a.account_id())
        .collect();
    assert_eq!(ids, [Some("oc-1")]);
}

#[gpui_kit::test]
fn the_implicit_cookie_workspace_is_not_added_again_through_oauth(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-implicit-duplicate",
        r#"{ "accountConfigurationVersion": 1, "openCodeGoWorkspaceId": "wrk_01ABC", "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth" } ] }"#,
    );
    let fake = FakeOpenCode::new(opencode_fixture::Outcome::SignsIn("wrk_01ABC", "usr_1"));
    let handle = open_opencode_accounts(cx, &settings, &fake);
    keep_implicit_cookie(cx);
    click(cx, handle, "sign-in-oc-1");
    assert_eq!(
        label_of(cx, "opencode-sign-in-error").as_deref(),
        Some("Workspace wrk_01ABC is already shown through the dashboard-cookie setup.")
    );
    assert!(saved_console_sign_in(cx, "oc-1").is_none());
}

#[gpui_kit::test]
fn a_cookie_that_cant_be_saved_with_the_account_gives_the_sign_in_back(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "opencode-cookie-rollback",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "oc-1", "providerId": "OpenCodeGo", "displayLabel": "Team", "enabled": true,
              "authenticationMethod": "OAuth", "workspaceId": "wrk_01ABC", "externalAccountId": "usr_1" } ] }"#,
    );
    let handle = open_opencode_accounts(cx, &settings, &FakeOpenCode::default());
    let sign_in = opencode_fixture::credential("wrk_01ABC", "usr_1").to_secret();
    cx.update(|cx| {
        codexbar_store::credentials::write_long(SettingsHub::global(cx).credentials().as_ref(), "oc-1", &sign_in)
    })
    .unwrap();
    click(cx, handle, "edit-oc-1");
    choose_browser_session(cx, handle);
    press(cx, handle, "tab");
    type_text(cx, handle, "Fe26.2**new-cookie");
    let file = settings.0.join("settings.json");
    let writable = std::fs::metadata(&file).unwrap().permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&file, read_only).unwrap();
    click(cx, handle, "account-save");
    std::fs::set_permissions(&file, writable).unwrap();
    assert!(label_of(cx, "account-error").is_some(), "the save failed");
    let saved = saved_console_sign_in(cx, "oc-1").expect("the OAuth account keeps its sign-in");
    assert_eq!(saved.workspace_id, "wrk_01ABC");
}

// Greptile (#103): review activity is a count, never a bar or a percentage.

#[gpui_kit::test]
fn a_greptile_row_shows_completed_reviews_as_a_count(cx: &mut TestAppContext) {
    let settings = TempSettings::new("greptile-row", "{}");
    let (handle, _) = open_dashboard(cx, &settings);
    assert!(
        label(cx, handle, "provider-badge-greptile").is_some(),
        "the table shows the account"
    );
    assert_eq!(
        label(cx, handle, "used-greptile").as_deref(),
        Some("Completed reviews: 37")
    );
    assert_eq!(label(cx, handle, "progress-cell-greptile").as_deref(), Some("No bar"));
    assert!(label(cx, handle, "severity-tag-greptile").is_none(), "no pill");
}

#[gpui_kit::test]
fn a_greptile_account_reads_with_its_own_key_under_its_own_id(cx: &mut TestAppContext) {
    let settings = TempSettings::new(
        "greptile-accounts",
        r#"{ "accountConfigurationVersion": 1, "accounts": [
            { "id": "gr-1", "providerId": "Greptile", "displayLabel": "Acme", "enabled": true,
              "authenticationMethod": "ApiKey" },
            { "id": "gr-2", "providerId": "Greptile", "displayLabel": "Side", "enabled": false,
              "authenticationMethod": "ApiKey" } ] }"#,
    );
    let _handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    let greptile: Vec<_> = adapters.iter().filter(|a| a.name() == "Greptile").collect();
    assert_eq!(greptile.len(), 1, "only the enabled account");
    assert_eq!(greptile[0].account_id(), Some("gr-1"));
    let owned = cx.update(|cx| crate::providers::owned_account_ids(SettingsHub::global(cx)));
    assert_eq!(owned.get("gr-1").map(String::as_str), Some("gr-1"));
}

#[gpui_kit::test]
fn greptile_stays_off_until_an_account_is_added(cx: &mut TestAppContext) {
    let settings = TempSettings::new("greptile-off", "{}");
    let _handle = open_settings_page(cx, &settings, MemoryCredentialStore::default(), 1);
    let adapters = cx.update(|cx| crate::providers::enabled(SettingsHub::global(cx)));
    assert!(adapters.iter().all(|a| a.name() != "Greptile"));
}
