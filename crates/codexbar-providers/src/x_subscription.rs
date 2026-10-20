//! The X Subscription (#172): Grok, Grok Bot, Cursor and X Premium sold as one plan since 2026-10-12. Its card shows
//! what Grok's Settings > Usage page shows, each once:
//!
//! - **Weekly SuperGrok limit**, **Extra usage credits** and the plan name, from the Grok CLI's sign-in through
//!   [`GrokProvider::usage`] (the billing proxy's shared weekly pool and prepaid balance).
//! - **Weekly Grok Bot limit**, from Cursor's `DashboardService/GetSandUsageStatus` with the Cursor sign-in. Grok's
//!   own page shows the same ledger: on 2026-10-16 both said 87% used, resetting at 21:10 UTC.
//!
//! The two weekly limits have their own windows and resets, and are never added together. The xAI token can't read
//! Cursor's ledger (401) and the CLI proxy has no Grok Bot endpoint, so the card needs both sign-ins.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider};
use serde_json::Value;

use crate::grok::GrokProvider;
use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider, cursor};

const SAND_USAGE: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";

pub const SUPERGROK_LABEL: &str = "Weekly SuperGrok limit";
pub const GROK_BOT_LABEL: &str = "Weekly Grok Bot limit";
pub const GROK_BOT_TRIAL_LABEL: &str = "Grok Bot trial";
/// The Cursor and Grok CLI sign-ins are separate stores, and The X Subscription links accounts whatever their email
/// (xAI's unify guide), so nothing here proves they're the same account; the card says where the Grok Bot limit came
/// from.
const UNVERIFIED_LINK: &str = "The Grok Bot limit is the Cursor sign-in's on this computer; CodexBar can't confirm \
                               it's linked to this xAI account.";

/// What Cursor says about the Grok Bot allowance: a metric when it reports one, and why not otherwise.
#[derive(Debug, Default, PartialEq)]
pub struct GrokBotUsage {
    pub metric: Option<Metric>,
    pub message: Option<String>,
}

fn instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn unavailable(message: &str) -> GrokBotUsage {
    GrokBotUsage {
        metric: None,
        message: Some(message.to_owned()),
    }
}

/// Parses `GetSandUsageStatus`. A missing, null or malformed percent stays unavailable; an explicit zero is reported
/// usage. Pooled team allowances and plans without an included allowance have no personal weekly limit.
pub fn parse_grok_bot(body: &str, now: DateTime<Utc>) -> GrokBotUsage {
    let Ok(Value::Object(status)) = serde_json::from_str::<Value>(body) else {
        return unavailable("Cursor's Grok Bot usage came in an unknown shape.");
    };
    let flag = |key: &str| status.get(key).and_then(Value::as_bool);
    if flag("usesPooledEnterpriseAllowance") == Some(true) {
        return unavailable("Grok Bot uses the team's pooled allowance, so there's no personal weekly limit.");
    }
    // `includedLimitZero` is the current flag and decides when present; the older `hasNonZeroIncludedLimit` only stands
    // in when it's absent, as in Cursor's own mapping.
    let has_allowance = flag("includedLimitZero")
        .map(|zero| !zero)
        .or_else(|| flag("hasNonZeroIncludedLimit"));
    // An active trial reports no included allowance, but its usage counts until the trial ends (Cursor's own mapping).
    let trial_ends = instant(status.get("sandTrialExpiresAt"))
        .filter(|end| now < *end)
        .filter(|_| has_allowance != Some(true));
    if has_allowance == Some(false) && trial_ends.is_none() {
        return unavailable("This plan includes no Grok Bot allowance.");
    }
    let used = match status.get("usagePercent") {
        None | Some(Value::Null) => return unavailable("Cursor didn't report Grok Bot usage."),
        Some(value) => match value.as_f64().filter(|p| p.is_finite() && *p >= 0.0) {
            Some(percent) => percent / 100.0,
            None => return unavailable("Cursor's Grok Bot usage is malformed."),
        },
    };
    if let Some(ends) = trial_ends {
        // A trial has no weekly reset; its date is when the trial ends.
        return GrokBotUsage {
            metric: Some(Metric::Window {
                label: GROK_BOT_TRIAL_LABEL.into(),
                used,
                resets_at: ends,
                pace: None,
                length: None,
            }),
            message: Some("The Grok Bot trial doesn't reset weekly; its date is when the trial ends.".to_owned()),
        };
    }
    let Some(resets_at) = instant(status.get("nextResetTimestampUtc")).filter(|at| now < *at) else {
        return unavailable("Cursor didn't report when the Grok Bot week resets.");
    };
    // Paid Grok Bot grants run a fixed seven-day week; `currentPeriodStart` can fall mid-week or be absent, so the pace
    // runs from a week before the reset.
    let pace = elapsed_pace(used, resets_at - Duration::days(7), resets_at, now);
    GrokBotUsage {
        metric: Some(Metric::Window {
            label: GROK_BOT_LABEL.into(),
            used,
            resets_at,
            pace,
            length: Some(Duration::days(7)),
        }),
        message: None,
    }
}

/// One X Subscription account: the Grok CLI sign-in in `grok`, and the Cursor sign-in at `cursor_auth`.
pub struct XSubscriptionProvider<H: HttpClient> {
    grok: GrokProvider<H>,
    http: H,
    cursor_auth: PathBuf,
    account: String,
    label: String,
}

impl<H: HttpClient> XSubscriptionProvider<H> {
    pub fn new(
        grok: GrokProvider<H>,
        http: H,
        cursor_auth: PathBuf,
        account: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            grok,
            http,
            cursor_auth,
            account: account.into(),
            label: label.into(),
        }
    }

    fn grok_bot(&self, now: DateTime<Utc>) -> GrokBotUsage {
        let Ok(token) = cursor::read_access_token(&self.cursor_auth) else {
            return unavailable("The Weekly Grok Bot limit needs the Cursor app's sign-in on this computer.");
        };
        let bearer = format!("Bearer {token}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("Connect-Protocol-Version", "1"),
        ];
        match self.http.post_json(SAND_USAGE, &headers, "{}") {
            Ok(response) => match response.status {
                200..=299 => parse_grok_bot(&response.body, now),
                401 | 403 => {
                    unavailable("Cursor refused its sign-in for Grok Bot usage. Open Cursor to sign in again.")
                }
                status => unavailable(&format!("Couldn't read Grok Bot usage (HTTP {status}).")),
            },
            Err(_) => unavailable("Couldn't reach Cursor for Grok Bot usage."),
        }
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        // The Grok sign-in owns the account: without it there is no plan to show.
        let usage = self.grok.usage(now)?;
        let bot = self.grok_bot(now);
        let mut metrics: Vec<Metric> = Vec::new();
        let mut rest: Vec<Metric> = Vec::new();
        for metric in usage.metrics {
            match metric {
                Metric::Window {
                    used,
                    resets_at,
                    pace,
                    length,
                    ..
                } => metrics.push(Metric::Window {
                    label: SUPERGROK_LABEL.into(),
                    used,
                    resets_at,
                    pace,
                    length,
                }),
                other => rest.push(other),
            }
        }
        let link = bot.metric.as_ref().map(|_| UNVERIFIED_LINK.to_owned());
        metrics.extend(bot.metric);
        metrics.extend(rest);
        let mut account = AccountSnapshot::new(
            AccountId::new(self.account.clone()),
            Provider::XSubscription,
            metrics,
            now,
        );
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        Ok(usage
            .messages
            .into_iter()
            .chain(bot.message)
            .chain(link)
            .fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> UsageProvider for XSubscriptionProvider<H> {
    fn name(&self) -> &'static str {
        "X Subscription"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.snapshot(now).map(|account| vec![account])
    }

    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        Ok(vec![match self.snapshot(now) {
            Ok(account) => AccountOutcome::Fresh(account),
            Err(error) => AccountOutcome::Failed {
                account: AccountId::new(self.account.clone()),
                label: Some(self.label.clone()).filter(|l| !l.trim().is_empty()),
                error,
            },
        }])
    }

    fn account_id(&self) -> Option<&str> {
        Some(&self.account)
    }

    fn account_label(&self) -> Option<&str> {
        Some(&self.label)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::HttpResponse;
    use crate::grok::GrokCli;

    fn now() -> DateTime<Utc> {
        "2026-10-16T18:00:00Z".parse().unwrap()
    }

    /// Franz's live Grok Bot shape on 2026-10-16, without the upgrade links.
    const SAND: &str = r#"{"currentPeriodStart":"2026-10-09T21:10:41.281Z","nextResetTimestampUtc":"2026-10-16T21:10:41.281Z",
        "usagePercent":87.396666,"hasAvailableUsage":true,"hasNonZeroIncludedLimit":true,"grokPlanLabel":"Grok Bot Plan",
        "cursorPlanName":"Pro","billingBrand":"SAND_BILLING_BRAND_CURSOR"}"#;

    #[test]
    fn grok_bot_reads_its_own_weekly_window() {
        let usage = parse_grok_bot(SAND, now());
        let metric = usage.metric.expect("a reported percent is a meter");
        assert_eq!(metric.label(), GROK_BOT_LABEL);
        assert!((metric.used_fraction().unwrap() - 0.87396666).abs() < 1e-9);
        assert_eq!(metric.resets_at(), "2026-10-16T21:10:41.281Z".parse().ok());
        assert_eq!(usage.message, None);
    }

    #[test]
    fn grok_bot_zero_is_reported_but_missing_null_and_malformed_are_not() {
        let zero = parse_grok_bot(&SAND.replace("87.396666", "0"), now());
        assert_eq!(zero.metric.and_then(|m| m.used_fraction()), Some(0.0));
        for (from, to, why) in [
            (
                r#""usagePercent":87.396666,"#,
                "",
                "Cursor didn't report Grok Bot usage.",
            ),
            ("87.396666", "null", "Cursor didn't report Grok Bot usage."),
            ("87.396666", r#""lots""#, "Cursor's Grok Bot usage is malformed."),
            ("87.396666", "-4", "Cursor's Grok Bot usage is malformed."),
        ] {
            let usage = parse_grok_bot(&SAND.replace(from, to), now());
            assert!(usage.metric.is_none(), "{to}");
            assert_eq!(usage.message.as_deref(), Some(why), "{to}");
        }
    }

    #[test]
    fn grok_bot_without_a_personal_allowance_or_a_current_week_has_no_meter() {
        for (from, to) in [
            (
                r#""hasNonZeroIncludedLimit":true"#,
                r#""hasNonZeroIncludedLimit":false"#,
            ),
            (r#""hasNonZeroIncludedLimit":true"#, r#""includedLimitZero":true"#),
            (
                r#""hasNonZeroIncludedLimit":true"#,
                r#""usesPooledEnterpriseAllowance":true"#,
            ),
            ("2026-10-16T21:10:41.281Z", "2026-10-16T17:00:00Z"),
            (r#""nextResetTimestampUtc":"2026-10-16T21:10:41.281Z","#, ""),
        ] {
            let usage = parse_grok_bot(&SAND.replace(from, to), now());
            assert!(usage.metric.is_none() && usage.message.is_some(), "{to}");
        }
        assert!(parse_grok_bot("[]", now()).metric.is_none());
    }

    #[test]
    fn grok_bot_included_limit_zero_decides_over_the_older_flag() {
        let stale = SAND.replace(
            r#""hasNonZeroIncludedLimit":true"#,
            r#""hasNonZeroIncludedLimit":false,"includedLimitZero":false"#,
        );
        assert!(
            parse_grok_bot(&stale, now()).metric.is_some(),
            "a stale legacy flag doesn't hide the meter"
        );
        let zero = SAND.replace(
            r#""hasNonZeroIncludedLimit":true"#,
            r#""hasNonZeroIncludedLimit":true,"includedLimitZero":true"#,
        );
        assert!(parse_grok_bot(&zero, now()).metric.is_none());
    }

    #[test]
    fn an_active_grok_bot_trial_shows_until_it_ends() {
        // The upstream trial shape: no included allowance, a trial expiry ahead.
        let trial = SAND.replace(
            r#""hasNonZeroIncludedLimit":true"#,
            r#""hasNonZeroIncludedLimit":false,"includedLimitZero":true,"sandTrialExpiresAt":"2026-10-20T00:00:00Z""#,
        );
        let usage = parse_grok_bot(&trial, now());
        match usage.metric {
            Some(Metric::Window {
                ref label,
                resets_at,
                pace,
                ..
            }) => {
                assert_eq!(label, GROK_BOT_TRIAL_LABEL);
                assert_eq!(resets_at, "2026-10-20T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
                assert_eq!(pace, None, "a trial has no weekly pace");
            }
            ref other => panic!("no trial meter: {other:?}"),
        }
        assert!(usage.message.unwrap().contains("trial ends"));
        let ended = trial.replace("2026-10-20T00:00:00Z", "2026-10-15T00:00:00Z");
        assert!(
            parse_grok_bot(&ended, now()).metric.is_none(),
            "an ended trial is no allowance"
        );
    }

    #[test]
    fn grok_bot_pace_runs_over_the_seven_day_week_before_the_reset() {
        let pace_of = |body: &str| match parse_grok_bot(body, now()).metric {
            Some(Metric::Window { pace, .. }) => pace,
            other => panic!("no window: {other:?}"),
        };
        let week = pace_of(SAND);
        assert!(week.is_some());
        // A mid-week or missing period start changes nothing.
        let mid_week = SAND.replace("2026-10-09T21:10:41.281Z", "2026-10-15T00:00:00Z");
        let absent = SAND.replace(r#""currentPeriodStart":"2026-10-09T21:10:41.281Z","#, "");
        assert_eq!(pace_of(&mid_week), week);
        assert_eq!(pace_of(&absent), week);
    }

    /// A temporary folder with a Grok CLI `auth.json` and a Cursor `auth.json`, removed when dropped.
    struct Homes(PathBuf);

    impl Homes {
        fn new(name: &str, cursor_signed_in: bool) -> Self {
            let root = std::env::temp_dir().join(format!("codexbar-xsub-{name}-{}", std::process::id()));
            std::fs::create_dir_all(root.join("grok")).unwrap();
            std::fs::write(
                root.join("grok").join("auth.json"),
                r#"{"https://auth.x.ai::b1a00492":{"key":"xai-tok","expires_at":"2026-10-17T18:00:00Z","user_id":"sub-1"}}"#,
            )
            .unwrap();
            if cursor_signed_in {
                std::fs::write(root.join("cursor-auth.json"), r#"{"accessToken":"cursor-tok"}"#).unwrap();
            }
            Self(root)
        }
    }

    impl Drop for Homes {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct NoRenew;

    impl GrokCli for NoRenew {
        fn renew(&self, _: &Path) -> bool {
            false
        }
    }

    /// Answers the Grok proxy's GETs and Cursor's POST, and records which token each request carried.
    struct Server {
        grok: Mutex<Vec<HttpResponse>>,
        sand: Option<HttpResponse>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl HttpClient for Server {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            assert!(url.starts_with("https://cli-chat-proxy.grok.com/"));
            self.seen.lock().unwrap().push(format!("GET {url} {}", headers[0].1));
            let mut script = self.grok.lock().unwrap();
            if script.is_empty() {
                return Err(ProviderError::Network);
            }
            Ok(script.remove(0))
        }

        fn post_json(&self, url: &str, headers: &[(&str, &str)], _: &str) -> Result<HttpResponse, ProviderError> {
            self.seen.lock().unwrap().push(format!("POST {url} {}", headers[0].1));
            self.sand.clone().ok_or(ProviderError::Network)
        }
    }

    fn x_subscription(
        homes: &Homes,
        sand: Option<HttpResponse>,
    ) -> (XSubscriptionProvider<Server>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let billing = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-15T16:32:13+00:00",
            "end":"2026-10-22T16:32:13+00:00"},"isUnifiedBillingUser":true,"creditUsagePercent":12,"prepaidBalance":{}}}"#;
        let grok_http = Server {
            grok: Mutex::new(vec![
                HttpResponse::new(200, billing),
                HttpResponse::new(200, r#"{"subscription_tier_display":"SuperGrok"}"#),
            ]),
            sand: None,
            seen: seen.clone(),
        };
        let cursor_http = Server {
            grok: Mutex::new(Vec::new()),
            sand,
            seen: seen.clone(),
        };
        let grok = GrokProvider::new(grok_http, Arc::new(NoRenew), homes.0.join("grok"), None, "xs-1", "");
        let provider = XSubscriptionProvider::new(grok, cursor_http, homes.0.join("cursor-auth.json"), "xs-1", "Me");
        (provider, seen)
    }

    #[test]
    fn the_card_shows_the_three_usage_page_metrics_once_each() {
        let homes = Homes::new("all", true);
        let (provider, seen) = x_subscription(&homes, Some(HttpResponse::new(200, SAND)));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.display_name(), "X Subscription · Me");
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, [SUPERGROK_LABEL, GROK_BOT_LABEL, "Extra usage credits"]);
        assert_eq!(account.metrics()[0].used_fraction(), Some(0.12));
        assert_eq!(account.metrics()[0].resets_at(), "2026-10-22T16:32:13Z".parse().ok());
        assert_eq!(account.metrics()[2].used_display(), "$0.00 left");
        assert!(account.messages().iter().any(|m| m == "Plan: SuperGrok."));
        assert!(
            account.messages().iter().any(|m| m == UNVERIFIED_LINK),
            "the card says the Grok Bot limit's sign-in isn't proven to be this account's"
        );
        // Each token goes only to its own service.
        let seen = seen.lock().unwrap();
        assert!(seen.iter().all(|line| {
            (line.contains("grok.com") && line.ends_with("Bearer xai-tok"))
                || (line.contains("cursor.sh") && line.ends_with("Bearer cursor-tok"))
        }));
    }

    #[test]
    fn a_missing_or_refused_cursor_sign_in_keeps_the_grok_metrics() {
        let homes = Homes::new("no-cursor", false);
        let (provider, seen) = x_subscription(&homes, Some(HttpResponse::new(200, SAND)));
        let account = provider.fetch(now()).unwrap().remove(0);
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, [SUPERGROK_LABEL, "Extra usage credits"]);
        assert!(account.messages().iter().any(|m| m.contains("Cursor app's sign-in")));
        assert!(seen.lock().unwrap().iter().all(|line| !line.contains("cursor.sh")));

        let homes = Homes::new("refused", true);
        let (provider, _) = x_subscription(&homes, Some(HttpResponse::new(401, "{}")));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 2);
        assert!(account.messages().iter().any(|m| m.starts_with("Cursor refused")));
    }

    #[test]
    fn without_the_grok_sign_in_the_account_fails() {
        let homes = Homes::new("no-grok", true);
        std::fs::remove_file(homes.0.join("grok").join("auth.json")).unwrap();
        let (provider, _) = x_subscription(&homes, Some(HttpResponse::new(200, SAND)));
        let outcome = provider.fetch_outcomes(now()).unwrap().remove(0);
        assert!(matches!(
            outcome,
            AccountOutcome::Failed {
                error: ProviderError::NotSignedIn { .. },
                ..
            }
        ));
    }

    /// Reads this computer's real Grok CLI and Cursor sign-ins and prints each metric's label, use and reset (never a
    /// token), to compare with Grok's Settings > Usage. Needs both sign-ins and the network. Run with `--ignored`.
    #[test]
    #[ignore = "reads this computer's Grok and Cursor sign-ins"]
    fn the_signed_in_x_subscription_matches_the_usage_page() {
        let home = std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| crate::platform::home_dir().map(|home| home.join(".grok")))
            .unwrap();
        let grok = GrokProvider::new(
            crate::UreqClient::without_redirects(),
            Arc::new(crate::grok::SystemGrokCli),
            home,
            None,
            "live",
            "",
        );
        let provider = XSubscriptionProvider::new(
            grok,
            crate::UreqClient::without_redirects(),
            cursor::default_auth_path(),
            "live",
            "",
        );
        let account = provider.fetch(Utc::now()).unwrap().remove(0);
        for metric in account.metrics() {
            println!(
                "{}: {} (resets {:?})",
                metric.label(),
                metric.used_display(),
                metric.resets_at()
            );
        }
        for message in account.messages() {
            println!("note: {message}");
        }
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert!(labels.contains(&GROK_BOT_LABEL), "{labels:?}");
    }
}
