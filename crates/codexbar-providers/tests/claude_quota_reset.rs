//! Reset validity through the production Claude parser and the dashboard's assessment/alert consumers.
use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::alerts::{self, AlertSettings};
use codexbar_core::{AccountSnapshot, Provider, Severity};
use codexbar_providers::claude::parse_usage;
use serde_json::json;

fn reset() -> DateTime<Utc> {
    "2026-11-03T12:00:00Z".parse().unwrap()
}

fn settings() -> AlertSettings {
    AlertSettings {
        enabled: true,
        ..Default::default()
    }
}

fn snapshot(
    session: serde_json::Value,
    weekly_used: u32,
    weekly_reset: DateTime<Utc>,
    at: DateTime<Utc>,
) -> AccountSnapshot {
    parse_usage(
        &json!({"five_hour":session,"seven_day":{"utilization":weekly_used,"resets_at":weekly_reset.to_rfc3339()}})
            .to_string(),
        None,
        at,
    )
    .unwrap()
    .with_plan("Ultra")
}

fn session(used: u32, reset: DateTime<Utc>) -> serde_json::Value {
    json!({"utilization":used,"resets_at":reset.to_rfc3339()})
}

fn keys(account: &AccountSnapshot, at: DateTime<Utc>) -> BTreeSet<String> {
    alerts::evaluate(&settings(), &BTreeSet::new(), std::slice::from_ref(account), at)
        .notify
        .iter()
        .flat_map(|alert| alert.keys().cloned())
        .collect()
}

#[test]
fn claude_quota_reset_cached_and_new_snapshots_do_not_alert_on_ended_session() {
    let cached = snapshot(
        session(100, reset()),
        5,
        reset() + Duration::days(6),
        reset() - Duration::seconds(1),
    );
    let observed = cached
        .clone()
        .with_observation(reset() - Duration::seconds(1), None, 3600);
    assert!(
        !keys(&cached, reset() - Duration::seconds(1)).is_empty(),
        "live exhausted session still alerts"
    );
    for seconds in [0, 1, 600] {
        let wall = reset() + Duration::seconds(seconds);
        let fresh = snapshot(session(100, reset()), 5, reset() + Duration::days(6), wall);
        for account in [&cached, &fresh, &observed] {
            let metric = &account.metrics()[0];
            assert_eq!(metric.used_fraction(), Some(1.0), "retain reported data");
            assert_eq!(metric.resets_at(), Some(reset()), "never invent a reset");
            assert!(
                account.evaluation_time(metric, wall).is_none(),
                "reset+{seconds}s must be unknown"
            );
            assert!(!account.assess_metric(metric, wall).is_exhausted());
            assert_eq!(
                account.assess(wall).severity(),
                Severity::Normal,
                "only the live healthy weekly remains"
            );
            assert!(keys(account, wall).is_empty(), "reset+{seconds}s");
            assert!(alerts::account_alerts(&settings(), &BTreeSet::new(), account, wall).is_empty());
        }
    }
}

#[test]
fn claude_quota_reset_live_weekly_sibling_still_alerts_and_ranks() {
    for seconds in [0, 1, 600] {
        let wall = reset() + Duration::seconds(seconds);
        let account = snapshot(
            session(100, reset()),
            98,
            reset() + Duration::days(6),
            reset() - Duration::seconds(1),
        );
        let out = alerts::evaluate(&settings(), &BTreeSet::new(), std::slice::from_ref(&account), wall);
        assert!(!out.notify.is_empty());
        assert!(out.notify.iter().all(|alert| alert.metric == "Weekly"));
        assert_eq!(account.assess(wall).severity(), Severity::LimitSoon);
        let details = alerts::account_alerts(&settings(), &BTreeSet::new(), &account, wall);
        assert!(!details.is_empty());
        assert!(details.iter().all(|detail| detail.metric == "Weekly"));

        let live_session = snapshot(session(100, reset() + Duration::hours(4)), 100, reset(), wall);
        let out = alerts::evaluate(&settings(), &BTreeSet::new(), std::slice::from_ref(&live_session), wall);
        assert!(!out.notify.is_empty());
        assert!(out.notify.iter().all(|alert| alert.metric == "5-hour window"));
        assert_eq!(live_session.assess(wall).severity(), Severity::LimitSoon);
    }
}

#[test]
fn claude_quota_reset_expired_weekly_is_unknown_without_fabricating_allowance() {
    for seconds in [0, 1, 600] {
        let wall = reset() + Duration::seconds(seconds);
        for parsed_at in [reset() - Duration::seconds(1), wall] {
            let account = snapshot(serde_json::Value::Null, 100, reset(), parsed_at);
            assert_eq!(account.metrics().len(), 1);
            let weekly = &account.metrics()[0];
            assert_eq!(weekly.used_fraction(), Some(1.0));
            assert_eq!(weekly.resets_at(), Some(reset()));
            assert!(account.evaluation_time(weekly, wall).is_none());
            assert!(
                account
                    .status_metrics()
                    .all(|metric| account.evaluation_time(metric, wall).is_none())
            );
            assert!(!account.assess(wall).is_exhausted());
            assert!(keys(&account, wall).is_empty());
            assert!(account.messages().iter().any(|message| message.contains("unknown")));
        }
    }
}

#[test]
fn claude_quota_reset_holds_keys_through_unknown_and_failure_then_recovers_on_new_window() {
    let before = reset() - Duration::seconds(1);
    let cached = snapshot(session(100, reset()), 5, reset() + Duration::days(6), before);
    let active = keys(&cached, before);
    assert!(!active.is_empty());
    let wall = reset() + Duration::seconds(1);
    for account in [
        cached,
        snapshot(serde_json::Value::Null, 5, reset() + Duration::days(6), wall),
    ] {
        let out = alerts::evaluate(&settings(), &active, &[account], wall);
        assert!(out.notify.is_empty());
        assert!(out.recovered.is_empty(), "unknown usage does not prove recovery");
    }
    let failed = alerts::evaluate(&settings(), &active, &[], wall);
    assert!(failed.notify.is_empty() && failed.recovered.is_empty());
    let new_reset = reset() + Duration::hours(5);
    let renewed = snapshot(session(0, new_reset), 5, reset() + Duration::days(6), wall);
    let out = alerts::evaluate(&settings(), &active, &[renewed], wall);
    assert!(out.notify.is_empty());
    assert_eq!(out.recovered.into_iter().collect::<BTreeSet<_>>(), active);
    let used = snapshot(session(100, new_reset), 5, reset() + Duration::days(6), wall);
    let new_keys = keys(&used, wall);
    assert!(!new_keys.is_empty());
    assert!(new_keys.is_disjoint(&active), "genuine new window can alert again");
}

#[test]
fn claude_quota_reset_unavailable_session_remains_unknown_then_restores_finite_data() {
    for missing in [serde_json::Value::Null, json!({"utilization":100,"resets_at":null})] {
        let weekly = snapshot(missing, 5, reset() + Duration::days(6), reset());
        assert_eq!(weekly.metrics().len(), 1);
        assert_eq!(weekly.metrics()[0].key(), "weekly");
        assert!(weekly.messages().iter().any(|message| message.contains("unknown")));
        assert!(keys(&weekly, reset()).is_empty());
    }
    let restored = snapshot(
        session(100, reset() + Duration::hours(5)),
        5,
        reset() + Duration::days(6),
        reset(),
    );
    assert_eq!(restored.metrics().len(), 2);
    assert!(restored.messages().is_empty());
    assert!(!keys(&restored, reset()).is_empty());
}

#[test]
fn claude_quota_reset_does_not_change_other_provider_validity() {
    let claude = snapshot(session(100, reset()), 5, reset() + Duration::days(6), reset());
    let codex = AccountSnapshot::new(claude.id().clone(), Provider::Codex, claude.metrics().to_vec(), reset());
    assert!(codex.evaluation_time(&codex.metrics()[0], reset()).is_some());
}
