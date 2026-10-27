//! What the dashboard tells the Windows widgets (#94): its accounts in table order, turned into the display-ready
//! `widgets.json` snapshot that the widget provider reads. Nothing here touches credentials.

use chrono::{DateTime, Utc};
use codexbar_core::layout::Layout;
use codexbar_core::{AccountSnapshot, Metric};
use codexbar_store::widgets::{WidgetAccount, WidgetGroup, WidgetHealth, WidgetMetric, WidgetSnapshot};

use crate::account_table::{AccountState, States};

/// The longest the dashboard goes without rewriting the snapshot while it runs.
#[cfg(windows)]
pub const HEARTBEAT: chrono::Duration = chrono::Duration::minutes(5);

/// The snapshot for `accounts`, which arrive arranged as the table shows them.
pub fn build(accounts: &[AccountSnapshot], states: &States, layout: &Layout, now: DateTime<Utc>) -> WidgetSnapshot {
    let groups = layout
        .groups()
        .iter()
        .map(|group| WidgetGroup {
            id: group.id.clone(),
            name: group.name.clone(),
        })
        .collect();
    let accounts = accounts
        .iter()
        .map(|account| {
            let id = account.id().as_str();
            let health = match states.get(id) {
                None | Some(AccountState::Fresh) => WidgetHealth::Fresh,
                Some(AccountState::Restored | AccountState::Failed(_)) => WidgetHealth::Stale,
                Some(AccountState::Loading | AccountState::Unavailable(_)) => WidgetHealth::Unavailable,
            };
            let health = if account.provider() == codexbar_core::Provider::MuseCode && account.metrics().is_empty() {
                WidgetHealth::Unavailable
            } else if health == WidgetHealth::Fresh && account.observation_stale(now) {
                WidgetHealth::Stale
            } else {
                health
            };
            let has_usage = health != WidgetHealth::Unavailable;
            WidgetAccount {
                id: id.to_owned(),
                observed_at: account.observed_at(),
                allowance_source: account.allowance_source().map(str::to_owned),
                fresh_for_seconds: account.fresh_for_seconds(),
                provider: account.provider().key().to_owned(),
                provider_name: account.provider().display_name().to_owned(),
                name: account.display_name(),
                group: layout.group_of(id).map(|group| group.id.clone()),
                health,
                updated_at: has_usage.then(|| account.fetched_at()),
                status: has_usage
                    .then(|| account.assess(now).label().map(str::to_owned))
                    .flatten(),
                metrics: if has_usage {
                    account.metrics().iter().map(|m| metric(account, m, now)).collect()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    WidgetSnapshot::new(now, groups, accounts)
}

/// The feed's status vocabulary differs from dashboard pills for exhausted quotas, which have no pill.
fn metric_status(account: &AccountSnapshot, metric: &Metric, now: DateTime<Utc>) -> Option<String> {
    let assessment = account.assess_metric(metric, now);
    // Exhaustion deliberately has no dashboard pill. Widgets still need an explicit status so their
    // normal-color fallback cannot advertise an exhausted, uncovered quota as healthy.
    if account.is_status_metric(metric) && !account.credits_cover(metric) && assessment.is_exhausted() {
        Some("Limit reached".to_owned())
    } else {
        assessment.label().map(str::to_owned)
    }
}

fn metric(account: &AccountSnapshot, metric: &Metric, now: DateTime<Utc>) -> WidgetMetric {
    let value = match metric {
        Metric::Window { .. } => format!("{} used", metric.used_display()),
        Metric::Quota { limit, .. } => format!(
            "{} of {}",
            metric.used_display(),
            codexbar_core::group_thousands(*limit)
        ),
        Metric::Spend { .. } | Metric::Balance { .. } => metric.used_display(),
        // A count, never a percentage (#150).
        Metric::Credits {
            balance: Some(balance), ..
        } => format!("{} credits left", codexbar_core::format_credits(*balance)),
        Metric::Credits { balance: None, .. } => "Unlimited credits".to_owned(),
        // A count with no limit (#103): "12 completed reviews".
        Metric::Activity { label, .. } => format!("{} {}", metric.used_display(), label.to_lowercase()),
    };
    WidgetMetric {
        key: metric.key(),
        label: metric.label().to_owned(),
        value,
        used_percent: metric.used_fraction().map(|used| (used * 100.0).clamp(0.0, 100.0)),
        resets_at: metric.resets_at(),
        status: metric_status(account, metric, now),
        // Past 100 when the limit runs out first (#197); the tile says the figure, never a bar beyond its end.
        projected_percent: account
            .evaluation_time(metric, now)
            .and_then(|at| metric.projected_at_reset(at))
            .map(|share| (share * 100.0).max(0.0)),
        // The share at the reset and when the limit runs out (#234), worded as the dashboard's bars word it.
        outlook: crate::usage_bar::outlook(metric, account, now),
        sus: codexbar_core::sus::weekly(metric, now)
            .filter(|_| account.sus_eligible(metric, now))
            .map(|s| s.value()),
        sus_early: codexbar_core::sus::early(metric, now)
            .filter(|_| account.sus_eligible(metric, now))
            .map(|s| s.value()),
        sus_eligible: codexbar_core::sus::text(metric, now)
            .filter(|_| account.sus_eligible(metric, now))
            .is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar_core::layout::Group;
    use codexbar_core::{AccountId, Money, Provider};
    use std::collections::BTreeMap;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn accounts() -> Vec<AccountSnapshot> {
        let fetched = now() - chrono::Duration::minutes(3);
        vec![
            AccountSnapshot::new(
                AccountId::new("claude-1"),
                Provider::Claude,
                vec![Metric::Window {
                    label: "Weekly".into(),
                    used: 0.42,
                    resets_at: now() + chrono::Duration::days(2),
                    pace: None,
                    length: None,
                }],
                fetched,
            )
            .with_label("Work"),
            AccountSnapshot::new(
                AccountId::new("copilot-me"),
                Provider::Copilot,
                vec![Metric::Quota {
                    label: "Premium requests".into(),
                    used: 1284,
                    limit: 1500,
                    resets_at: now() + chrono::Duration::days(20),
                    pace: None,
                    length: None,
                }],
                fetched,
            ),
            AccountSnapshot::new(
                AccountId::new("or-1"),
                Provider::OpenRouter,
                vec![Metric::Balance {
                    label: "Credits".into(),
                    remaining: Money::from_cents(1230),
                    burn_per_day: None,
                }],
                fetched,
            ),
        ]
    }

    fn layout() -> Layout {
        Layout::new(
            vec![Group {
                id: "g1".into(),
                name: "Work".into(),
            }],
            BTreeMap::from([("claude-1".to_owned(), "g1".to_owned())]),
            Vec::new(),
        )
    }

    #[test]
    fn weekly_scores_reach_widgets_and_turns_never_become_percentages() {
        let accounts = crate::sus::fixture(now(), 0);
        let snapshot = build(&accounts, &States::new(), &Layout::default(), now());
        let codex = snapshot.accounts.iter().find(|a| a.id == "codex-personal").unwrap();
        assert_eq!(codex.metrics[0].sus, Some(48));
        let claude = snapshot.accounts.iter().find(|a| a.id == "claude-personal").unwrap();
        assert!(!claude.metrics[0].sus_eligible);
        assert_eq!(claude.metrics[1].sus, Some(76));
        let x = snapshot.accounts.iter().find(|a| a.id == "x-subscription").unwrap();
        assert_eq!(x.metrics[0].used_percent, None);
        assert!(x.metrics[0].sus_eligible);
        assert_eq!(x.metrics[0].sus, None);
    }

    #[test]
    fn an_early_week_carries_its_early_score_apart_from_the_firm_one() {
        let early = AccountSnapshot::new(
            AccountId::new("claude-early"),
            Provider::Claude,
            vec![Metric::Window {
                label: "Weekly".into(),
                used: 0.08,
                resets_at: now() + chrono::Duration::hours(162),
                pace: None,
                length: Some(chrono::Duration::days(7)),
            }],
            now(),
        );
        let snapshot = build(&[early], &States::new(), &Layout::default(), now());
        let metric = &snapshot.accounts[0].metrics[0];
        assert_eq!(
            (metric.sus, metric.sus_early, metric.sus_eligible),
            (None, Some(45), true)
        );
        assert_eq!(metric.projected_percent, None);
    }

    #[test]
    fn a_paced_window_carries_its_projection_and_an_unpaced_one_none() {
        // 1%/h with 48h left on top of 42%: 90% at the reset (#197), near the limit (#234).
        let paced = AccountSnapshot::new(
            AccountId::new("claude-2"),
            Provider::Claude,
            vec![Metric::Window {
                label: "Weekly".into(),
                used: 0.42,
                resets_at: now() + chrono::Duration::days(2),
                pace: Some(codexbar_core::Pace::per_hour(0.01)),
                length: None,
            }],
            now(),
        );
        let snapshot = build(&[paced, accounts().remove(0)], &States::new(), &layout(), now());
        assert_eq!(
            snapshot.accounts[0].metrics[0].projected_percent.map(f64::round),
            Some(90.0)
        );
        assert_eq!(
            snapshot.accounts[0].metrics[0].outlook.as_deref(),
            Some("90% at reset · near limit")
        );
        assert_eq!(
            snapshot.accounts[1].metrics[0].projected_percent, None,
            "too young to project"
        );
        assert_eq!(snapshot.accounts[1].metrics[0].outlook, None);
    }

    /// A used-up window the account's credits carry says so, and one with no pace left says the limit is reached
    /// (#234), as the dashboard does.
    #[test]
    fn a_used_up_window_says_covered_or_reached() {
        let five_hour = |pace| Metric::Window {
            label: "5-hour window".into(),
            used: 1.0,
            resets_at: now() + chrono::Duration::hours(2),
            pace,
            length: None,
        };
        let covered = AccountSnapshot::new(
            AccountId::new("codex-covered"),
            Provider::Codex,
            vec![
                five_hour(Some(codexbar_core::Pace::per_hour(0.2))),
                Metric::Credits {
                    label: "Credits".into(),
                    balance: Some(12.0),
                    start: None,
                },
            ],
            now(),
        );
        let reached = AccountSnapshot::new(
            AccountId::new("codex-out"),
            Provider::Codex,
            vec![five_hour(None)],
            now(),
        );
        let snapshot = build(&[covered, reached], &States::new(), &layout(), now());
        assert_eq!(
            snapshot.accounts[0].metrics[0].outlook.as_deref(),
            Some("covered by credits")
        );
        assert_eq!(
            snapshot.accounts[1].metrics[0].outlook.as_deref(),
            Some("limit reached")
        );
    }

    #[test]
    fn accounts_keep_table_order_groups_and_display_values() {
        let snapshot = build(&accounts(), &States::new(), &layout(), now());
        let ids: Vec<&str> = snapshot.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["claude-1", "copilot-me", "or-1"]);
        assert_eq!(
            snapshot.groups,
            vec![WidgetGroup {
                id: "g1".into(),
                name: "Work".into()
            }]
        );
        let claude = &snapshot.accounts[0];
        assert_eq!(claude.name, "Claude · Work");
        assert_eq!(claude.group.as_deref(), Some("g1"));
        assert_eq!(claude.metrics[0].value, "42% used");
        assert_eq!(claude.metrics[0].used_percent, Some(42.0));
        assert_eq!(snapshot.accounts[1].metrics[0].value, "1,284 of 1,500");
        assert_eq!(snapshot.accounts[1].group, None);
        let balance = &snapshot.accounts[2].metrics[0];
        assert_eq!(balance.value, "$12.30 left");
        assert_eq!(balance.used_percent, None, "a balance has no limit to fill");
        assert_eq!(snapshot.generated_at, now());
    }

    #[test]
    fn credits_are_a_count_and_a_used_up_window_has_no_status() {
        let account = |id: &str, balance| {
            AccountSnapshot::new(
                AccountId::new(id),
                Provider::Codex,
                vec![
                    Metric::Window {
                        label: "Weekly".into(),
                        used: 1.0,
                        resets_at: now() + chrono::Duration::days(2),
                        pace: None,
                        length: None,
                    },
                    Metric::Credits {
                        label: "Credits".into(),
                        balance,
                        start: Some(50_000.0),
                    },
                ],
                now(),
            )
            .with_credits_placed()
        };
        let accounts = [account("on-credits", Some(18_400.0)), account("unlimited", None)];
        let snapshot = build(&accounts, &States::new(), &Layout::default(), now());
        let credits = &snapshot.accounts[0].metrics[0];
        assert_eq!(credits.value, "18,400 credits left");
        assert_eq!(credits.used_percent, None, "never a percentage");
        assert_eq!(snapshot.accounts[0].status, None);
        assert_eq!(snapshot.accounts[1].metrics[0].value, "Unlimited credits");
        let weekly = &snapshot.accounts[0].metrics[1];
        assert_eq!(weekly.used_percent, Some(100.0));
        assert_eq!(weekly.status, None, "no pill on a used-up window");
    }

    #[test]
    fn an_activity_count_names_what_it_counts() {
        let account = AccountSnapshot::new(
            AccountId::new("gr-1"),
            Provider::Greptile,
            vec![Metric::Activity {
                label: "Completed reviews".into(),
                count: 37,
            }],
            now(),
        );
        let snapshot = build(&[account], &States::new(), &Layout::default(), now());
        let reviews = &snapshot.accounts[0].metrics[0];
        assert_eq!(reviews.value, "37 completed reviews");
        assert_eq!(reviews.used_percent, None, "never a percentage");
        assert_eq!(reviews.resets_at, None);
        assert_eq!(reviews.status, None);
    }

    #[test]
    fn health_follows_the_refresh_state() {
        let states = States::from([
            ("claude-1".to_owned(), AccountState::Failed("offline".into())),
            ("copilot-me".to_owned(), AccountState::Unavailable("signed out".into())),
            ("or-1".to_owned(), AccountState::Restored),
        ]);
        let snapshot = build(&accounts(), &states, &layout(), now());
        let health: Vec<WidgetHealth> = snapshot.accounts.iter().map(|a| a.health).collect();
        assert_eq!(
            health,
            [WidgetHealth::Stale, WidgetHealth::Unavailable, WidgetHealth::Stale]
        );
        let unavailable = &snapshot.accounts[1];
        assert!(unavailable.metrics.is_empty() && unavailable.updated_at.is_none());
        assert!(
            snapshot.accounts[0].updated_at.is_some(),
            "last known usage keeps its time"
        );
    }

    #[test]
    fn error_messages_never_reach_the_snapshot() {
        // Provider errors can quote server responses; the widget only learns that usage is stale or missing.
        let states = States::from([(
            "claude-1".to_owned(),
            AccountState::Failed("401 bad token sk-123".into()),
        )]);
        let text = serde_json::to_string(&build(&accounts(), &states, &layout(), now())).unwrap();
        assert!(!text.contains("sk-123") && !text.contains("token"));
    }
}
