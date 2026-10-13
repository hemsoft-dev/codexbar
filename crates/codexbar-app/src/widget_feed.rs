//! What the dashboard tells the Windows widgets (#94): its accounts in table order, turned into the display-ready
//! `widgets.json` snapshot that the widget provider reads. Nothing here touches credentials.

use chrono::{DateTime, Utc};
use codexbar_core::layout::Layout;
use codexbar_core::{AccountSnapshot, Metric};
use codexbar_store::widgets::{WidgetAccount, WidgetGroup, WidgetHealth, WidgetMetric, WidgetSnapshot};

use crate::account_table::{AccountState, States};

/// The longest the dashboard goes without rewriting the snapshot while it runs.
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
            let has_usage = health != WidgetHealth::Unavailable;
            WidgetAccount {
                id: id.to_owned(),
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
                    account.metrics().iter().map(|m| metric(m, now)).collect()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    WidgetSnapshot::new(now, groups, accounts)
}

fn metric(metric: &Metric, now: DateTime<Utc>) -> WidgetMetric {
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
        status: codexbar_core::assess(metric, now).label().map(str::to_owned),
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
