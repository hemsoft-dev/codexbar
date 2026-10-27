//! Producer -> persisted accounts -> widget file -> actual Windows reader / GPUI preview.
use crate::{
    account_table::{AccountState, States},
    muse_fixture as fixture, widget_feed,
    widgets::cards::{self, Config, Size},
};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider, layout::Layout};
use codexbar_providers::UsageProvider as _;
use codexbar_store::{
    snapshots,
    widgets::{self, WidgetHealth},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "codexbar-273-roundtrip-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn muse_adapter_account_and_widget_roundtrip_covers_each_synthetic_state() {
    for (state, count, health) in [
        ("measured", 2, WidgetHealth::Fresh),
        ("zero", 2, WidgetHealth::Fresh),
        ("exhausted", 2, WidgetHealth::Fresh),
        ("partial", 1, WidgetHealth::Fresh),
        ("stale", 2, WidgetHealth::Stale),
        ("malformed", 0, WidgetHealth::Unavailable),
        ("null", 0, WidgetHealth::Unavailable),
        ("payg-unknown", 0, WidgetHealth::Unavailable),
        ("expired-entitlement-unknown", 0, WidgetHealth::Unavailable),
        ("timeout", 0, WidgetHealth::Unavailable),
        ("rate-command-failure", 0, WidgetHealth::Unavailable),
        ("transport", 0, WidgetHealth::Unavailable),
        ("missing-cli", 0, WidgetHealth::Unavailable),
        ("unsupported", 0, WidgetHealth::Unavailable),
        ("token-activity-only", 0, WidgetHealth::Unavailable),
    ] {
        let now = fixture::now(state);
        let mut states = States::new();
        let account = match fixture::provider(state).fetch(now) {
            Ok(mut accounts) => accounts.remove(0),
            Err(error) => {
                states.insert("muse-demo".into(), AccountState::Unavailable(error.to_string()));
                AccountSnapshot::new(AccountId::new("muse-demo"), Provider::MuseCode, Vec::new(), now)
            }
        };
        assert_eq!(account.metrics().len(), count, "{state}");
        let dir = Scratch::new();
        snapshots::save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        let mut restored = snapshots::load_snapshots(&dir.0);
        // A first-fetch placeholder has no source observation and isn't a last-good snapshot.
        if count == 0 {
            assert!(restored.is_empty());
            restored.push(account.clone());
        } else {
            assert_eq!(restored[0], account);
        }
        let feed = widget_feed::build(&restored, &states, &Layout::default(), now);
        assert_eq!(feed.accounts[0].health, health, "{state}");
        assert_eq!(feed.accounts[0].observed_at, account.observed_at());
        assert!(
            feed.accounts[0]
                .metrics
                .iter()
                .all(|m| m.sus.is_none() && !m.sus_eligible)
        );
        widgets::save_widget_snapshot(&dir.0, &feed).unwrap();
        let loaded = widgets::load_widget_snapshot(&dir.0).unwrap();
        let mut expected = feed.clone();
        for (actual, expected) in loaded
            .accounts
            .iter()
            .flat_map(|a| &a.metrics)
            .zip(expected.accounts.iter_mut().flat_map(|a| &mut a.metrics))
        {
            if let (Some(actual), Some(wanted)) = (actual.projected_percent, expected.projected_percent) {
                assert!(
                    (actual - wanted).abs() <= wanted.abs().max(1.0) * 1e-12,
                    "projection must survive JSON round-trip"
                );
                // JSON's float parser may round the forecast by one ULP. Every other wire field remains exact.
                expected.projected_percent = Some(actual);
            }
        }
        assert_eq!(loaded, expected);
        let views = cards::tile_views(&loaded, &Config::default(), Size::Medium, now).unwrap();
        assert_eq!(views.len(), 1);
        let serialized =
            serde_json::to_string(&cards::render(&Ok(loaded), &Config::default(), Size::Medium, now)).unwrap();
        assert!(!serialized.contains("Fixture User"));
        assert!(!serialized.contains("example.com"));
        if count == 0 {
            assert_eq!(views[0].percent, None);
            assert_eq!(views[0].note.as_deref(), Some("Unavailable"));
        }
        if state == "measured" {
            assert_eq!(views[0].percent, Some(15.0));
            assert!(views[0].line.as_ref().unwrap().contains("126%"));
        }
        if state == "zero" {
            assert_eq!(views[0].percent, Some(0.0));
        }
        if state == "exhausted" {
            assert_eq!(views[0].percent, Some(100.0));
        }
        if state == "partial" {
            assert_eq!(views[0].percent, Some(5.0));
            assert_eq!(account.metrics()[0].key(), "weekly");
        }
        if state == "stale" {
            assert!(views[0].note.as_ref().unwrap().contains("Last known"));
            assert!(views[0].line.as_ref().unwrap().contains("15% used"));
            assert!(!views[0].line.as_ref().unwrap().contains("at reset"));
        }
    }
}
#[test]
fn failed_refresh_retains_times_and_widget_reader_ages_independently_of_writer() {
    let account = fixture::provider("measured")
        .fetch(fixture::now("measured"))
        .unwrap()
        .remove(0);
    assert!(fixture::provider("timeout").fetch(fixture::now("measured")).is_err());
    let states = States::from([("muse-demo".into(), AccountState::Failed("Muse usage timed out".into()))]);
    let feed = widget_feed::build(
        std::slice::from_ref(&account),
        &states,
        &Layout::default(),
        fixture::now("measured"),
    );
    assert_eq!(feed.accounts[0].updated_at, Some(account.fetched_at()));
    assert_eq!(feed.accounts[0].observed_at, Some(fixture::base()));
    assert_eq!(feed.accounts[0].health, WidgetHealth::Stale);
    let fresh_feed = widget_feed::build(
        std::slice::from_ref(&account),
        &States::new(),
        &Layout::default(),
        fixture::now("measured"),
    );
    let old = cards::tile_views(&fresh_feed, &Config::default(), Size::Medium, fixture::now("stale")).unwrap();
    assert!(old[0].note.as_ref().unwrap().contains("Last known"));
    assert_eq!(old[0].status.as_deref(), Some("Stale"));
    let reset = account.metrics()[0].resets_at().unwrap() + chrono::Duration::seconds(1);
    let rolled = cards::tile_views(&fresh_feed, &Config::default(), Size::Medium, reset).unwrap();
    assert_eq!(rolled[0].percent, None);
    assert!(rolled[0].note.as_ref().unwrap().contains("reset"));
    assert!(
        codexbar_core::alerts::evaluate(
            &Default::default(),
            &Default::default(),
            &[account],
            fixture::now("stale")
        )
        .notify
        .is_empty()
    );
}
#[test]
fn existing_provider_wire_shape_is_unchanged_and_unknown_additions_are_compatible() {
    let account = AccountSnapshot::new(
        AccountId::new("existing"),
        Provider::Claude,
        vec![Metric::Window {
            label: "Weekly".into(),
            used: 0.42,
            resets_at: fixture::base() + chrono::Duration::days(3),
            pace: None,
            length: None,
        }],
        fixture::base(),
    );
    let feed = widget_feed::build(&[account], &States::new(), &Layout::default(), fixture::base());
    let value = serde_json::to_value(&feed).unwrap();
    let row = &value["accounts"][0];
    assert!(row.get("observedAt").is_none());
    assert!(row.get("freshForSeconds").is_none());
    assert!(row.get("allowanceSource").is_none());
    assert_eq!(row["metrics"][0]["usedPercent"], 42.0);
    assert_eq!(row["metrics"][0]["value"], "42% used");
    assert_eq!(value["version"], 1);
    let mut additive = value.clone();
    additive["accounts"][0]["futureUnknownField"] = serde_json::json!("ignored");
    assert_eq!(
        serde_json::from_value::<widgets::WidgetSnapshot>(additive).unwrap(),
        feed
    );
    let dir = Scratch::new();
    snapshots::save_snapshots(
        &dir.0,
        &codexbar_core::demo::demo_accounts(fixture::base(), &chrono::Utc),
    )
    .unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.0.join("snapshots.json")).unwrap()).unwrap();
    assert!(
        raw["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a.get("observedAt").is_none())
    );
    assert_eq!(raw["version"], 4);
    assert!(!snapshots::load_snapshots(&dir.0).is_empty());
}

#[test]
fn history_does_not_retimestamp_cached_readings_or_merge_configured_accounts() {
    let provider = fixture::provider("measured");
    let original = provider.fetch(fixture::now("measured")).unwrap().remove(0);
    let mut history = codexbar_store::HistoryStore::in_memory(chrono::Duration::days(30));
    assert_eq!(
        history
            .record(std::slice::from_ref(&original), fixture::now("measured"))
            .unwrap(),
        2
    );
    let cached = provider
        .fetch(fixture::now("measured") + chrono::Duration::minutes(3))
        .unwrap();
    assert_eq!(
        history
            .record(&cached, fixture::now("measured") + chrono::Duration::minutes(3))
            .unwrap(),
        0
    );
    let sample = history.series("muse-demo", "5-hour-window").next().unwrap();
    assert_eq!(sample.at().timestamp(), fixture::base().timestamp());
    assert_eq!(
        sample.resets_at(),
        original.metrics()[0]
            .resets_at()
            .map(|at| chrono::DateTime::from_timestamp(at.timestamp(), 0).unwrap())
    );
    let other = codexbar_providers::muse::parse(
        include_str!("../../codexbar-providers/fixtures/muse/measured.json"),
        fixture::now("measured"),
        "other-account",
        "Other",
    )
    .unwrap();
    assert_eq!(history.record(&[other], fixture::now("measured")).unwrap(), 2);
    assert_eq!(history.series("muse-demo", "weekly").count(), 1);
    assert_eq!(history.series("other-account", "weekly").count(), 1);
}

/// Inspect the actual Adaptive Card output rather than a producer-only percentage assertion.
fn bar_styles(value: &serde_json::Value, styles: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("type").and_then(serde_json::Value::as_str) == Some("Column")
                && map.get("minHeight").and_then(serde_json::Value::as_str) == Some("6px")
            {
                styles.push(map["style"].as_str().unwrap().to_owned());
            }
            for child in map.values() {
                bar_styles(child, styles);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                bar_styles(child, styles);
            }
        }
        _ => {}
    }
}

#[test]
fn exhausted_muse_reaches_actual_widget_reader_as_attention_while_unknown_usage_is_not_healthy() {
    use codexbar_store::widgets::{TileMode, WidgetBuilder, WidgetTile};
    for (state, expected, status) in [
        ("measured", Some("good"), None),
        ("exhausted", Some("attention"), Some("Limit reached")),
        ("stale", Some("default"), Some("Stale")),
        ("null", None, None),
    ] {
        let now = fixture::now(state);
        let mut states = States::new();
        let account = match fixture::provider(state).fetch(now) {
            Ok(mut accounts) => accounts.remove(0),
            Err(error) => {
                states.insert("muse-demo".into(), AccountState::Unavailable(error.to_string()));
                AccountSnapshot::new(AccountId::new("muse-demo"), Provider::MuseCode, Vec::new(), now)
            }
        };
        let dir = Scratch::new();
        snapshots::save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        let accounts = if state == "null" {
            vec![account]
        } else {
            snapshots::load_snapshots(&dir.0)
        };
        let snapshot = widget_feed::build(&accounts, &states, &Layout::default(), now).with_builder(WidgetBuilder {
            tiles: vec![WidgetTile {
                account: "muse-demo".into(),
                metric: "weekly".into(),
                mode: TileMode::Status,
            }],
            ..Default::default()
        });
        widgets::save_widget_snapshot(&dir.0, &snapshot).unwrap();
        let snapshot = widgets::load_widget_snapshot(&dir.0).unwrap();
        let config = Config {
            focus: cards::Focus::Custom,
            ..Default::default()
        };
        let views = cards::tile_views(&snapshot, &config, Size::Medium, now).unwrap();
        assert_eq!(views[0].status.as_deref(), status, "{state}");
        if state != "measured" {
            assert_ne!(views[0].big.as_deref(), Some("OK"));
        }
        // A bar-mode render checks the filled segment's semantic color, status mode checks the large label.
        let mut bar_snapshot = snapshot.clone();
        bar_snapshot.builder.tiles[0].mode = TileMode::Bar;
        let mut styles = Vec::new();
        bar_styles(
            &cards::render(&Ok(bar_snapshot), &config, Size::Medium, now),
            &mut styles,
        );
        assert_eq!(styles.first().map(String::as_str), expected, "{state}: {styles:?}");
        let status_card = cards::render(&Ok(snapshot), &config, Size::Medium, now).to_string();
        if state == "exhausted" {
            assert!(status_card.contains("Attention"));
            assert!(status_card.contains("Limit reached"));
            assert!(!styles.iter().any(|style| style == "good"));
        }
        if matches!(state, "stale" | "null") {
            assert!(!styles.iter().any(|style| style == "good"));
            assert!(!status_card.contains("\"Good\""));
        }
    }
}

#[test]
fn exhausted_widget_status_respects_informational_pools_and_credit_coverage() {
    let window = |label: &str| Metric::Window {
        label: label.into(),
        used: 1.0,
        resets_at: fixture::base() + chrono::Duration::days(2),
        pace: None,
        length: Some(chrono::Duration::days(7)),
    };
    let argon = AccountSnapshot::new(
        AccountId::new("argon"),
        Provider::Gemini,
        vec![window("Argon weekly")],
        fixture::base(),
    );
    let covered = AccountSnapshot::new(
        AccountId::new("covered"),
        Provider::Codex,
        vec![
            window("Weekly"),
            Metric::Credits {
                label: "Credits".into(),
                balance: Some(12.0),
                start: None,
            },
        ],
        fixture::base(),
    );
    let feed = widget_feed::build(&[argon, covered], &States::new(), &Layout::default(), fixture::base());
    for account in &feed.accounts {
        assert_eq!(account.metrics[0].status, None);
    }
    let card = cards::render(&Ok(feed), &Config::default(), Size::Medium, fixture::base());
    let mut styles = Vec::new();
    bar_styles(&card, &mut styles);
    assert_eq!(styles, vec!["good", "good"]);
    assert!(!card.to_string().contains("Attention"));
}

#[test]
fn ended_muse_window_cannot_forecast_or_alert_while_its_weekly_sibling_remains_current() {
    use codexbar_core::{Severity, alerts};
    let before = fixture::base() + chrono::Duration::minutes(1);
    let after = fixture::base() + chrono::Duration::minutes(3);
    let account = fixture::provider("reset-before").fetch(before).unwrap().remove(0);
    let dir = Scratch::new();
    snapshots::save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
    let account = snapshots::load_snapshots(&dir.0).remove(0);
    assert_eq!(account.observed_at(), Some(fixture::base()));
    assert_eq!(account.fetched_at(), before);
    let short = &account.metrics()[0];
    let weekly = &account.metrics()[1];
    assert!(!account.observation_stale(after));
    assert_eq!(
        crate::account_table::row_metric_at(&account, before).unwrap().key(),
        "5-hour-window"
    );
    assert_eq!(
        crate::account_table::row_metric_at(&account, after).unwrap().key(),
        "weekly"
    );
    assert!(short.resets_at().unwrap() <= after);
    assert!(account.evaluation_time(short, short.resets_at().unwrap()).is_none());
    assert!(crate::usage_bar::Forecast::for_account(short, &account, after).is_none());
    assert!(crate::usage_bar::outlook(short, &account, before).is_some());
    assert!(
        crate::usage_bar::outlook(short, &account, after).is_none(),
        "ended short window must not retain its observation-anchored outlook"
    );
    assert!(crate::usage_bar::outlook(weekly, &account, after).is_some());
    let projection = crate::usage_bar::Projection::of_account(short, short.used_fraction().unwrap(), &account, after);
    assert_eq!(projection.projected, None);
    assert_eq!(account.assess_metric(short, after).severity(), Severity::Normal);
    assert_eq!(
        account.assess(after).severity(),
        account.assess_metric(weekly, after).severity()
    );
    let settings = alerts::AlertSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(
        !alerts::evaluate(&settings, &Default::default(), std::slice::from_ref(&account), before)
            .notify
            .is_empty()
    );
    assert!(
        alerts::evaluate(&settings, &Default::default(), std::slice::from_ref(&account), after)
            .notify
            .is_empty()
    );
    assert!(alerts::account_alerts(&settings, &Default::default(), &account, after).is_empty());
    // Reconciling old-window keys remains independent of whether the ended current window may alert.
    let previous_slot = format!(
        "{}@{}",
        short.key(),
        (short.resets_at().unwrap() - chrono::Duration::hours(5)).timestamp() / 3600
    );
    let old_key = alerts::alert_key(account.id().as_str(), &previous_slot, alerts::AlertKind::Usage);
    let active = [old_key.clone()].into_iter().collect();
    let reconciled = alerts::evaluate(&settings, &active, std::slice::from_ref(&account), after);
    assert!(reconciled.recovered.contains(&old_key));
    assert!(reconciled.notify.is_empty());
    let stale =
        account
            .clone()
            .with_observation(fixture::base() - chrono::Duration::hours(1), Some("mint".into()), 420);
    let held = alerts::evaluate(&settings, &active, &[stale], after);
    assert!(held.notify.is_empty() && held.recovered.is_empty());
    let feed = widget_feed::build(&[account], &States::new(), &Layout::default(), after);
    assert_eq!(feed.accounts[0].metrics[0].projected_percent, None);
    assert_eq!(feed.accounts[0].metrics[0].outlook, None);
    assert!(feed.accounts[0].metrics[1].projected_percent.is_some());
}
