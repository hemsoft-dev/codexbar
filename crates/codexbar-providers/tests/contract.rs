//! Result-contract compatibility (#75): every provider's results identify their account and expose stable metric
//! keys. History, alerts, snapshots and preferences are keyed by these, so a change here is a breaking change and
//! must come with a migration.

use chrono::{DateTime, TimeZone, Utc};
use codexbar_core::{AccountSnapshot, Currency, Metric};
use codexbar_providers::{balance, claude, codex, copilot, cursor, opencode};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap()
}

fn keys(account: &AccountSnapshot) -> Vec<String> {
    account.metrics().iter().map(Metric::key).collect()
}

#[test]
fn codex_results_keep_their_id_and_metric_keys() {
    let account = codex::parse_usage(include_str!("fixtures/codex-usage.json"), now()).unwrap();
    assert_eq!(account.id().as_str(), "codex-chatgpt");
    assert_eq!(keys(&account), vec!["5-hour-window", "weekly"]);
    for key in keys(&account) {
        assert!(
            key.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
            "{key}"
        );
    }
}

#[test]
fn claude_results_keep_their_id_and_metric_keys() {
    let account = claude::parse_usage(include_str!("fixtures/claude-usage.json"), None, now()).unwrap();
    assert_eq!(account.id().as_str(), "claude");
    assert_eq!(keys(&account), vec!["5-hour-window", "weekly"]);
    // The structured payload adds model-scoped weekly windows and usage credits (#82).
    let current = claude::parse_usage(include_str!("fixtures/claude-usage-current.json"), None, now()).unwrap();
    assert_eq!(
        keys(&current),
        vec![
            "5-hour-window",
            "weekly",
            "weekly-fable",
            "extra-usage",
            "credit-balance"
        ]
    );
}

#[test]
fn copilot_results_are_identified_by_username() {
    let account = copilot::parse_user(include_str!("fixtures/copilot-user.json"), "HemSoft", now()).unwrap();
    assert_eq!(
        account.id(),
        &copilot::account_id("hemsoft"),
        "case-insensitive, like GitHub usernames"
    );
    assert_eq!(account.id().as_str(), "copilot-hemsoft");
    assert_eq!(keys(&account), vec!["premium-requests"]);
}

#[test]
fn cursor_results_keep_their_id_and_metric_keys() {
    let payload = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":71,"autoPercentUsed":40,"apiPercentUsed":12.5}}"#;
    let account = cursor::parse_usage(payload, now()).unwrap();
    // Parsed alone, the legacy id; the provider names the account after the signed-in identity (#81).
    assert_eq!(account.id().as_str(), cursor::LEGACY_ID);
    assert_eq!(keys(&account), vec!["included-usage", "auto", "api"]);
    let with_on_demand = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":71},
        "spendLimitUsage":{"individualLimit":2000,"individualRemaining":1500}}"#;
    let account = cursor::parse_usage(with_on_demand, now()).unwrap();
    assert_eq!(keys(&account), vec!["included-usage", "on-demand"]);
}

#[test]
fn balance_results_are_dollars_with_stable_keys() {
    let openrouter = balance::parse_openrouter(r#"{"data":{"total_credits":20,"total_usage":1.58}}"#, now()).unwrap();
    assert_eq!(openrouter.id().as_str(), "openrouter");
    assert_eq!(keys(&openrouter), vec!["credits"]);
    let moonshot = balance::parse_moonshot(r#"{"data":{"available_balance":7.85}}"#, now()).unwrap();
    assert_eq!(moonshot.id().as_str(), "moonshot");
    assert_eq!(keys(&moonshot), vec!["balance"]);
    for account in [&openrouter, &moonshot] {
        let Some(Metric::Balance { remaining, .. }) = account.primary() else {
            panic!("a balance")
        };
        assert_eq!(
            remaining.currency(),
            Currency::Usd,
            "api.moonshot.ai and OpenRouter bill in dollars"
        );
    }
}

#[test]
fn opencode_metrics_keep_their_keys() {
    let go = r#"<script>_$HY.r["go"]=$R[0]={rollingUsage:$R[1]={usagePercent:38,resetInSec:3600,limit:1},weeklyUsage:$R[2]={resetInSec:432000,usagePercent:12},monthlyUsage:$R[3]={usagePercent:5,resetInSec:1728000}}</script>"#;
    let metrics = opencode::parse_go(go, now());
    let keys: Vec<String> = metrics.iter().map(Metric::key).collect();
    assert_eq!(keys, vec!["5-hour-window", "weekly", "monthly"]);
    let zen = opencode::parse_zen(r#"<script>$R[4]={id:"wrk",balance:1260000000,currency:"usd"}</script>"#).unwrap();
    assert_eq!(zen.currency(), Currency::Usd);
}
