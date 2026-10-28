//! Non-inference Muse Code allowance, pinned to CLI 1.4.13-R6996.1 / schema 1.
//! Free-form reason/hint, tokens, billing and undocumented pools are never interpreted.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration as Timeout,
};

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Pace, Provider};
use serde_json::Value;

use crate::{CommandError, CommandOptions, CommandRunner, ProviderError, UsageProvider};

pub const FRESH_FOR_SECONDS: i64 = 420;
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub struct MuseCache(Mutex<Option<Cache>>);

struct Cache {
    account: (String, String),
    next: DateTime<Utc>,
    failures: u32,
    result: Result<Vec<AccountSnapshot>, ProviderError>,
}

pub struct MuseProvider<R> {
    runner: R,
    home: PathBuf,
    id: String,
    label: String,
    cache: Arc<MuseCache>,
}
impl<R: CommandRunner> MuseProvider<R> {
    /// Shared by the settings hub because the app rebuilds adapters for each refresh.
    pub fn with_cache(mut self, cache: Arc<MuseCache>) -> Self {
        self.cache = cache;
        self
    }
    pub fn new(runner: R, home: PathBuf, id: String, label: String) -> Self {
        Self {
            runner,
            home,
            id,
            label,
            cache: Arc::new(MuseCache::default()),
        }
    }
}
impl<R: CommandRunner> UsageProvider for MuseProvider<R> {
    fn name(&self) -> &'static str {
        "Muse Code"
    }
    fn account_id(&self) -> Option<&str> {
        Some(&self.id)
    }
    fn account_label(&self) -> Option<&str> {
        Some(&self.label)
    }
    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        let mut cache = self.cache.0.lock().map_err(|_| unexpected("Muse cache unavailable"))?;
        let identity = (self.id.clone(), self.label.clone());
        if cache.as_ref().is_some_and(|cached| cached.account != identity) {
            *cache = None;
        }
        if let Some(cached) = cache.as_ref().filter(|cached| now < cached.next) {
            return cached.result.clone();
        }
        let options = CommandOptions {
            cwd: Some(self.home.clone()),
            remove_env: vec![
                "META_API_KEY",
                "MUSE_LOGIN",
                "MUSE_NO_AUTO_UPDATE",
                "MUSE_SYNC_UPDATE",
                "MUSE_LAUNCHER_INSTALL",
            ],
            env: vec![("MUSE_LOGIN", "0"), ("MUSE_NO_AUTO_UPDATE", "1")],
            output_limit: OUTPUT_LIMIT,
            stop_on_stderr: None,
        };
        let result = self
            .runner
            .run_with_options("muse", &["usage", "--json"], Timeout::from_secs(15), &options)
            .map_err(|error| match error {
                CommandError::NotFound => unexpected("Muse CLI not installed"),
                CommandError::TimedOut => unexpected("Muse usage timed out"),
                CommandError::Failed => unexpected("Muse usage transport failed"),
            })
            .and_then(|output| {
                if !output.success {
                    return Err(unexpected("Muse usage command failed"));
                }
                parse(&output.stdout, now, &self.id, &self.label).map(|account| vec![account])
            });
        let failures = if result.is_ok() {
            0
        } else {
            cache.as_ref().map_or(1, |cached| cached.failures.saturating_add(1))
        };
        let minutes = if failures == 0 {
            5
        } else {
            (5_i64 * (1_i64 << failures.min(4).saturating_sub(1))).min(30)
        };
        let mut next = now + Duration::minutes(minutes);
        if let Ok(accounts) = &result {
            for reset in accounts.iter().flat_map(|a| a.metrics()).filter_map(Metric::resets_at) {
                next = next.min(reset);
            }
        }
        *cache = Some(Cache {
            account: identity,
            next,
            failures,
            result: result.clone(),
        });
        result
    }
}
fn unexpected(detail: &'static str) -> ProviderError {
    ProviderError::Unexpected { detail }
}

fn epoch(value: &Value) -> Option<DateTime<Utc>> {
    let ms = value.as_f64().filter(|n| n.is_finite() && *n >= 1e12 && *n < 1e13)?;
    DateTime::from_timestamp_millis(ms as i64)
}
fn safe_text(value: Option<&Value>, max: usize, valid: impl Fn(char) -> bool) -> Option<String> {
    value?
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && s.chars().all(valid))
        .map(str::to_owned)
}

/// Parse only the documented quota fields. Public for sanitized producer-to-consumer qualification.
pub fn parse(text: &str, fetched: DateTime<Utc>, id: &str, label: &str) -> Result<AccountSnapshot, ProviderError> {
    if text.len() > OUTPUT_LIMIT {
        return Err(unexpected("Muse usage output too large"));
    }
    let root: Value = serde_json::from_str(text).map_err(|_| unexpected("Muse usage output malformed"))?;
    if root.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(unexpected("Muse usage schema unsupported"));
    }
    let allowance = root
        .get("allowance")
        .filter(|v| !v.is_null())
        .ok_or_else(|| unexpected("Muse allowance unavailable (CLI reported none)"))?;
    let allowance = allowance
        .as_object()
        .ok_or_else(|| unexpected("Muse usage output malformed"))?;
    let observed = allowance
        .get("asOfMs")
        .and_then(epoch)
        .ok_or_else(|| unexpected("Muse allowance observation time missing"))?;
    if observed > fetched + Duration::seconds(60) {
        return Err(unexpected("Muse allowance observation time is in the future"));
    }
    let mut metrics = Vec::new();
    let mut warnings = Vec::new();
    for (key, label, seconds) in [("window", "5-hour window", 18000), ("weekly", "Weekly", 604800)] {
        let Some(raw) = allowance.get(key) else {
            continue;
        };
        let length = Duration::seconds(seconds);
        let window = raw
            .get("usedPercent")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
            .zip(raw.get("resetsAtMs").and_then(epoch))
            .filter(|(_, reset)| {
                *reset > observed && *reset > fetched && *reset - observed <= length && *reset - fetched <= length
            });
        let Some((percent, resets_at)) = window else {
            warnings.push(format!("{label} invalid"));
            continue;
        };
        let elapsed = length - (resets_at - observed);
        let used = percent / 100.0;
        let pace = (used > 0.0 && elapsed >= codexbar_core::sus::WEEKLY_MATURITY)
            .then(|| Pace::per_hour(used * 3600.0 / elapsed.num_milliseconds() as f64 * 1000.0));
        metrics.push(Metric::Window {
            label: label.into(),
            used,
            resets_at,
            pace,
            length: Some(length),
        });
    }
    if metrics.is_empty() {
        return Err(unexpected("Muse allowance has no valid windows"));
    }
    let extra = allowance
        .iter()
        .filter(|(key, raw)| {
            !matches!(key.as_str(), "window" | "weekly")
                && raw.get("usedPercent").is_some()
                && raw.get("resetsAtMs").is_some()
        })
        .count();
    if extra > 0 {
        warnings.push(format!("{extra} undocumented allowance windows ignored"));
    }
    if fetched - observed > Duration::seconds(FRESH_FOR_SECONDS) {
        warnings.push("Muse allowance observation is stale (over 7 minutes old)".into());
    }
    let source = safe_text(allowance.get("source"), 32, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c)
    });
    let mut account = AccountSnapshot::new(AccountId::new(id), Provider::MuseCode, metrics, fetched)
        .with_label(label)
        .with_observation(observed, source, FRESH_FOR_SECONDS);
    if let Some(tier) = safe_text(allowance.get("tier"), 64, |c| {
        c.is_ascii_alphanumeric() || " ._()+-".contains(c)
    }) {
        account = account.with_plan(tier);
    }
    for warning in warnings {
        account = account.with_message(warning);
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandOutput;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering},
    };
    const MEASURED: &str = include_str!("../fixtures/muse/measured.json");
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1793037245064).unwrap()
    }
    fn reading(text: &str, at: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        parse(text, at, "muse-account", "Demo")
    }
    #[test]
    fn measured_mapping_and_observation_are_independent_of_fetch_time() {
        let a = reading(MEASURED, now() + Duration::minutes(1)).unwrap();
        assert_eq!(a.provider(), Provider::MuseCode);
        assert_eq!(a.plan(), Some("Muse Code Power Usage"));
        assert_eq!(a.observed_at(), Some(now()));
        assert_eq!(a.allowance_source(), Some("mint"));
        assert_eq!(a.fetched_at(), now() + Duration::minutes(1));
        assert_eq!(
            a.metrics().iter().map(Metric::key).collect::<Vec<_>>(),
            ["5-hour-window", "weekly"]
        );
        for (m, used, seconds, reset) in [
            (&a.metrics()[0], 0.15, 18000, 1793053104000),
            (&a.metrics()[1], 0.05, 604800, 1793577600000),
        ] {
            assert_eq!(m.used_fraction(), Some(used));
            assert_eq!(m.window_length(), Some(Duration::seconds(seconds)));
            assert_eq!(m.resets_at().unwrap().timestamp_millis(), reset);
        }
        let projected = a.metrics()[0]
            .elapsed_projection(a.evaluation_time(&a.metrics()[0], now()).unwrap())
            .unwrap();
        assert!((1.255..1.265).contains(&projected));
        assert_eq!(codexbar_core::sus::subscription_score(&a, now()), None);
        assert_eq!(a.subscription_price(), None);
    }
    #[test]
    fn every_window_is_firm_from_one_hour() {
        let observed = now().timestamp_millis();
        for (elapsed, projects) in [
            (Duration::hours(1) - Duration::minutes(1), false),
            (Duration::hours(1), true),
        ] {
            let reset = observed + (Duration::days(7) - elapsed).num_milliseconds();
            let payload = MEASURED.replace("1793577600000", &reset.to_string());
            let weekly = reading(&payload, now()).unwrap().metrics()[1].clone();
            assert_eq!(weekly.projected_at_reset(now()).is_some(), projects, "{elapsed}");
        }
        for (elapsed, projects) in [(Duration::seconds(119), false), (Duration::hours(1), true)] {
            let reset = observed + (Duration::hours(5) - elapsed).num_milliseconds();
            let payload = MEASURED.replace("1793053104000", &reset.to_string());
            let window = reading(&payload, now()).unwrap().metrics()[0].clone();
            assert_eq!(window.projected_at_reset(now()).is_some(), projects, "{elapsed}");
        }
    }
    #[test]
    fn zero_exhausted_partial_and_bad_siblings_keep_truthful_values() {
        let zero = reading(include_str!("../fixtures/muse/zero.json"), now()).unwrap();
        assert!(zero.metrics().iter().all(|m| m.used_fraction() == Some(0.0)));
        let exhausted = reading(include_str!("../fixtures/muse/exhausted.json"), now()).unwrap();
        assert!(exhausted.metrics().iter().all(|m| m.used_fraction() == Some(1.0)));
        assert!(exhausted.metrics()[0].elapsed_projection(now()).unwrap() > 10.0);
        for payload in [
            include_str!("../fixtures/muse/partial.json"),
            include_str!("../fixtures/muse/reset-beyond-window.json"),
        ] {
            let partial = reading(payload, now()).unwrap();
            assert_eq!(partial.metrics().len(), 1);
            assert_eq!(partial.messages().len(), 1);
        }
        assert!(reading(include_str!("../fixtures/muse/malformed-values.json"), now()).is_err());
    }
    #[test]
    fn null_tokens_unknown_billing_and_untrusted_diagnostics_never_become_quota() {
        for reason in [
            "expired auth Fixture User@example.com",
            "PAYG",
            "denied entitlement",
            "rate limited",
        ] {
            let text = serde_json::json!({"schemaVersion":1,"allowance":null,"reason":reason,"hint":reason,"run":{"usage":{"tokens":999}}}).to_string();
            let error = reading(&text, now()).unwrap_err().to_string();
            assert!(error.contains("allowance unavailable"));
            assert!(!error.contains(reason));
        }
        for payload in [
            "{",
            "[]",
            r#"{"schemaVersion":"1","allowance":null}"#,
            include_str!("../fixtures/muse/unsupported-schema.json"),
            include_str!("../fixtures/muse/truncated.json"),
            r#"{"schemaVersion":1,"allowance":{"asOfMs":1793037245064,"tokens":123,"run":{"usage":500}}}"#,
        ] {
            assert!(reading(payload, now()).is_err(), "{payload}");
        }
        let unknown = reading(include_str!("../fixtures/muse/extra-window.json"), now()).unwrap();
        assert_eq!(unknown.metrics().len(), 1);
        assert_eq!(unknown.plan(), None);
        assert!(unknown.messages().iter().any(|m| m.contains("undocumented")));
    }
    #[test]
    fn reset_rollover_and_stale_future_observations_do_not_claim_current_capacity() {
        let stale = reading(MEASURED, now() + Duration::minutes(8)).unwrap();
        assert!(stale.observation_stale(now() + Duration::minutes(8)));
        assert_eq!(stale.observed_at(), Some(now()));
        assert!(stale.messages()[0].contains("stale"));
        assert!(reading(MEASURED, now() - Duration::seconds(61)).is_err());
        let rolled = reading(MEASURED, DateTime::from_timestamp_millis(1793053104001).unwrap()).unwrap();
        assert_eq!(rolled.metrics().len(), 1);
        assert_eq!(rolled.metrics()[0].key(), "weekly");
        assert!(reading(MEASURED, DateTime::from_timestamp_millis(1793577600001).unwrap()).is_err());
        for value in [
            serde_json::json!(-1),
            serde_json::json!("15"),
            Value::Null,
            serde_json::json!(1e300),
        ] {
            let mut payload: Value = serde_json::from_str(MEASURED).unwrap();
            payload["allowance"]["window"]["usedPercent"] = value;
            let result = reading(&payload.to_string(), now());
            // A huge finite value can exceed the projection's numeric range, but cannot break the weekly sibling.
            assert!(result.is_ok());
            assert!(result.unwrap().metrics().iter().any(|m| m.key() == "weekly"));
        }
    }
    struct Fake {
        calls: AtomicUsize,
        answers: Mutex<VecDeque<Result<CommandOutput, CommandError>>>,
    }
    impl CommandRunner for Fake {
        fn run(&self, _: &str, _: &[&str], _: Timeout) -> Result<CommandOutput, CommandError> {
            panic!("unscoped execution forbidden")
        }
        fn run_with_options(
            &self,
            program: &str,
            args: &[&str],
            timeout: Timeout,
            options: &CommandOptions,
        ) -> Result<CommandOutput, CommandError> {
            assert_eq!(program, "muse");
            assert_eq!(args, ["usage", "--json"]);
            assert_eq!(timeout, Timeout::from_secs(15));
            assert_eq!(options.output_limit, OUTPUT_LIMIT);
            assert_eq!(options.cwd, Some(PathBuf::from("/synthetic-home")));
            assert!(options.remove_env.contains(&"META_API_KEY"));
            assert_eq!(options.env, [("MUSE_LOGIN", "0"), ("MUSE_NO_AUTO_UPDATE", "1")]);
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.answers.lock().unwrap().pop_front().expect("unexpected command")
        }
    }
    fn provider(answers: Vec<Result<CommandOutput, CommandError>>) -> MuseProvider<Fake> {
        MuseProvider::new(
            Fake {
                calls: AtomicUsize::new(0),
                answers: Mutex::new(answers.into()),
            },
            PathBuf::from("/synthetic-home"),
            "muse-account".into(),
            "Demo".into(),
        )
    }
    fn output(payload: &str, success: bool) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput {
            success,
            stdout: payload.into(),
            stderr: "never surface Fixture User@example.com".into(),
        })
    }
    #[test]
    fn acquisition_cache_and_bounded_backoff_preserve_original_timestamp() {
        let p = provider(vec![
            output(MEASURED, true),
            Err(CommandError::TimedOut),
            Err(CommandError::Failed),
            output("private command failure", false),
            Err(CommandError::NotFound),
        ]);
        let first = p.fetch(now()).unwrap();
        assert_eq!(p.fetch(now() + Duration::minutes(4)).unwrap(), first);
        assert_eq!(p.runner.calls.load(Ordering::Relaxed), 1);
        let mut at = now() + Duration::minutes(5);
        for (minutes, detail) in [
            (5, "timed out"),
            (10, "transport failed"),
            (20, "command failed"),
            (30, "not installed"),
        ] {
            let error = p.fetch(at).unwrap_err();
            assert!(error.to_string().contains(detail));
            assert!(!error.to_string().contains("private"));
            assert_eq!(
                p.cache.0.lock().unwrap().as_ref().unwrap().next,
                at + Duration::minutes(minutes)
            );
            assert_eq!(p.fetch(at + Duration::seconds(1)).unwrap_err(), error);
            at += Duration::minutes(minutes);
        }
    }
    #[test]
    fn cache_never_outlives_earliest_reset_and_account_identity_survives_rollover() {
        let mut payload: Value = serde_json::from_str(MEASURED).unwrap();
        payload["allowance"]["window"]["resetsAtMs"] =
            serde_json::json!((now() + Duration::minutes(1)).timestamp_millis());
        let p = provider(vec![output(&payload.to_string(), true), output(MEASURED, true)]);
        let a = p.fetch(now()).unwrap().remove(0);
        assert_eq!(
            p.cache.0.lock().unwrap().as_ref().unwrap().next,
            now() + Duration::minutes(1)
        );
        let b = p.fetch(now() + Duration::minutes(1)).unwrap().remove(0);
        assert_eq!(a.id(), b.id());
        assert_eq!(a.metrics()[0].key(), b.metrics()[0].key());
    }
    #[test]
    fn reconstructed_adapter_reuses_only_the_same_configured_accounts_cache() {
        let first = provider(vec![output(MEASURED, true)]);
        let cache = first.cache.clone();
        let expected = first.fetch(now()).unwrap();
        let rebuilt = provider(vec![]).with_cache(cache.clone());
        assert_eq!(rebuilt.fetch(now() + Duration::minutes(1)).unwrap(), expected);
        assert_eq!(rebuilt.runner.calls.load(Ordering::Relaxed), 0);
        let mut other = provider(vec![output(MEASURED, true)]).with_cache(cache);
        other.id = "other-account".into();
        let account = other.fetch(now() + Duration::minutes(1)).unwrap().remove(0);
        assert_eq!(account.id().as_str(), "other-account");
        assert_eq!(other.runner.calls.load(Ordering::Relaxed), 1);
    }
}
