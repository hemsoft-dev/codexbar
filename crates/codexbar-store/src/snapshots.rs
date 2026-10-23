//! Last-good account snapshots (#76), kept in `snapshots.json` next to the settings so the dashboard shows every
//! account at startup before the first fetch returns, and keeps usage visible while a provider is failing.
//!
//! Only usage is stored: account ids, provider keys, labels, reported plans, metric names, numbers and times. Never keys, tokens or
//! cookies. Written to a temp file and swapped in; an unreadable or foreign file is ignored, never misread.

use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Currency, Metric, Money, Pace, Provider};
use serde_json::{Value, json};

use crate::settings::{AccountRecord, Settings};

pub const SNAPSHOTS_FILE: &str = "snapshots.json";
/// Version 3 adds reported plans. Version 2 added currencies, spend and messages.
/// Version 1 files read as US dollars with no messages.
const VERSION: u64 = 3;

/// Loads the last-good snapshots in `dir`. Missing, unreadable or newer files give none; entries that can't be read
/// are skipped.
pub fn load_snapshots(dir: &Path) -> Vec<AccountSnapshot> {
    let Ok(text) = std::fs::read_to_string(dir.join(SNAPSHOTS_FILE)) else {
        return Vec::new();
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(version) = doc
        .get("version")
        .and_then(Value::as_u64)
        .filter(|v| (1..=VERSION).contains(v))
    else {
        return Vec::new();
    };
    // A configured name can itself be "Pro" or end in " · Pro". Keep it a name.
    let settings = (version < 3).then(|| Settings::load_or_default(dir));
    let configured = settings.as_ref().map(Settings::accounts).unwrap_or_default();
    doc.get("accounts")
        .and_then(Value::as_array)
        .map(|accounts| {
            accounts
                .iter()
                .filter_map(|value| account_from_json(value, version, configured))
                .collect()
        })
        .unwrap_or_default()
}

/// Saves snapshots, replacing the file atomically. A file from a newer CodexBar is left alone, so running an older
/// build once doesn't destroy the newer one's data.
pub fn save_snapshots(dir: &Path, accounts: &[AccountSnapshot]) -> io::Result<()> {
    let existing = std::fs::read_to_string(dir.join(SNAPSHOTS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|doc| doc.get("version").and_then(Value::as_u64));
    if existing.is_some_and(|version| version > VERSION) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "snapshots.json is from a newer CodexBar and was not changed",
        ));
    }
    let doc = json!({
        "version": VERSION,
        "accounts": accounts.iter().map(account_to_json).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&doc).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(SNAPSHOTS_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn account_to_json(account: &AccountSnapshot) -> Value {
    json!({
        "id": account.id().as_str(),
        "provider": account.provider().key(),
        "label": account.label(),
        "plan": account.plan(),
        "fetchedAt": account.fetched_at().to_rfc3339(),
        "metrics": account.metrics().iter().map(metric_to_json).collect::<Vec<_>>(),
        "messages": account.messages(),
    })
}

fn account_from_json(value: &Value, version: u64, configured: &[AccountRecord]) -> Option<AccountSnapshot> {
    let id = value.get("id")?.as_str()?;
    let provider = Provider::from_key(value.get("provider")?.as_str()?)?;
    let fetched_at = time(value.get("fetchedAt")?)?;
    let metrics = value
        .get("metrics")?
        .as_array()?
        .iter()
        .map(metric_from_json)
        .collect::<Option<Vec<_>>>()?;
    let mut account = AccountSnapshot::new(AccountId::new(id), provider, metrics, fetched_at);
    let label = value.get("label").and_then(Value::as_str);
    let plan = value.get("plan").and_then(Value::as_str);
    let (label, plan) = if version < 3 && plan.is_none() {
        legacy_plan_label(provider, label, configured)
    } else {
        (label, plan)
    };
    if let Some(label) = label {
        account = account.with_label(label);
    }
    if let Some(plan) = plan {
        account = account.with_plan(plan);
    }
    for message in value.get("messages").and_then(Value::as_array).into_iter().flatten() {
        if let Some(message) = message.as_str() {
            account = account.with_message(message);
        }
    }
    Some(account)
}

/// Before version 3, Codex and Claude appended the reported plan to the configured label.
/// Only recognize the old adapters' known plan names; an arbitrary account name is not a tier.
/// Split from the right so a name containing the separator survives intact. Never expand an
/// old generic Pro/Max into a precise tier: the cache did not retain that information.
fn legacy_plan_label<'a>(
    provider: Provider,
    label: Option<&'a str>,
    configured: &[AccountRecord],
) -> (Option<&'a str>, Option<&'a str>) {
    let Some(combined) = label else {
        return (None, None);
    };
    if configured
        .iter()
        .any(|account| account.provider.eq_ignore_ascii_case(provider.key()) && account.label.trim() == combined)
    {
        return (label, None);
    }
    let (name, plan) = combined
        .rsplit_once(" · ")
        .map_or((None, combined), |(name, plan)| (Some(name), plan));
    let known = match provider {
        Provider::Codex => matches!(
            plan,
            "Free" | "Go" | "Plus" | "Pro" | "Promax" | "Team" | "Team Plus" | "Business" | "Enterprise" | "Edu"
        ),
        Provider::Claude => matches!(plan, "Free" | "Pro" | "Max" | "Team" | "Enterprise"),
        _ => false,
    };
    if known { (name, Some(plan)) } else { (label, None) }
}

fn metric_to_json(metric: &Metric) -> Value {
    match metric {
        Metric::Window {
            label,
            used,
            resets_at,
            pace,
            length,
        } => json!({
            "kind": "window", "label": label, "used": used, "resetsAt": resets_at.to_rfc3339(),
            "pacePerHour": pace.map(Pace::fraction_per_hour), "lengthSeconds": length.map(|length| length.num_seconds()),
        }),
        Metric::Quota {
            label,
            used,
            limit,
            resets_at,
            pace,
            length,
        } => json!({
            "kind": "quota", "label": label, "used": used, "limit": limit, "resetsAt": resets_at.to_rfc3339(),
            "pacePerHour": pace.map(Pace::fraction_per_hour), "lengthSeconds": length.map(|length| length.num_seconds()),
        }),
        Metric::Spend {
            label,
            spent,
            limit,
            resets_at,
            pace,
        } => json!({
            "kind": "spend", "label": label, "currency": spent.currency().code(), "spentMinor": spent.cents(),
            "limitMinor": limit.map(Money::cents), "limitCurrency": limit.map(|limit| limit.currency().code()),
            "resetsAt": resets_at.map(|at| at.to_rfc3339()), "pacePerHour": pace.map(Pace::fraction_per_hour),
        }),
        // Amounts are minor units of `currency`; files written before currencies existed are US dollars.
        Metric::Balance {
            label,
            remaining,
            burn_per_day,
        } => json!({
            "kind": "balance", "label": label, "currency": remaining.currency().code(),
            "remainingCents": remaining.cents(), "burnPerDayCents": burn_per_day.map(Money::cents),
        }),
        // A count of activity with no limit (#103). Older builds skip the kind.
        Metric::Activity { label, count } => json!({ "kind": "activity", "label": label, "count": count }),
        // A count of credits, not money (#150); no balance means unlimited. Older builds skip the kind.
        Metric::Credits { label, balance, start } => json!({
            "kind": "credits", "label": label, "balance": balance, "unlimited": balance.is_none(), "start": start,
        }),
    }
}

fn metric_from_json(value: &Value) -> Option<Metric> {
    let label = value.get("label")?.as_str()?.to_owned();
    let pace = value.get("pacePerHour").and_then(Value::as_f64).map(Pace::per_hour);
    // How long a window lasts (#223); files written before it was kept have none, and an implausible value, even one
    // too large to be a duration, counts as none (#226).
    let length = value
        .get("lengthSeconds")
        .and_then(Value::as_i64)
        .and_then(codexbar_core::window_length_from_seconds);
    Some(match value.get("kind")?.as_str()? {
        "window" => Metric::Window {
            label,
            used: value.get("used")?.as_f64()?,
            resets_at: time(value.get("resetsAt")?)?,
            pace,
            length,
        },
        "quota" => Metric::Quota {
            label,
            used: value.get("used")?.as_u64()?,
            limit: value.get("limit")?.as_u64()?,
            resets_at: time(value.get("resetsAt")?)?,
            pace,
            length,
        },
        "balance" => Metric::Balance {
            label,
            remaining: Money::new(value.get("remainingCents")?.as_i64()?, currency(value)?),
            burn_per_day: value
                .get("burnPerDayCents")
                .and_then(Value::as_i64)
                .map(|minor| Money::new(minor, currency(value).unwrap_or_default())),
        },
        "activity" => Metric::Activity {
            label,
            count: value.get("count")?.as_u64()?,
        },
        "credits" => {
            let count = |key: &str| {
                value
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite() && *n >= 0.0)
            };
            let balance = count("balance");
            if balance.is_none() && value.get("unlimited").and_then(Value::as_bool) != Some(true) {
                return None;
            }
            Metric::Credits {
                label,
                balance,
                start: count("start"),
            }
        }
        "spend" => {
            let currency = currency(value)?;
            // The limit keeps its own currency, so a cap in another currency stays incomparable after a restart.
            let limit = match value.get("limitMinor").and_then(Value::as_i64) {
                None => None,
                Some(minor) => {
                    let limit_currency = match value.get("limitCurrency").and_then(Value::as_str) {
                        None => currency,
                        Some(code) => Currency::from_code(code)?,
                    };
                    Some(Money::new(minor, limit_currency))
                }
            };
            Metric::Spend {
                label,
                spent: Money::new(value.get("spentMinor")?.as_i64()?, currency),
                limit,
                resets_at: value.get("resetsAt").and_then(time),
                pace,
            }
        }
        _ => return None,
    })
}

/// The amounts' currency: US dollars when absent (files from before currencies), `None` when unknown.
fn currency(value: &Value) -> Option<Currency> {
    match value.get("currency").and_then(Value::as_str) {
        None => Some(Currency::Usd),
        Some(code) => Currency::from_code(code),
    }
}

fn time(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};
    use codexbar_core::demo::demo_accounts;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("codexbar-snapshots-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 14, 0, 0).unwrap()
    }

    #[test]
    fn effective_plan_round_trips_separately_from_the_account_name() {
        let dir = Dir::new("plan");
        let account = AccountSnapshot::new(AccountId::new("plan"), Provider::Claude, Vec::new(), now())
            .with_label("Work")
            .with_plan("Max 5x");
        save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        assert_eq!(load_snapshots(&dir.0), [account]);
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap()).unwrap();
        assert_eq!(
            doc["version"], 3,
            "version-2 builds must refuse to overwrite persisted plans"
        );
    }

    #[test]
    fn version_two_snapshots_without_plans_load_and_upgrade_without_losing_existing_data() {
        let dir = Dir::new("legacy-version-two");
        let account = AccountSnapshot::new(AccountId::new("legacy"), Provider::Claude, Vec::new(), now())
            .with_label("Work")
            .with_message("Last good usage");
        let mut value = account_to_json(&account);
        value.as_object_mut().unwrap().remove("plan");
        std::fs::write(
            dir.0.join(SNAPSHOTS_FILE),
            json!({"version": 2, "accounts": [value]}).to_string(),
        )
        .unwrap();
        let loaded = load_snapshots(&dir.0);
        assert_eq!(loaded, [account]);
        assert_eq!(loaded[0].plan(), None);
        save_snapshots(&dir.0, &loaded).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap()).unwrap();
        assert_eq!(doc["version"], 3);
        assert_eq!(load_snapshots(&dir.0), loaded);
    }

    #[test]
    fn legacy_reported_plans_are_split_before_a_failed_refresh_can_save_them() {
        for version in [1, 2] {
            let dir = Dir::new(&format!("legacy-plan-labels-{version}"));
            let cases = [
                (Provider::Codex, "Pro", None, Some("Pro")),
                (Provider::Codex, "Work · Pro", Some("Work"), Some("Pro")),
                (Provider::Claude, "Max", None, Some("Max")),
                (
                    Provider::Claude,
                    "Work · Research · Pro",
                    Some("Work · Research"),
                    Some("Pro"),
                ),
                (Provider::Claude, "Work", Some("Work"), None),
                (Provider::Codex, "Work · Research", Some("Work · Research"), None),
                (Provider::Cursor, "Work · Pro", Some("Work · Pro"), None),
            ];
            let mut values = Vec::new();
            let mut expected = Vec::new();
            for (index, (provider, old, label, plan)) in cases.into_iter().enumerate() {
                let account = AccountSnapshot::new(
                    AccountId::new(format!("legacy-{index}")),
                    provider,
                    vec![Metric::Activity {
                        label: "Last good activity".into(),
                        count: 7,
                    }],
                    now(),
                )
                .with_message("Last good usage");
                let mut value = account_to_json(&account.clone().with_label(old));
                value.as_object_mut().unwrap().remove("plan");
                values.push(value);
                let mut migrated = account;
                if let Some(label) = label {
                    migrated = migrated.with_label(label);
                }
                if let Some(plan) = plan {
                    migrated = migrated.with_plan(plan);
                }
                expected.push(migrated);
            }
            std::fs::write(
                dir.0.join(SNAPSHOTS_FILE),
                json!({"version": version, "accounts": values}).to_string(),
            )
            .unwrap();
            let loaded = load_snapshots(&dir.0);
            assert_eq!(loaded, expected);
            // No successful fetch occurs: saving restored last-good data must retain the split.
            save_snapshots(&dir.0, &loaded).unwrap();
            assert_eq!(load_snapshots(&dir.0), expected);
        }
    }

    #[test]
    fn a_configured_legacy_account_name_that_looks_like_a_plan_stays_a_name() {
        let dir = Dir::new("legacy-custom-plan-name");
        std::fs::write(dir.0.join("settings.json"), json!({
            "accountConfigurationVersion": 1,
            "accounts": [
                {"id": "custom", "providerId": "Codex", "displayLabel": "Pro", "enabled": true, "authenticationMethod": "OAuth"},
                {"id": "work", "providerId": "Claude", "displayLabel": "Work · Pro", "enabled": true, "authenticationMethod": "OAuth"}
            ]
        }).to_string()).unwrap();
        let accounts = [
            AccountSnapshot::new(AccountId::new("signed-in-codex"), Provider::Codex, Vec::new(), now())
                .with_label("Pro"),
            AccountSnapshot::new(AccountId::new("signed-in-claude"), Provider::Claude, Vec::new(), now())
                .with_label("Work · Pro"),
        ];
        std::fs::write(
            dir.0.join(SNAPSHOTS_FILE),
            json!({"version": 2, "accounts": accounts.iter().map(account_to_json).map(|mut value| {
            value.as_object_mut().unwrap().remove("plan"); value
        }).collect::<Vec<_>>()})
            .to_string(),
        )
        .unwrap();
        assert_eq!(load_snapshots(&dir.0), accounts);
    }

    #[test]
    fn modern_labels_and_explicit_plans_are_not_reinterpreted() {
        let dir = Dir::new("modern-plan-labels");
        let accounts = [
            AccountSnapshot::new(AccountId::new("custom"), Provider::Codex, Vec::new(), now()).with_label("Work · Pro"),
            AccountSnapshot::new(AccountId::new("both"), Provider::Claude, Vec::new(), now())
                .with_label("Pro")
                .with_plan("Max 5x"),
        ];
        save_snapshots(&dir.0, &accounts).unwrap();
        assert_eq!(load_snapshots(&dir.0), accounts);
        let mut doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap()).unwrap();
        doc["version"] = json!(2);
        doc["accounts"].as_array_mut().unwrap().remove(0);
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), doc.to_string()).unwrap();
        assert_eq!(
            load_snapshots(&dir.0),
            [accounts[1].clone()],
            "explicit data always wins"
        );
    }

    #[test]
    fn snapshots_round_trip_every_metric_kind() {
        let dir = Dir::new("round-trip");
        // Trends and detail series are derived from history, not stored.
        let accounts: Vec<AccountSnapshot> = demo_accounts(now(), &Local)
            .into_iter()
            .map(|account| {
                let bare = AccountSnapshot::new(
                    account.id().clone(),
                    account.provider(),
                    account.metrics().to_vec(),
                    account.fetched_at(),
                );
                match account.label() {
                    Some(label) => bare.with_label(label),
                    None => bare,
                }
            })
            .collect();
        save_snapshots(&dir.0, &accounts).unwrap();
        assert_eq!(load_snapshots(&dir.0), accounts);
    }

    #[test]
    fn a_window_length_round_trips_and_older_or_bad_files_have_none() {
        // Files written before window lengths were kept (#223), or with a nonsense length, load with none.
        for (length, expected) in [
            ("", None),
            (r#","lengthSeconds":18000"#, Some(chrono::Duration::hours(5))),
            (r#","lengthSeconds":0"#, None),
            (r#","lengthSeconds":-60"#, None),
            (r#","lengthSeconds":"5h""#, None),
            // Too large for a duration: loading it must not panic (#226).
            (r#","lengthSeconds":9223372036854775807"#, None),
            (r#","lengthSeconds":31708800"#, None),
        ] {
            let value: Value = serde_json::from_str(&format!(
                r#"{{"kind":"window","label":"5-hour window","used":0.5,"resetsAt":"2026-10-07T15:00:00Z"{length}}}"#
            ))
            .unwrap();
            let metric = metric_from_json(&value).unwrap();
            assert_eq!(metric.window_length(), expected, "{length}");
            if expected.is_some() {
                assert_eq!(metric_from_json(&metric_to_json(&metric)), Some(metric));
            }
        }
    }

    #[test]
    fn an_activity_count_round_trips() {
        let dir = Dir::new("activity");
        let account = AccountSnapshot::new(
            AccountId::new("gr-1"),
            Provider::Greptile,
            vec![Metric::Activity {
                label: "Completed reviews".into(),
                count: 37,
            }],
            now(),
        )
        .with_label("HemSoft");
        save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        assert_eq!(load_snapshots(&dir.0), vec![account]);
    }

    #[test]
    fn spend_currency_and_messages_round_trip() {
        let dir = Dir::new("spend");
        let account = AccountSnapshot::new(
            AccountId::new("billing"),
            Provider::Copilot,
            vec![
                Metric::Spend {
                    label: "Monthly spend".into(),
                    spent: Money::new(1240, Currency::Eur),
                    limit: Some(Money::new(5000, Currency::Eur)),
                    resets_at: Some(now() + chrono::Duration::days(9)),
                    pace: Some(Pace::per_hour(0.002)),
                },
                // A cap in another currency stays incomparable after a restart.
                Metric::Spend {
                    label: "Team spend".into(),
                    spent: Money::new(1240, Currency::Eur),
                    limit: Some(Money::from_cents(5000)),
                    resets_at: None,
                    pace: None,
                },
                Metric::Balance {
                    label: "Credits".into(),
                    remaining: Money::new(1200, Currency::Jpy),
                    burn_per_day: None,
                },
            ],
            now(),
        )
        .with_message("Usage is delayed by up to an hour");
        save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        assert_eq!(load_snapshots(&dir.0), vec![account]);
    }

    #[test]
    fn files_from_before_currencies_read_as_dollars() {
        let dir = Dir::new("legacy-currency");
        std::fs::write(
            dir.0.join(SNAPSHOTS_FILE),
            r#"{ "version": 1, "accounts": [ { "id": "o", "provider": "openrouter", "fetchedAt": "2026-10-07T14:00:00Z",
                "metrics": [ { "kind": "balance", "label": "Credits", "remainingCents": 1842 } ] } ] }"#,
        )
        .unwrap();
        let loaded = load_snapshots(&dir.0);
        assert_eq!(loaded[0].metrics()[0].used_display(), "$18.42 left");
        save_snapshots(&dir.0, &loaded).unwrap();
        let text = std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap();
        assert!(text.contains(r#""version": 3"#), "rewritten in the current format");
    }

    #[test]
    fn missing_unreadable_or_newer_files_give_no_snapshots() {
        let dir = Dir::new("bad");
        assert!(load_snapshots(&dir.0).is_empty());
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), "{ nope").unwrap();
        assert!(load_snapshots(&dir.0).is_empty());
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), r#"{ "version": 4, "accounts": [] }"#).unwrap();
        assert!(load_snapshots(&dir.0).is_empty());
    }

    #[test]
    fn a_newer_file_is_never_overwritten() {
        let dir = Dir::new("newer");
        let newer = r#"{ "version": 4, "accounts": [] }"#;
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), newer).unwrap();
        let account = AccountSnapshot::new(AccountId::new("c"), Provider::Cursor, Vec::new(), now());
        assert!(save_snapshots(&dir.0, &[account]).is_err());
        assert_eq!(std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap(), newer);
    }

    #[test]
    fn an_unreadable_entry_is_skipped_not_the_whole_file() {
        let dir = Dir::new("entry");
        let good = AccountSnapshot::new(AccountId::new("c"), Provider::Cursor, Vec::new(), now());
        save_snapshots(&dir.0, std::slice::from_ref(&good)).unwrap();
        let text = std::fs::read_to_string(dir.0.join(SNAPSHOTS_FILE)).unwrap();
        let mut doc: Value = serde_json::from_str(&text).unwrap();
        doc["accounts"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": "x", "provider": "unknown" }));
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), doc.to_string()).unwrap();
        assert_eq!(load_snapshots(&dir.0), vec![good]);
    }
}
