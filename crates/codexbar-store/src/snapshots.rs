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

use crate::settings::{AccountRecord, AuthMethod, Settings};

pub const SNAPSHOTS_FILE: &str = "snapshots.json";
/// Version 4 adds subscription prices. Version 3 added reported plans. Version 2 added currencies, spend and messages.
/// Version 1 files read as US dollars with no messages.
const VERSION: u64 = 4;

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
    // A configured name can itself be "Pro" or end in " · Pro". Keep it a name only for that same account.
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
        "subscriptionRecord": account.subscription_record(),
        "subscriptionPrice": account.subscription_price().map(crate::subscription_price::to_json),
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
        legacy_plan_label(provider, id, label, configured)
    } else {
        (label, plan)
    };
    if let Some(label) = label {
        account = account.with_label(label);
    }
    if let Some(plan) = plan {
        account = account.with_plan(plan);
    }
    if let Some(price) = value
        .get("subscriptionPrice")
        .and_then(crate::subscription_price::from_json)
    {
        account = account.with_subscription_price(price);
    }
    if let Some(record) = value.get("subscriptionRecord").and_then(Value::as_str) {
        account = account.with_subscription_record(record.to_owned());
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
    id: &str,
    label: Option<&'a str>,
    configured: &[AccountRecord],
) -> (Option<&'a str>, Option<&'a str>) {
    let Some(combined) = label else {
        return (None, None);
    };
    let same_provider = |account: &&AccountRecord| account.provider.eq_ignore_ascii_case(provider.key());
    let known = configured
        .iter()
        .filter(same_provider)
        .find(|account| account.id == id || account.external_id.as_deref() == Some(id));
    // Default CLI adapters do not store their live identity on the generated settings record. Codex uses a
    // user/workspace hash; Claude keeps the account-neutral id. A known record always takes precedence, so an
    // automatic record's name cannot suppress migration on a different managed account.
    let default_identity = match provider {
        Provider::Codex => {
            id == "codex-chatgpt"
                || id
                    .strip_prefix("codex-")
                    .is_some_and(|hash| hash.len() == 12 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        }
        Provider::Claude => id == "claude",
        _ => false,
    };
    let configured_name = known.map_or_else(
        || {
            default_identity
                && configured.iter().filter(same_provider).any(|account| {
                    matches!(account.method, AuthMethod::Automatic | AuthMethod::CommandLine)
                        && account.external_id.is_none()
                        && account.label.trim() == combined
                })
        },
        |account| account.label.trim() == combined,
    );
    if configured_name {
        return (label, None);
    }
    let (name, plan) = combined
        .rsplit_once(" · ")
        .map_or((None, combined), |(name, plan)| (Some(name), plan));
    let migrated = match provider {
        Provider::Codex => match plan {
            "Hc" | "Ent26" => Some("Enterprise"),
            "Enterprise Cbp Automation" => Some("Enterprise (Automation)"),
            "Sci" => Some("Academic Researchers"),
            "Sci Plus" => Some("Academic Researchers Plus"),
            "Education" => Some("Edu"),
            "Promax" => Some("Pro Max"),
            "Free"
            | "Go"
            | "Plus"
            | "Pro"
            | "Team"
            | "Team Plus"
            | "Business"
            | "Enterprise"
            | "Edu"
            | "Self Serve Business Prolite"
            | "Self Serve Business Usage Based"
            | "Enterprise Cbp Usage Based"
            | "Edu Plus"
            | "Edu Pro" => Some(plan),
            _ => None,
        },
        Provider::Claude => matches!(plan, "Free" | "Pro" | "Max" | "Team" | "Enterprise").then_some(plan),
        _ => None,
    };
    migrated.map_or((label, None), |plan| (name, Some(plan)))
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
    fn prices_round_trip_and_version_three_without_prices_still_loads() {
        let dir = Dir::new("sus-price");
        let price = codexbar_core::sus::Price {
            monthly_cents: Some(20_000),
            scheduled: Some((Some(10_000), now() + chrono::Duration::days(2))),
        };
        let account = AccountSnapshot::new(AccountId::new("stable"), Provider::Codex, Vec::new(), now())
            .with_label("Work")
            .with_subscription_price(price)
            .with_subscription_record("configured-source".to_owned());
        save_snapshots(&dir.0, std::slice::from_ref(&account)).unwrap();
        assert_eq!(load_snapshots(&dir.0).as_slice(), std::slice::from_ref(&account));
        let path = dir.0.join(SNAPSHOTS_FILE);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["version"] = 3.into();
        doc["accounts"][0].as_object_mut().unwrap().remove("subscriptionPrice");
        std::fs::write(path, doc.to_string()).unwrap();
        let restored = load_snapshots(&dir.0);
        assert_eq!(restored[0].id(), account.id());
        assert_eq!(restored[0].label(), account.label());
        assert!(restored[0].subscription_price().is_none());
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
            doc["version"], 4,
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
        assert_eq!(doc["version"], 4);
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
    fn every_known_codex_tier_migrates_from_its_old_serialized_spelling() {
        // Version 1/2 title-cased underscore-separated API codes, except prolite -> Pro.
        // Cover current supported codes and aliases, not only the personal tiers.
        let tiers = [
            ("Free", "Free"),
            ("Go", "Go"),
            ("Plus", "Plus"),
            ("Pro", "Pro"),
            ("Promax", "Pro Max"),
            ("Team", "Team"),
            ("Team Plus", "Team Plus"),
            ("Business", "Business"),
            ("Hc", "Enterprise"),
            ("Ent26", "Enterprise"),
            ("Enterprise", "Enterprise"),
            ("Enterprise Cbp Automation", "Enterprise (Automation)"),
            ("Enterprise Cbp Usage Based", "Enterprise Cbp Usage Based"),
            ("Self Serve Business Prolite", "Self Serve Business Prolite"),
            ("Self Serve Business Usage Based", "Self Serve Business Usage Based"),
            ("Edu", "Edu"),
            ("Education", "Edu"),
            ("Edu Plus", "Edu Plus"),
            ("Edu Pro", "Edu Pro"),
            ("Sci", "Academic Researchers"),
            ("Sci Plus", "Academic Researchers Plus"),
        ];
        for version in [1, 2] {
            let dir = Dir::new(&format!("all-legacy-codex-tiers-{version}"));
            let mut values = Vec::new();
            let mut expected = Vec::new();
            for (index, (old, plan)) in tiers.into_iter().enumerate() {
                for label in [None, Some("Work · Research")] {
                    let account = AccountSnapshot::new(
                        AccountId::new(format!("tier-{index}-{}", label.is_some())),
                        Provider::Codex,
                        Vec::new(),
                        now(),
                    );
                    let combined = label.map_or_else(|| old.to_owned(), |name| format!("{name} · {old}"));
                    let mut value = account_to_json(&account.clone().with_label(combined));
                    value.as_object_mut().unwrap().remove("plan");
                    values.push(value);
                    let account = account.with_plan(plan);
                    expected.push(match label {
                        Some(name) => account.with_label(name),
                        None => account,
                    });
                }
            }
            std::fs::write(
                dir.0.join(SNAPSHOTS_FILE),
                json!({"version":version,"accounts":values}).to_string(),
            )
            .unwrap();
            let loaded = load_snapshots(&dir.0);
            assert_eq!(loaded, expected);
            save_snapshots(&dir.0, &loaded).unwrap();
            assert_eq!(
                load_snapshots(&dir.0),
                expected,
                "no successful refresh is needed to preserve the plan"
            );
        }
    }

    #[test]
    fn legacy_plan_like_names_are_preserved_only_for_the_same_account() {
        let dir = Dir::new("legacy-custom-plan-name");
        std::fs::write(dir.0.join("settings.json"), json!({
            "accountConfigurationVersion": 1,
            "accounts": [
                {"id": "custom", "externalAccountId": "signed-in-codex", "providerId": "Codex", "displayLabel": "Pro", "enabled": true, "authenticationMethod": "OAuth"},
                {"id": "work", "externalAccountId": "signed-in-claude", "providerId": "Claude", "displayLabel": "Work · Pro", "enabled": true, "authenticationMethod": "OAuth"}
            ]
        }).to_string()).unwrap();
        let accounts = [
            AccountSnapshot::new(AccountId::new("signed-in-codex"), Provider::Codex, Vec::new(), now())
                .with_label("Pro"),
            AccountSnapshot::new(AccountId::new("signed-in-claude"), Provider::Claude, Vec::new(), now())
                .with_label("Work · Pro"),
            AccountSnapshot::new(AccountId::new("custom"), Provider::Codex, Vec::new(), now()).with_label("Pro"),
            AccountSnapshot::new(AccountId::new("automatic-codex"), Provider::Codex, Vec::new(), now())
                .with_label("Pro"),
            AccountSnapshot::new(AccountId::new("other-claude"), Provider::Claude, Vec::new(), now())
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
        let expected = [
            accounts[0].clone(),
            accounts[1].clone(),
            accounts[2].clone(),
            AccountSnapshot::new(AccountId::new("automatic-codex"), Provider::Codex, Vec::new(), now())
                .with_plan("Pro"),
            AccountSnapshot::new(AccountId::new("other-claude"), Provider::Claude, Vec::new(), now())
                .with_label("Work")
                .with_plan("Pro"),
        ];
        let loaded = load_snapshots(&dir.0);
        assert_eq!(
            loaded, expected,
            "another account's configured name must not hide a legacy plan"
        );
        save_snapshots(&dir.0, &loaded).unwrap();
        assert_eq!(
            load_snapshots(&dir.0),
            expected,
            "a failed refresh cannot persist the wrong split"
        );
    }

    #[test]
    fn automatic_legacy_names_survive_the_default_adapters_different_identity() {
        for version in [1, 2] {
            for method in ["Automatic", "CommandLine"] {
                for (provider, automatic_id, tier) in [
                    (Provider::Codex, "codex-0123456789ab", "Pro"),
                    (Provider::Claude, "claude", "Max"),
                ] {
                    let other_id = if provider == Provider::Codex {
                        "codex-abcdef012345"
                    } else {
                        "claude-abcdef012345"
                    };
                    for name in [tier.to_owned(), format!("Work · {tier}")] {
                        let dir = Dir::new("legacy-automatic-name");
                        std::fs::write(dir.0.join("settings.json"), json!({
                            "accountConfigurationVersion": 1,
                            "accounts": [
                                {"id":"generated-default-record", "providerId":provider.key(), "displayLabel":name,
                                 "enabled":true, "authenticationMethod":method},
                                {"id":"managed-record", "externalAccountId":other_id, "providerId":provider.key(),
                                 "displayLabel":"Other", "enabled":true,
                                 "authenticationMethod":if provider == Provider::Codex { "OAuth" } else { "BrowserSession" }}
                            ]
                        }).to_string()).unwrap();
                        let automatic = AccountSnapshot::new(AccountId::new(automatic_id), provider, Vec::new(), now())
                            .with_label(name.clone());
                        let other = AccountSnapshot::new(AccountId::new(other_id), provider, Vec::new(), now())
                            .with_label(name.clone());
                        let values: Vec<_> = [&automatic, &other]
                            .into_iter()
                            .map(account_to_json)
                            .map(|mut value| {
                                value.as_object_mut().unwrap().remove("plan");
                                value
                            })
                            .collect();
                        std::fs::write(
                            dir.0.join(SNAPSHOTS_FILE),
                            json!({"version":version,"accounts":values}).to_string(),
                        )
                        .unwrap();
                        let other =
                            AccountSnapshot::new(AccountId::new(other_id), provider, Vec::new(), now()).with_plan(tier);
                        let other = if name == tier { other } else { other.with_label("Work") };
                        let expected = [automatic, other];
                        assert_eq!(
                            load_snapshots(&dir.0),
                            expected,
                            "{provider:?} {method} v{version} {name}"
                        );
                        save_snapshots(&dir.0, &expected).unwrap();
                        assert_eq!(load_snapshots(&dir.0), expected);
                    }
                }
            }
        }
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
        assert!(text.contains(r#""version": 4"#), "rewritten in the current format");
    }

    #[test]
    fn missing_unreadable_or_newer_files_give_no_snapshots() {
        let dir = Dir::new("bad");
        assert!(load_snapshots(&dir.0).is_empty());
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), "{ nope").unwrap();
        assert!(load_snapshots(&dir.0).is_empty());
        std::fs::write(dir.0.join(SNAPSHOTS_FILE), r#"{ "version": 5, "accounts": [] }"#).unwrap();
        assert!(load_snapshots(&dir.0).is_empty());
    }

    #[test]
    fn a_newer_file_is_never_overwritten() {
        let dir = Dir::new("newer");
        let newer = r#"{ "version": 5, "accounts": [] }"#;
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
