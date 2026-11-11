//! Malformed percentages must not become confident zero-usage readings (#331).

use chrono::{DateTime, Utc};
use codexbar_core::AccountSnapshot;
use codexbar_providers::{claude, codex, cursor};
use serde_json::{Value, json};

fn now() -> DateTime<Utc> {
    "2026-11-11T13:00:00Z".parse().unwrap()
}

fn codex_window(percent: f64, seconds: i64) -> Value {
    json!({"used_percent": percent, "reset_at": 1794416400, "limit_window_seconds": seconds})
}

fn claude_window(percent: f64) -> Value {
    json!({"utilization": percent, "resets_at": "2026-11-11T18:00:00Z"})
}

fn claude_limit(percent: f64, kind: &str) -> Value {
    json!({"percent": percent, "kind": kind, "resets_at": "2026-11-11T18:00:00Z"})
}

fn fractions(account: &AccountSnapshot) -> Vec<(&str, Option<f64>)> {
    account
        .metrics()
        .iter()
        .map(|metric| (metric.label(), metric.used_fraction()))
        .collect()
}

#[test]
fn codex_rejects_negative_only_usage() {
    for percent in [-25.0, -0.1] {
        let payload = json!({"rate_limit": {"primary_window": codex_window(percent, 18000)}});
        assert!(codex::parse_usage(&payload.to_string(), now()).is_err(), "{payload}");
    }
}

#[test]
fn codex_keeps_valid_sibling_of_negative_window() {
    let payload = json!({"rate_limit": {
        "primary_window": codex_window(-25.0, 18000),
        "secondary_window": codex_window(40.0, 604800)
    }});
    let account = codex::parse_usage(&payload.to_string(), now()).unwrap();
    assert_eq!(fractions(&account), vec![("Weekly", Some(0.4))]);
}

#[test]
fn claude_rejects_negative_only_flat_and_structured_usage() {
    for percent in [-25.0, -0.1] {
        for payload in [
            json!({"five_hour": claude_window(percent)}),
            json!({"limits": [claude_limit(percent, "session")]}),
        ] {
            assert!(
                claude::parse_usage(&payload.to_string(), None, now()).is_err(),
                "{payload}"
            );
        }
    }
}

#[test]
fn claude_keeps_valid_siblings_of_negative_flat_and_structured_windows() {
    for payload in [
        json!({"five_hour": claude_window(-25.0), "seven_day": claude_window(40.0)}),
        json!({"limits": [claude_limit(-25.0, "session"), claude_limit(40.0, "weekly_all")]}),
    ] {
        let account = claude::parse_usage(&payload.to_string(), None, now()).unwrap();
        assert_eq!(fractions(&account), vec![("Weekly", Some(0.4))]);
        assert!(
            account
                .messages()
                .iter()
                .any(|message| message.contains("5-hour limit unavailable"))
        );
    }
}

#[test]
fn claude_negative_structured_window_does_not_suppress_valid_flat_fallback() {
    let payload = json!({
        "limits": [claude_limit(-25.0, "session")],
        "five_hour": claude_window(40.0)
    });
    let account = claude::parse_usage(&payload.to_string(), None, now()).unwrap();
    assert_eq!(fractions(&account), vec![("5-hour window", Some(0.4))]);
    assert!(account.messages().is_empty());
}

#[test]
fn claude_negative_flat_weekly_does_not_suppress_valid_oauth_apps_fallback() {
    let payload = json!({
        "seven_day": claude_window(-25.0),
        "seven_day_oauth_apps": claude_window(40.0)
    });
    let account = claude::parse_usage(&payload.to_string(), None, now()).unwrap();
    assert_eq!(fractions(&account), vec![("Weekly", Some(0.4))]);
}

#[test]
fn cursor_rejects_negative_only_usage_in_each_plan_field() {
    for key in ["totalPercentUsed", "autoPercentUsed", "apiPercentUsed"] {
        for percent in [-25.0, -0.1] {
            let payload = json!({"billingCycleEnd": 1796083200000_i64, "planUsage": {key: percent}});
            assert!(cursor::parse_usage(&payload.to_string(), now()).is_err(), "{payload}");
        }
    }
}

#[test]
fn cursor_keeps_valid_siblings_of_negative_percentage() {
    let payload = json!({"billingCycleEnd": 1796083200000_i64, "planUsage": {
        "totalPercentUsed": -25, "autoPercentUsed": 0, "apiPercentUsed": 40
    }});
    let account = cursor::parse_usage(&payload.to_string(), now()).unwrap();
    assert_eq!(fractions(&account), vec![("Auto", Some(0.0)), ("API", Some(0.4))]);
}

#[test]
fn zero_normal_and_over_limit_percentages_keep_existing_behavior() {
    for (percent, expected) in [(0.0, 0.0), (40.0, 0.4), (100.0, 1.0), (125.0, 1.0)] {
        let codex_payload = json!({"rate_limit": {"primary_window": codex_window(percent, 18000)}});
        let flat_payload = json!({"five_hour": claude_window(percent)});
        let structured_payload = json!({"limits": [claude_limit(percent, "session")]});
        let cursor_payload = json!({"billingCycleEnd": 1796083200000_i64, "planUsage": {
            "totalPercentUsed": percent, "autoPercentUsed": percent, "apiPercentUsed": percent
        }});
        for account in [
            codex::parse_usage(&codex_payload.to_string(), now()).unwrap(),
            claude::parse_usage(&flat_payload.to_string(), None, now()).unwrap(),
            claude::parse_usage(&structured_payload.to_string(), None, now()).unwrap(),
            cursor::parse_usage(&cursor_payload.to_string(), now()).unwrap(),
        ] {
            assert!(
                account
                    .metrics()
                    .iter()
                    .all(|metric| metric.used_fraction() == Some(expected))
            );
        }
    }
}
