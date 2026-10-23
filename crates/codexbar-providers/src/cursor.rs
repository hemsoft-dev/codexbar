//! Cursor included-usage for the current billing period, read with the sign-in Cursor keeps in
//! `%APPDATA%\Cursor\auth.json` on Windows and `$XDG_CONFIG_HOME/cursor/auth.json` (`~/.config`) on Linux. On macOS
//! Cursor keeps it in the Keychain instead (#161).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Months, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Money, Provider};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

const PLAN_ENDPOINT: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo";
static PLAN_BACKOFF: Mutex<Vec<(PathBuf, String, DateTime<Utc>)>> = Mutex::new(Vec::new());
const USAGE_ENDPOINT: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const SIGN_IN_HINT: &str = "Sign in to Cursor, then refresh CodexBar.";
/// The hint for a sign-in CodexBar made (#81).
pub const MANAGED_SIGN_IN_HINT: &str = "Sign in again in Settings > Accounts.";

#[cfg(windows)]
pub fn default_auth_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Cursor")
        .join("auth.json")
}

/// The file Cursor would use on macOS. It keeps the sign-in in the Keychain there, so this file only exists if a user
/// put one; `read_access_token` reads the Keychain for it.
#[cfg(target_os = "macos")]
pub fn default_auth_path() -> PathBuf {
    crate::platform::home_dir()
        .unwrap_or_default()
        .join(".cursor")
        .join("auth.json")
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn default_auth_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::platform::home_dir().map(|home| home.join(".config")))
        .unwrap_or_default()
        .join("cursor")
        .join("auth.json")
}

pub fn read_access_token(path: &Path) -> Result<String, ProviderError> {
    let not_signed_in = ProviderError::NotSignedIn { hint: SIGN_IN_HINT };
    #[cfg(target_os = "macos")]
    if path == default_auth_path() && !path.exists() {
        return keychain_token().ok_or(not_signed_in);
    }
    let text = std::fs::read_to_string(path).map_err(|_| not_signed_in.clone())?;
    let json: Value = serde_json::from_str(&text).map_err(|_| not_signed_in.clone())?;
    json.get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned)
        .ok_or(not_signed_in)
}

/// The Cursor CLI's sign-in in the macOS Keychain (service `cursor-access-token`, account `cursor-user`), through the
/// system's `security` tool. macOS asks the user once whether CodexBar may read it; an unanswered prompt gives up after
/// a minute, so the other providers' refresh goes on.
#[cfg(target_os = "macos")]
fn keychain_token() -> Option<String> {
    use std::io::Read as _;
    use std::time::{Duration, Instant};
    let mut child = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "cursor-access-token",
            "-a",
            "cursor-user",
            "-w",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    // The token is one short line, so waiting before reading the pipe can't block on a full buffer.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut token = String::new();
    child.stdout.take()?.read_to_string(&mut token).ok()?;
    let token = token.trim();
    let token = (status.success() && !token.is_empty()).then(|| token.to_owned());
    *LAST_KEYCHAIN_TOKEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = token.clone();
    token
}

/// The token the last Keychain read returned, for reads that mustn't wait on the Keychain.
#[cfg(target_os = "macos")]
static LAST_KEYCHAIN_TOKEN: Mutex<Option<String>> = Mutex::new(None);

#[cfg(target_os = "macos")]
fn last_keychain_token() -> Option<String> {
    LAST_KEYCHAIN_TOKEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Maps `GetCurrentPeriodUsage`: `planUsage.*PercentUsed` are percentages, `billingCycleEnd` (and
/// `billingCycleStart` when sent) are Unix milliseconds as a string or number, and `spendLimitUsage` carries the
/// on-demand caps (per user, and the team pool on team plans) in US cents.
///
/// Projections (#83) use the real billing period: its start when Cursor sends one, otherwise one month before the
/// end (Cursor bills monthly, also on annual plans). A start that can't be read or isn't before the end gives no
/// projection rather than a guessed one.
pub fn parse_usage(payload: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let resets_at = json
        .get("billingCycleEnd")
        .and_then(millis)
        .ok_or(ProviderError::Unexpected {
            detail: "missing billing cycle end",
        })?;
    let start = match json.get("billingCycleStart") {
        Some(value) => millis(value),
        None => resets_at.checked_sub_months(Months::new(1)),
    }
    .filter(|start| *start < resets_at);
    let pace = |used: f64| start.and_then(|start| elapsed_pace(used, start, resets_at, now));
    let plan = json.get("planUsage").ok_or(ProviderError::Unexpected {
        detail: "missing plan usage",
    })?;

    let mut metrics: Vec<Metric> = [
        ("totalPercentUsed", "Included usage"),
        ("autoPercentUsed", "Auto"),
        ("apiPercentUsed", "API"),
    ]
    .iter()
    .filter_map(|(key, label)| {
        let used = (plan.get(*key)?.as_f64()? / 100.0).clamp(0.0, 1.0);
        Some(Metric::Window {
            label: (*label).to_owned(),
            used,
            resets_at,
            pace: pace(used),
            length: start.map(|start| resets_at - start),
        })
    })
    .collect();
    if metrics.is_empty() {
        return Err(ProviderError::Unexpected {
            detail: "no usage percentages",
        });
    }
    metrics.extend(on_demand(&json, resets_at));
    Ok(AccountSnapshot::new(
        AccountId::new(LEGACY_ID),
        Provider::Cursor,
        metrics,
        now,
    ))
}

/// The id Cursor reported under before accounts were told apart (#81); still used when a token carries no identity.
pub const LEGACY_ID: &str = "cursor";

/// The account a Cursor sign-in belongs to (#81): `cursor-` and a short hash of the token's subject, so each Cursor
/// account keeps its own history, label and alerts, and signing in to another account never inherits them. The token
/// is only read, never validated: this names the account, it doesn't authenticate anything. A token that isn't a
/// readable JWT keeps the legacy id.
pub fn account_id_for_token(token: &str) -> AccountId {
    match token_subject(token) {
        Some(subject) => {
            let digest = Sha256::digest(subject.as_bytes());
            let short: String = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect();
            AccountId::new(format!("cursor-{short}"))
        }
        None => AccountId::new(LEGACY_ID),
    }
}

/// The account the sign-in in `path` belongs to, when there is one. This runs on the UI thread, so on macOS it never
/// reads the Keychain (which may show a prompt): it uses the token the last background fetch read.
pub fn signed_in_account(path: &Path) -> Option<AccountId> {
    #[cfg(target_os = "macos")]
    if path == default_auth_path() && !path.exists() {
        return last_keychain_token().map(|token| account_id_for_token(&token));
    }
    read_access_token(path).ok().map(|token| account_id_for_token(&token))
}

/// The `sub` claim of a JWT's payload.
fn token_subject(token: &str) -> Option<String> {
    crate::jwt::string_claim(&crate::jwt::claims(token)?, "sub")
}

/// Unix milliseconds, as a number or a numeric string.
fn millis(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .and_then(DateTime::from_timestamp_millis)
}

/// On-demand spend against each cap that can block it: the per-user limit and, on team plans, the team pool. Both
/// are US cents. Neither gets a pace: on-demand charges start only once included usage runs out, so an average from
/// the cycle start would understate a recent burst, and there is no onset time to project from.
fn on_demand(json: &Value, resets_at: DateTime<Utc>) -> Vec<Metric> {
    let Some(usage) = json.get("spendLimitUsage") else {
        return Vec::new();
    };
    [("individual", "On-demand"), ("pooled", "Team on-demand")]
        .iter()
        .filter_map(|(prefix, label)| {
            let amount = |field: &str| {
                usage
                    .get(format!("{prefix}{field}"))
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite())
                    .map(f64::round)
            };
            let limit = amount("Limit").filter(|limit| *limit > 0.0)?;
            let spent = amount("Used").or_else(|| Some(limit - amount("Remaining")?))?.max(0.0);
            Some(Metric::Spend {
                label: (*label).to_owned(),
                spent: Money::from_cents(spent as i64),
                limit: Some(Money::from_cents(limit as i64)),
                resets_at: Some(resets_at),
                pace: None,
            })
        })
        .collect()
}

/// The account each sign-in file last fetched for. Process-wide, because the dashboard builds its providers anew for
/// every refresh, so a switch between refreshes is still noticed.
static LAST_ACCOUNTS: Mutex<Vec<(PathBuf, AccountId)>> = Mutex::new(Vec::new());

pub struct CursorProvider<H: HttpClient> {
    http: H,
    auth_path: PathBuf,
    /// The dashboard account and name this adapter reports, when it serves one configured account of several.
    account: Option<(String, String)>,
    hint: &'static str,
}

impl<H: HttpClient> CursorProvider<H> {
    pub fn new(http: H, auth_path: PathBuf) -> Self {
        Self {
            http,
            auth_path,
            account: None,
            hint: SIGN_IN_HINT,
        }
    }

    /// Reports one configured account: `id` is the dashboard account it is expected under (its signed-in identity, or
    /// the record before a sign-in), `label` its name.
    pub fn with_account(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.account = Some((id.into(), label.into()));
        self
    }

    /// A sign-in CodexBar made: signing in again happens in Settings.
    pub fn managed(mut self) -> Self {
        self.hint = MANAGED_SIGN_IN_HINT;
        self
    }

    fn label(&self) -> Option<String> {
        self.account
            .as_ref()
            .map(|(_, label)| label.trim().to_owned())
            .filter(|label| !label.is_empty())
    }

    fn current_plan(&self, token: &str, headers: &[(&str, &str)], now: DateTime<Utc>) -> Option<String> {
        // Bind holds to this exact sign-in without retaining the token itself.
        let identity = format!("{:x}", Sha256::digest(token.as_bytes()));
        {
            let mut holds = PLAN_BACKOFF.lock().unwrap_or_else(|error| error.into_inner());
            holds.retain(|(_, _, until)| *until > now);
            if holds
                .iter()
                .any(|(path, key, _)| path == &self.auth_path && key == &identity)
            {
                return None;
            }
        }
        let response = self.http.post_json(PLAN_ENDPOINT, headers, "{}").ok()?;
        if response.status == 429 {
            let seconds = response
                .retry_after_secs
                .unwrap_or_default()
                .min(Duration::MAX.num_seconds() as u64) as i64;
            let until = now
                .checked_add_signed(Duration::seconds(seconds).max(Duration::minutes(15)))
                .unwrap_or(DateTime::<Utc>::MAX_UTC);
            PLAN_BACKOFF.lock().unwrap_or_else(|error| error.into_inner()).push((
                self.auth_path.clone(),
                identity,
                until,
            ));
        }
        if response.status != 200 {
            return None;
        }
        let json: Value = serde_json::from_str(&response.body).ok()?;
        json.pointer("/planInfo/planName")?
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    fn usage(&self, token: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let bearer = format!("Bearer {token}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("Connect-Protocol-Version", "1"),
        ];
        let response = self.http.post_json(USAGE_ENDPOINT, &headers, "{}")?;
        match response.status {
            200..=299 => {
                let account = parse_usage(&response.body, now)?;
                Ok(match self.current_plan(token, &headers, now) {
                    Some(plan) => account.with_plan(plan),
                    None => {
                        account.with_message("Cursor's plan lookup did not report a plan; usage is still available.")
                    }
                })
            }
            401 | 403 => Err(ProviderError::Expired { hint: self.hint }),
            status => Err(ProviderError::Http { status }),
        }
    }
}

impl<H: HttpClient> UsageProvider for CursorProvider<H> {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.fetch_outcomes(now)?
            .into_iter()
            .map(|outcome| match outcome {
                AccountOutcome::Fresh(account) => Ok(account),
                AccountOutcome::Failed { error, .. } => Err(error),
            })
            .collect()
    }

    /// No sign-in fails the provider as a whole. Once the token is read the account is known, so a failed fetch is
    /// that account's failure, never attributed to the account shown before a switch.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        let token = read_access_token(&self.auth_path).map_err(|_| ProviderError::NotSignedIn { hint: self.hint })?;
        let id = account_id_for_token(&token);
        let outcome = match self.usage(&token, now) {
            Ok(usage) => {
                let mut account = AccountSnapshot::new(id.clone(), Provider::Cursor, usage.metrics().to_vec(), now);
                if let Some(label) = self.label() {
                    account = account.with_label(label);
                }
                if let Some(plan) = usage.plan() {
                    account = account.with_plan(plan);
                }
                for message in usage.messages() {
                    account = account.with_message(message.clone());
                }
                let mut last = LAST_ACCOUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let previous = last.iter_mut().find(|(path, _)| *path == self.auth_path);
                match previous {
                    Some((_, previous)) => {
                        if *previous != id {
                            account = account.with_message(if self.hint == MANAGED_SIGN_IN_HINT {
                                "Signed in to a different Cursor account. It is a new account here; the previous one's usage isn't carried over."
                            } else {
                                "The Cursor app is signed in to a different account. The previous account's usage stays with it."
                            });
                        }
                        *previous = id;
                    }
                    None => last.push((self.auth_path.clone(), id)),
                }
                AccountOutcome::Fresh(account)
            }
            Err(error) => AccountOutcome::Failed {
                account: id,
                label: self.label(),
                error,
            },
        };
        Ok(vec![outcome])
    }

    fn account_id(&self) -> Option<&str> {
        self.account.as_ref().map(|(id, _)| id.as_str())
    }

    fn account_label(&self) -> Option<&str> {
        self.account.as_ref().map(|(_, label)| label.as_str())
    }

    /// The sign-in's account; a configured account that is signed out still holds the identity it had, so its saved
    /// usage is shown rather than set aside.
    fn signed_in_account(&self) -> Option<AccountId> {
        signed_in_account(&self.auth_path).or_else(|| self.account.as_ref().map(|(id, _)| AccountId::new(id.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpResponse;
    use codexbar_core::Severity;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    const USAGE: &str = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":71,"autoPercentUsed":40,"apiPercentUsed":12.5}}"#;

    struct Fixed(HttpResponse);

    impl HttpClient for Fixed {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            unreachable!("Cursor posts")
        }

        fn post_json(&self, _: &str, _: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
            assert_eq!(body, "{}");
            Ok(self.0.clone())
        }
    }

    #[test]
    fn parse_usage_maps_percentages_and_cycle_end() {
        let account = parse_usage(USAGE, now()).unwrap();
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, ["Included usage", "Auto", "API"]);
        assert_eq!(account.primary().unwrap().used_fraction(), Some(0.71));
        assert_eq!(
            account.primary().unwrap().resets_at(),
            Some("2026-10-21T00:00:00Z".parse().unwrap())
        );
        // 71% in 16 days of a 30-day cycle runs out before Oct 21.
        assert_eq!(account.assess(now()).severity(), Severity::AtRisk);
    }

    /// A payload shaped like Cursor's, with the fields CodexBar doesn't read left in.
    const FULL: &str = r#"{
        "billingCycleStart": "1790553600000",
        "billingCycleEnd": "1792540800000",
        "planUsage": {"totalSpend": 1420, "includedSpend": 1420, "limit": 2000,
                      "totalPercentUsed": 71, "autoPercentUsed": 40, "apiPercentUsed": 12.5},
        "spendLimitUsage": {"individualLimit": 2000, "individualRemaining": 1500},
        "displayMessage": "You've used 71% of your included usage"
    }"#;

    fn with_start(start: &str) -> String {
        format!(
            r#"{{"billingCycleStart":{start},"billingCycleEnd":"1792540800000","planUsage":{{"totalPercentUsed":40}}}}"#
        )
    }

    #[test]
    fn parse_usage_projects_over_the_real_billing_period() {
        let account = parse_usage(FULL, now()).unwrap();
        let included = account.primary().unwrap();
        // Sep 28 to Oct 21: 71% in 9 days 2 hours runs out well before the reset.
        let projected = included.projected_at_reset(now()).unwrap();
        assert!((projected - (0.71 + 0.71 / 218.0 * 334.0)).abs() < 1e-6, "{projected}");
        assert!(included.exhausts_before_reset(now()));
        // The default one-month period projects less from the same usage.
        let monthly = parse_usage(USAGE, now()).unwrap();
        let projected = monthly.primary().unwrap().projected_at_reset(now()).unwrap();
        assert!((projected - (0.71 + 0.71 / 386.0 * 334.0)).abs() < 1e-6, "{projected}");
    }

    #[test]
    fn parse_usage_maps_on_demand_spend_without_a_projection() {
        let account = parse_usage(FULL, now()).unwrap();
        let on_demand = account.metrics().last().unwrap();
        assert_eq!(on_demand.key(), "on-demand");
        assert_eq!(on_demand.used_display(), "$5.00 of $20.00");
        assert_eq!(on_demand.headroom(), Some(Money::from_cents(1500)));
        // On-demand starts only after included usage runs out, so a cycle average would understate it.
        assert_eq!(on_demand.projected_at_reset(now()), None);
    }

    #[test]
    fn team_plans_report_the_team_pool_as_its_own_cap() {
        // Shaped like Cursor's team response: the pool is nearly spent while the per-user cap has room.
        let team = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":100},
            "spendLimitUsage":{"totalSpend":49000,"pooledLimit":50000,"pooledUsed":49000,"pooledRemaining":1000,
                "individualLimit":10000,"individualUsed":1200,"individualRemaining":8800,"limitType":"team"}}"#;
        let account = parse_usage(team, now()).unwrap();
        let keys: Vec<String> = account.metrics().iter().map(Metric::key).collect();
        assert_eq!(keys, ["included-usage", "on-demand", "team-on-demand"]);
        assert_eq!(account.metrics()[1].used_display(), "$12.00 of $100.00");
        assert_eq!(account.metrics()[2].used_display(), "$490.00 of $500.00");
        assert_eq!(account.assess(now()).severity(), Severity::LimitSoon);
        // A team response with only the pool still shows it.
        let pooled = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":10},
            "spendLimitUsage":{"pooledLimit":50000,"pooledRemaining":20000,"limitType":"team"}}"#;
        let account = parse_usage(pooled, now()).unwrap();
        assert_eq!(account.metrics()[1].used_display(), "$300.00 of $500.00");
    }

    #[test]
    fn parse_usage_without_an_on_demand_limit_adds_no_spend() {
        let zero = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":10},
            "spendLimitUsage":{"individualLimit":0,"individualRemaining":0}}"#;
        assert_eq!(parse_usage(zero, now()).unwrap().metrics().len(), 1);
        let partial = r#"{"billingCycleEnd":"1792540800000","planUsage":{"totalPercentUsed":10},
            "spendLimitUsage":{"individualLimit":2000}}"#;
        assert_eq!(parse_usage(partial, now()).unwrap().metrics().len(), 1);
    }

    #[test]
    fn invalid_or_zero_length_periods_omit_projections() {
        for start in [r#""soon""#, "null", r#""1792540800000""#, r#""1795000000000""#] {
            let account = parse_usage(&with_start(start), now()).unwrap();
            let metric = account.primary().unwrap();
            assert_eq!(metric.used_fraction(), Some(0.4), "{start}: usage is still shown");
            assert_eq!(metric.projected_at_reset(now()), None, "{start}");
            assert_eq!(account.assess(now()).severity(), Severity::Normal, "{start}");
        }
    }

    #[test]
    fn projections_respect_the_clock() {
        // Before the period starts, in its first tenth, and after it ended: nothing to project from.
        let start = "2026-09-28T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-10-21T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for at in [
            start - chrono::Duration::hours(1),
            start + chrono::Duration::hours(12),
            end,
            end + chrono::Duration::days(1),
        ] {
            let account = parse_usage(FULL, at).unwrap();
            assert_eq!(account.primary().unwrap().projected_at_reset(at), None, "{at}");
        }
        let later = start + chrono::Duration::days(5);
        assert!(
            parse_usage(FULL, later)
                .unwrap()
                .primary()
                .unwrap()
                .projected_at_reset(later)
                .is_some()
        );
    }

    #[test]
    fn parse_usage_numeric_cycle_end_and_missing_plan() {
        let numeric = r#"{"billingCycleEnd":1792540800000,"planUsage":{"totalPercentUsed":10}}"#;
        assert_eq!(parse_usage(numeric, now()).unwrap().metrics().len(), 1);
        assert!(parse_usage(r#"{"billingCycleEnd":1792540800000}"#, now()).is_err());
    }

    /// An unsigned JWT with the given subject, shaped like Cursor's.
    fn token(subject: &str) -> String {
        let encode = |text: &str| {
            const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let bytes = text.as_bytes();
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = ((chunk[0] as u32) << 16)
                    | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
                    | (chunk.get(2).copied().unwrap_or(0) as u32);
                for ix in 0..=chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * ix)) & 63) as usize] as char);
                }
            }
            out
        };
        format!(
            "{}.{}.signature",
            encode(r#"{"alg":"HS256","typ":"JWT"}"#),
            encode(&format!(r#"{{"sub":"{subject}","type":"session","scope":"openid"}}"#))
        )
    }

    #[test]
    fn each_cursor_sign_in_is_its_own_account() {
        let alice = account_id_for_token(&token("google-oauth2|111"));
        let bob = account_id_for_token(&token("google-oauth2|222"));
        assert!(alice.as_str().starts_with("cursor-") && alice.as_str().len() == "cursor-".len() + 12);
        assert_ne!(alice, bob);
        assert_eq!(
            alice,
            account_id_for_token(&token("google-oauth2|111")),
            "stable for one account"
        );
        assert!(!alice.as_str().contains("111"), "the subject itself isn't exposed");
        // A token that isn't a readable JWT keeps the legacy id.
        assert_eq!(account_id_for_token("opaque-token").as_str(), LEGACY_ID);
        assert_eq!(account_id_for_token("a.%%%.c").as_str(), LEGACY_ID);
    }

    struct PlanHttp {
        profiles: Mutex<Vec<HttpResponse>>,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl HttpClient for PlanHttp {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            unreachable!()
        }
        fn post_json(&self, url: &str, _: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
            assert_eq!(body, "{}");
            if url == PLAN_ENDPOINT {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(self.profiles.lock().unwrap().remove(0));
            }
            assert_eq!(url, USAGE_ENDPOINT);
            Ok(HttpResponse::new(200, USAGE))
        }
    }

    #[test]
    fn cursor_plan_names_follow_explicit_profile_changes_without_changing_identity() {
        let auth = Auth::new("plans", "plan-source");
        let profile = |name| HttpResponse::new(200, serde_json::json!({"planInfo":{"planName":name}}).to_string());
        let provider = CursorProvider::new(
            PlanHttp {
                profiles: Mutex::new(vec![profile("Premium"), profile("Ultra")]),
                calls: std::sync::atomic::AtomicUsize::new(0),
            },
            auth.0.clone(),
        )
        .with_account("record", "Work");
        let first = provider.fetch(now()).unwrap().remove(0);
        let next = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(first.plan(), Some("Premium"));
        assert_eq!(next.plan(), Some("Ultra"));
        assert_eq!(first.id(), next.id());
        assert_eq!(next.label(), Some("Work"));
    }

    #[test]
    fn a_refused_or_unknown_cursor_plan_does_not_hide_usage_or_guess_from_price() {
        for (index, response) in [
            HttpResponse::new(403, ""),
            HttpResponse::new(200, r#"{"planInfo":{"price":200}}"#),
        ]
        .into_iter()
        .enumerate()
        {
            let auth = Auth::new(&format!("plans-missing-{index}"), "plan-missing");
            let provider = CursorProvider::new(
                PlanHttp {
                    profiles: Mutex::new(vec![response]),
                    calls: std::sync::atomic::AtomicUsize::new(0),
                },
                auth.0.clone(),
            );
            let account = provider.fetch(now()).unwrap().remove(0);
            assert_eq!(account.plan(), None);
            assert!(!account.metrics().is_empty());
        }
    }

    #[test]
    fn cursor_plan_rate_limits_do_not_block_usage_refreshes() {
        let auth = Auth::new("plans-rate-limit", "plan-rate-limit");
        let provider = CursorProvider::new(
            PlanHttp {
                profiles: Mutex::new(vec![
                    HttpResponse {
                        retry_after_secs: Some(3600),
                        ..HttpResponse::new(429, "")
                    },
                    HttpResponse::new(200, r#"{"planInfo":{"planName":"Premium"}}"#),
                ]),
                calls: std::sync::atomic::AtomicUsize::new(0),
            },
            auth.0.clone(),
        );
        assert_eq!(provider.fetch(now()).unwrap()[0].plan(), None);
        assert_eq!(provider.fetch(now() + Duration::minutes(2)).unwrap()[0].plan(), None);
        assert_eq!(provider.http.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            provider.fetch(now() + Duration::minutes(61)).unwrap()[0].plan(),
            Some("Premium")
        );
    }

    struct Auth(PathBuf);

    impl Auth {
        fn new(name: &str, subject: &str) -> Self {
            let path = std::env::temp_dir().join(format!("codexbar-cursor-{}-{name}.json", std::process::id()));
            let auth = Self(path);
            auth.sign_in(subject);
            auth
        }

        fn sign_in(&self, subject: &str) {
            std::fs::write(&self.0, format!(r#"{{"accessToken":"{}"}}"#, token(subject))).unwrap();
        }
    }

    impl Drop for Auth {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_configured_cursor_account_is_named_and_keeps_its_identity_signed_out() {
        let auth = Auth::new("configured", "google-oauth2|333");
        let provider = || {
            CursorProvider::new(Fixed(HttpResponse::new(200, USAGE)), auth.0.clone())
                .managed()
                .with_account("cursor-aaaaaaaaaaaa", "Work")
        };
        let account = provider().fetch(now()).unwrap().remove(0);
        assert_eq!(account.label(), Some("Work"));
        assert_eq!(provider().account_id(), Some("cursor-aaaaaaaaaaaa"));
        assert_eq!(provider().account_label(), Some("Work"));
        std::fs::remove_file(&auth.0).unwrap();
        // Signed out: it still holds the identity it had, and says to sign in from Settings.
        assert_eq!(
            provider()
                .signed_in_account()
                .map(|id| id.as_str().to_owned())
                .as_deref(),
            Some("cursor-aaaaaaaaaaaa")
        );
        assert_eq!(
            provider().fetch_outcomes(now()).err(),
            Some(ProviderError::NotSignedIn {
                hint: MANAGED_SIGN_IN_HINT
            })
        );
    }

    #[test]
    fn a_refused_configured_account_is_its_own_failure() {
        let auth = Auth::new("refused", "google-oauth2|444");
        let provider = CursorProvider::new(Fixed(HttpResponse::new(401, "")), auth.0.clone())
            .managed()
            .with_account("cursor-bbbbbbbbbbbb", "Work");
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { label, error, .. } = &outcomes[0] else {
            panic!("expected a failure")
        };
        assert_eq!(label.as_deref(), Some("Work"));
        assert_eq!(
            error,
            &ProviderError::Expired {
                hint: MANAGED_SIGN_IN_HINT
            }
        );
    }

    #[test]
    fn switching_cursor_accounts_starts_a_separate_account_and_says_so() {
        let auth = Auth::new("switch", "google-oauth2|111");
        // A new provider each time, as the dashboard builds them for every refresh.
        let provider = || CursorProvider::new(Fixed(HttpResponse::new(200, USAGE)), auth.0.clone());
        let first = provider().fetch(now()).unwrap().remove(0);
        assert_eq!(first.id(), &account_id_for_token(&token("google-oauth2|111")));
        assert!(
            !first
                .messages()
                .iter()
                .any(|message| message.contains("signed in to a different"))
        );
        assert_eq!(provider().signed_in_account(), Some(first.id().clone()));
        // The Cursor app signs in to another account.
        auth.sign_in("google-oauth2|222");
        let second = provider().fetch(now()).unwrap().remove(0);
        assert_ne!(second.id(), first.id(), "nothing carries over");
        assert_eq!(
            second
                .messages()
                .iter()
                .filter(|message| message.contains("signed in to a different"))
                .count(),
            1
        );
        assert!(second.messages().iter().any(|message| message
            == "The Cursor app is signed in to a different account. The previous account's usage stays with it."));
        assert!(
            !provider().fetch(now()).unwrap()[0]
                .messages()
                .iter()
                .any(|message| message.contains("signed in to a different")),
            "said once"
        );
        // Signed out: the provider as a whole has no sign-in.
        std::fs::remove_file(&auth.0).unwrap();
        assert!(matches!(
            provider().fetch(now()),
            Err(ProviderError::NotSignedIn { .. })
        ));
        assert_eq!(provider().signed_in_account(), None);
    }

    #[test]
    fn a_failed_fetch_is_the_signed_in_accounts_failure() {
        let auth = Auth::new("failure", "google-oauth2|333");
        let provider = CursorProvider::new(Fixed(HttpResponse::new(401, "")), auth.0.clone());
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        match &outcomes[..] {
            [AccountOutcome::Failed { account, error, .. }] => {
                assert_eq!(account, &account_id_for_token(&token("google-oauth2|333")));
                assert!(matches!(error, ProviderError::Expired { .. }));
            }
            _ => panic!("one failed outcome for the signed-in account"),
        }
    }

    #[test]
    fn fetch_missing_auth_is_not_signed_in() {
        let provider = CursorProvider::new(Fixed(HttpResponse::new(200, USAGE)), PathBuf::from("Z:/nope/auth.json"));
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
    }

    #[test]
    fn fetch_posts_and_maps_rejection() {
        let path = std::env::temp_dir().join(format!("codexbar-cursor-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"accessToken":"tok"}"#).unwrap();
        let ok = CursorProvider::new(Fixed(HttpResponse::new(200, USAGE)), path.clone());
        assert_eq!(ok.fetch(now()).unwrap().len(), 1);
        let rejected = CursorProvider::new(Fixed(HttpResponse::new(401, "")), path.clone());
        assert!(matches!(rejected.fetch(now()), Err(ProviderError::Expired { .. })));
        let _ = std::fs::remove_file(path);
    }
}
