//! Grok consumer usage (#108), experimental: the shared weekly pool of a paid SuperGrok plan and Extra Usage Credits,
//! from the Grok CLI's billing proxy (`cli-chat-proxy.grok.com/v1/billing?format=credits`). That is xAI's first-party
//! CLI contract, not a published API.
//!
//! Each account has its own Grok CLI home (`GROK_HOME`), signed in by the official CLI (`grok login`) and renewed by it
//! (`grok models` refreshes an expiring token), so CodexBar borrows no OAuth client. API/developer-team spending,
//! monthly billing dates, product shares and Cursor's Grok Bot allowance are never folded into the weekly meter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Money, Provider};
use serde_json::Value;

use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

const BILLING: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SETTINGS: &str = "https://cli-chat-proxy.grok.com/v1/settings";
const ISSUER: &str = "https://auth.x.ai";
const SIGN_IN_HINT: &str = "Sign in to Grok in CodexBar settings.";
const OTHER_ACCOUNT: &str = "The Grok CLI here is signed in to a different xAI account than this one. Sign in again.";
/// Plans whose weekly pool the omitted-usage convention applies to; exact names only.
const PAID_PLANS: [&str; 4] = ["SuperGrok Lite", "SuperGrok", "SuperGrok Plus", "SuperGrok Heavy"];
/// A token is renewed when it expires within this long.
const RENEW_MARGIN: Duration = Duration::minutes(5);
/// The CLI is asked to renew a home at most this often.
const RENEW_EVERY: StdDuration = StdDuration::from_secs(5 * 60);

/// The Grok CLI's sign-in in one home: its access token and who it belongs to.
#[derive(Clone, PartialEq, Eq)]
pub struct GrokAuth {
    pub token: String,
    pub expires_at: Option<DateTime<Utc>>,
    /// The xAI subject (OIDC `sub`), stable across renewals.
    pub subject: String,
    pub email: Option<String>,
}

impl std::fmt::Debug for GrokAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokAuth")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// The CLI's `auth.json` in `home`.
pub fn auth_path(home: &Path) -> PathBuf {
    home.join("auth.json")
}

/// Parses `auth.json`: the xAI sign-in among its entries (keyed `issuer::client`).
pub fn parse_auth(text: &str) -> Option<GrokAuth> {
    let value: Value = serde_json::from_str(text).ok()?;
    let (_, entry) = value
        .as_object()?
        .iter()
        .find(|(key, _)| key.starts_with(&format!("{ISSUER}::")))?;
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(GrokAuth {
        token: text("key").filter(|t| !t.is_empty() && !t.chars().any(char::is_whitespace))?,
        expires_at: text("expires_at")
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&Utc)),
        subject: text("user_id").filter(|s| !s.trim().is_empty())?,
        email: text("email"),
    })
}

/// The sign-in in `home`, if any.
pub fn read_auth(home: &Path) -> Option<GrokAuth> {
    parse_auth(&std::fs::read_to_string(auth_path(home)).ok()?)
}

/// Renews a home's sign-in through the Grok CLI. A seam for tests.
pub trait GrokCli: Send + Sync {
    /// Asks the CLI in `home` to refresh its token; true when it ran.
    fn renew(&self, home: &Path) -> bool;
}

/// Runs `grok models` for the home, which refreshes an expiring token without any model call.
pub struct SystemGrokCli;

impl GrokCli for SystemGrokCli {
    fn renew(&self, home: &Path) -> bool {
        let mut command = crate::platform::command(cli());
        command
            .arg("models")
            .env("GROK_HOME", home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let Ok(mut process) = crate::contained::spawn(&mut command) else {
            return false;
        };
        let deadline = Instant::now() + StdDuration::from_secs(30);
        loop {
            match process.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(StdDuration::from_millis(100)),
                _ => {
                    let _ = process.child.kill();
                    return false;
                }
            }
        }
    }
}

/// The Grok CLI's own sign-in (`grok login`) for a home, in a console window of its own inside a kill-on-close job.
pub struct LoginWindow(crate::contained::Contained);

impl LoginWindow {
    pub fn open(home: &Path) -> std::io::Result<Self> {
        let mut command = crate::platform::command(cli());
        command.arg("login").env("GROK_HOME", home);
        crate::contained::spawn_console(&mut command).map(Self)
    }

    /// True once the CLI has exited.
    pub fn finished(&mut self) -> bool {
        matches!(self.0.child.try_wait(), Ok(Some(_)))
    }

    pub fn kill(&mut self) {
        let _ = self.0.child.kill();
        let _ = self.0.child.wait();
    }
}

/// The Grok CLI: where its installer puts it, or on PATH.
pub fn cli() -> String {
    let installed = crate::platform::home_dir()
        .and_then(|home| crate::platform::program_in(&home.join(".grok").join("bin"), "grok"))
        .or_else(|| crate::platform::find_program("grok"));
    match installed {
        Some(path) => path.to_string_lossy().into_owned(),
        None => "grok".to_owned(),
    }
}

/// When each home last asked the CLI to renew; process-wide, as providers are rebuilt every refresh.
static RENEWED: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);

fn may_renew(home: &Path) -> bool {
    let mut renewed = RENEWED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = renewed.get_or_insert_with(HashMap::new);
    match map.get(home) {
        Some(at) if at.elapsed() < RENEW_EVERY => false,
        _ => {
            map.insert(home.to_owned(), Instant::now());
            true
        }
    }
}

/// What the billing proxy says about the consumer pool, as metrics and messages.
#[derive(Debug, Default, PartialEq)]
pub struct GrokUsage {
    pub plan: Option<String>,
    pub metrics: Vec<Metric>,
    pub messages: Vec<String>,
}

fn instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// USD cents in xAI's `{"val": n}` shape. An empty object is zero (proto3 omits zero values); missing or null is no
/// value, and anything else is malformed.
fn cents(value: Option<&Value>) -> Result<Option<i64>, ()> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) if object.is_empty() => Ok(Some(0)),
        Some(Value::Object(object)) => match object.get("val") {
            Some(Value::Number(number)) => number.as_i64().map(Some).ok_or(()),
            Some(Value::String(text)) => text.parse().map(Some).map_err(|_| ()),
            _ => Err(()),
        },
        Some(_) => Err(()),
    }
}

/// A billing response in a shape CodexBar doesn't know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownBilling;

/// Parses the billing proxy's response. `plan` is the account's verified plan name, when the settings said one.
pub fn parse_billing(body: &str, plan: Option<&str>, now: DateTime<Utc>) -> Result<GrokUsage, UnknownBilling> {
    let value: Value = serde_json::from_str(body).map_err(|_| UnknownBilling)?;
    let config = value.get("config").and_then(Value::as_object).ok_or(UnknownBilling)?;
    let mut usage = GrokUsage {
        plan: plan.filter(|plan| !plan.trim().is_empty()).map(str::to_owned),
        ..GrokUsage::default()
    };

    let unified = config.get("isUnifiedBillingUser").and_then(Value::as_bool) == Some(true);
    if !unified {
        usage
            .messages
            .push("This xAI account has no unified Grok billing, so there is no weekly pool to show.".to_owned());
        return Ok(usage);
    }
    let period = config.get("currentPeriod");
    let kind = period.and_then(|p| p.get("type")).and_then(Value::as_str);
    let start = instant(period.and_then(|p| p.get("start")));
    let end = instant(period.and_then(|p| p.get("end")));
    let weekly = match (kind, start, end) {
        (Some("USAGE_PERIOD_TYPE_WEEKLY"), Some(start), Some(end)) if start <= now && now < end => Some((start, end)),
        (Some("USAGE_PERIOD_TYPE_MONTHLY"), ..) => {
            usage
                .messages
                .push("Grok reports a monthly period, which isn't the consumer weekly pool.".to_owned());
            None
        }
        _ => {
            usage
                .messages
                .push("Grok didn't report a current weekly period.".to_owned());
            None
        }
    };
    if let Some((start, end)) = weekly {
        let alternatives = ["productUsage", "used"]
            .iter()
            .any(|key| config.get(*key).is_some_and(|v| !v.is_null()));
        match config.get("creditUsagePercent") {
            Some(Value::Number(number)) => match number.as_f64().filter(|p| p.is_finite() && *p >= 0.0) {
                Some(percent) => {
                    let used = percent / 100.0;
                    usage.metrics.push(Metric::Window {
                        label: "Weekly usage".into(),
                        used,
                        resets_at: end,
                        pace: elapsed_pace(used, start, end, now),
                        length: (start < end).then(|| end - start),
                    });
                }
                None => usage.messages.push("Grok's weekly usage is malformed.".to_owned()),
            },
            // The CLI's own display treats an omitted percent as zero; CodexBar does so only for a verified paid plan
            // with no other usage field, and says so.
            None if !alternatives && plan.is_some_and(|plan| PAID_PLANS.contains(&plan)) => {
                usage.metrics.push(Metric::Window {
                    label: "Weekly usage".into(),
                    used: 0.0,
                    resets_at: end,
                    pace: None,
                    length: (start < end).then(|| end - start),
                });
                usage.messages.push(
                    "No included usage reported by Grok: 0% follows the Grok CLI's convention, not a measurement."
                        .to_owned(),
                );
            }
            None => usage.messages.push("Grok didn't report this week's usage.".to_owned()),
            Some(Value::Null) => usage.messages.push("Grok reported no weekly usage figure.".to_owned()),
            Some(_) => usage.messages.push("Grok's weekly usage is malformed.".to_owned()),
        }
        if let Some(products) = config.get("productUsage").and_then(Value::as_array) {
            let shares: Vec<String> = products
                .iter()
                .filter_map(|p| {
                    let name = p.get("product").and_then(Value::as_str)?;
                    // The Grok CLI's own billing fixture names it `usagePercent`; `percent` is read as well.
                    let share = p
                        .get("usagePercent")
                        .or_else(|| p.get("percent"))
                        .and_then(Value::as_f64)?;
                    Some(format!("{name} {share:.0}%"))
                })
                .collect();
            if !shares.is_empty() {
                usage.messages.push(format!("By product: {}.", shares.join(", ")));
            }
        }
    }
    // Extra Usage Credits are their own balance, never part of the weekly pool.
    match cents(config.get("prepaidBalance")) {
        Ok(Some(balance)) => usage.metrics.push(Metric::Balance {
            label: "Extra usage credits".into(),
            remaining: Money::from_cents(balance),
            burn_per_day: None,
        }),
        Ok(None) => {}
        Err(()) => usage
            .messages
            .push("Grok's Extra Usage Credits are malformed.".to_owned()),
    }
    if let Some(plan) = usage.plan.as_deref() {
        usage.messages.push(format!("Plan: {plan}."));
    }
    Ok(usage)
}

/// The plan name in the CLI settings, when it says one.
pub fn parse_plan(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    [
        "subscription_tier_display",
        "subscriptionTierDisplay",
        "subscription_tier",
        "subscriptionTier",
    ]
    .iter()
    .find_map(|key| value.get(*key)?.as_str().map(str::trim).filter(|v| !v.is_empty()))
    .map(str::to_owned)
}

/// One Grok account: its own CLI home, and the xAI subject it was signed in as.
pub struct GrokProvider<H: HttpClient> {
    http: H,
    cli: Arc<dyn GrokCli>,
    home: PathBuf,
    subject: Option<String>,
    account: String,
    label: String,
    /// Set when an X Subscription account (#172) reads the same Grok CLI home and shows its usage instead.
    shown_on_x_subscription: bool,
}

impl<H: HttpClient> GrokProvider<H> {
    pub fn new(
        http: H,
        cli: Arc<dyn GrokCli>,
        home: PathBuf,
        subject: Option<String>,
        account: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            http,
            cli,
            home,
            subject,
            account: account.into(),
            label: label.into(),
            shown_on_x_subscription: false,
        }
    }

    /// Points the card at the X Subscription account that reads the same sign-in, instead of a second copy of its
    /// usage (#172).
    pub fn shown_on_x_subscription(mut self) -> Self {
        self.shown_on_x_subscription = true;
        self
    }

    fn auth(&self) -> Result<GrokAuth, ProviderError> {
        let auth = read_auth(&self.home).ok_or(ProviderError::NotSignedIn { hint: SIGN_IN_HINT })?;
        if self.subject.as_deref().is_some_and(|subject| subject != auth.subject) {
            return Err(ProviderError::Refused { reason: OTHER_ACCOUNT });
        }
        Ok(auth)
    }

    fn get(&self, url: &str, auth: &GrokAuth) -> Result<crate::HttpResponse, ProviderError> {
        let bearer = format!("Bearer {}", auth.token);
        self.http.get(
            url,
            &[
                ("Authorization", bearer.as_str()),
                ("x-xai-token-auth", "xai-grok-cli"),
                ("Accept", "application/json"),
            ],
        )
    }

    /// The weekly pool, Extra Usage Credits and plan, read with the CLI's sign-in and renewed through the CLI when
    /// needed.
    pub fn usage(&self, now: DateTime<Utc>) -> Result<GrokUsage, ProviderError> {
        let mut auth = self.auth()?;
        if auth.expires_at.is_some_and(|at| at - now <= RENEW_MARGIN) && may_renew(&self.home) {
            self.cli.renew(&self.home);
            auth = self.auth()?;
        }
        let mut billing = self.get(BILLING, &auth)?;
        // A refused token, 401 or 403, is renewed once through the CLI before it counts as expired.
        if matches!(billing.status, 401 | 403) && may_renew(&self.home) {
            self.cli.renew(&self.home);
            auth = self.auth()?;
            billing = self.get(BILLING, &auth)?;
        }
        match billing.status {
            200 => {}
            401 | 403 => return Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
            status => return Err(ProviderError::Http { status }),
        }
        // The plan is optional; a failed lookup only means the omitted-usage convention doesn't apply.
        let plan = self
            .get(SETTINGS, &auth)
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| parse_plan(&r.body));
        parse_billing(&billing.body, plan.as_deref(), now).map_err(|UnknownBilling| ProviderError::Unexpected {
            detail: "Grok billing in an unknown shape",
        })
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let usage = if self.shown_on_x_subscription {
            GrokUsage {
                plan: None,
                metrics: Vec::new(),
                messages: vec!["Shown on the X Subscription card, which reads the same Grok sign-in.".to_owned()],
            }
        } else {
            self.usage(now)?
        };
        let mut account =
            AccountSnapshot::new(AccountId::new(self.account.clone()), Provider::Grok, usage.metrics, now);
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        if let Some(plan) = usage.plan {
            account = account.with_plan(plan);
        }
        Ok(usage.messages.into_iter().fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> UsageProvider for GrokProvider<H> {
    fn name(&self) -> &'static str {
        "Grok"
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

/// Reads the OIDC `sub` the token belongs to from xAI's userinfo endpoint (found through discovery), to confirm a new
/// sign-in before it is kept.
pub fn userinfo_subject<H: HttpClient>(http: &H, token: &str) -> Result<String, ProviderError> {
    let discovery = http.get(
        &format!("{ISSUER}/.well-known/openid-configuration"),
        &[("Accept", "application/json")],
    )?;
    let endpoint = serde_json::from_str::<Value>(&discovery.body)
        .ok()
        .and_then(|v| v.get("userinfo_endpoint")?.as_str().map(str::to_owned))
        .filter(|url| url.starts_with("https://") && url.contains(".x.ai/"))
        .ok_or(ProviderError::Unexpected {
            detail: "xAI sign-in discovery without a userinfo endpoint",
        })?;
    let bearer = format!("Bearer {token}");
    let response = http.get(
        &endpoint,
        &[("Authorization", bearer.as_str()), ("Accept", "application/json")],
    )?;
    match response.status {
        200 => serde_json::from_str::<Value>(&response.body)
            .ok()
            .and_then(|v| v.get("sub")?.as_str().map(str::to_owned))
            .filter(|sub| !sub.trim().is_empty())
            .ok_or(ProviderError::Unexpected {
                detail: "xAI userinfo without a subject",
            }),
        401 | 403 => Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
        status => Err(ProviderError::Http { status }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-12T12:00:00Z".parse().unwrap()
    }

    fn billing(extra: &str) -> String {
        format!(
            r#"{{"config":{{"currentPeriod":{{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-10-08T16:32:13+00:00",
            "end":"2026-10-15T16:32:13+00:00"}},"isUnifiedBillingUser":true{extra}}}}}"#
        )
    }

    #[test]
    fn a_reported_weekly_percent_and_credits_show_apart() {
        let body = billing(r#","creditUsagePercent":37.5,"prepaidBalance":{"val":1250}"#);
        let usage = parse_billing(&body, Some("SuperGrok"), now()).unwrap();
        assert_eq!(usage.metrics[0].label(), "Weekly usage");
        assert_eq!(usage.metrics[0].used_fraction(), Some(0.375));
        assert_eq!(usage.metrics[0].resets_at(), "2026-10-15T16:32:13Z".parse().ok());
        assert_eq!(usage.metrics[1].used_display(), "$12.50 left");
        assert_eq!(usage.messages, ["Plan: SuperGrok."]);
    }

    #[test]
    fn omitted_usage_is_zero_only_for_a_verified_paid_plan_with_nothing_else() {
        // Franz's live shape on 2026-10-12: no percent, a zero prepaid balance, plan SuperGrok.
        let body = billing(r#","prepaidBalance":{"val":0},"onDemandUsed":{"val":0}"#);
        let usage = parse_billing(&body, Some("SuperGrok"), now()).unwrap();
        assert_eq!(usage.metrics[0].used_fraction(), Some(0.0));
        assert!(usage.messages[0].starts_with("No included usage reported by Grok"));
        // An unknown plan, no plan, or another usage field: unavailable, never zero.
        for (plan, extra) in [
            (Some("Grok Free"), ""),
            (None, ""),
            (
                Some("SuperGrok"),
                r#","productUsage":[{"product":"PRODUCT_GROK_BUILD","usagePercent":61.2},{"product":"Chat","percent":20}]"#,
            ),
        ] {
            let usage = parse_billing(&billing(extra), plan, now()).unwrap();
            assert!(
                usage.metrics.iter().all(|m| m.label() != "Weekly usage"),
                "{plan:?} {extra}"
            );
        }
        // An explicit null is not an omission.
        let usage = parse_billing(&billing(r#","creditUsagePercent":null"#), Some("SuperGrok"), now()).unwrap();
        assert!(usage.metrics.is_empty());
        assert_eq!(usage.messages[0], "Grok reported no weekly usage figure.");
    }

    #[test]
    fn credits_distinguish_empty_missing_and_malformed() {
        let empty = parse_billing(&billing(r#","creditUsagePercent":1,"prepaidBalance":{}"#), None, now()).unwrap();
        assert_eq!(empty.metrics[1].used_display(), "$0.00 left", "an empty object is zero");
        let missing = parse_billing(&billing(r#","creditUsagePercent":1"#), None, now()).unwrap();
        assert_eq!(missing.metrics.len(), 1);
        let broken = parse_billing(
            &billing(r#","creditUsagePercent":1,"prepaidBalance":{"val":"x"}"#),
            None,
            now(),
        )
        .unwrap();
        assert!(
            broken
                .messages
                .contains(&"Grok's Extra Usage Credits are malformed.".to_owned())
        );
    }

    #[test]
    fn other_ledgers_and_periods_never_become_the_weekly_meter() {
        let monthly = r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-10-01T00:00:00Z",
            "end":"2026-11-01T00:00:00Z"},"isUnifiedBillingUser":true,"creditUsagePercent":40}}"#;
        let usage = parse_billing(monthly, Some("SuperGrok"), now()).unwrap();
        assert!(usage.metrics.iter().all(|m| m.label() != "Weekly usage"));
        let ended = billing(r#","creditUsagePercent":40"#).replace("2026-10-15T16:32:13", "2026-10-10T16:32:13");
        assert!(
            parse_billing(&ended, None, now()).unwrap().metrics.is_empty(),
            "an ended week isn't current"
        );
        let api_only =
            r#"{"config":{"isUnifiedBillingUser":false,"creditUsagePercent":40,"prepaidBalance":{"val":900}}}"#;
        let usage = parse_billing(api_only, None, now()).unwrap();
        assert!(
            usage.metrics.is_empty(),
            "no consumer meter or balance without unified billing"
        );
        assert_eq!(parse_billing("{}", None, now()), Err(UnknownBilling));
    }

    #[test]
    fn a_non_unified_account_preserves_its_reported_plan_without_inventing_a_pool() {
        let body = r#"{"config":{"isUnifiedBillingUser":false}}"#;
        let usage = parse_billing(body, Some("SuperGrok Lite"), now()).unwrap();
        assert_eq!(usage.plan.as_deref(), Some("SuperGrok Lite"));
        assert!(usage.metrics.is_empty());
        assert!(usage.messages.iter().any(|m| m.contains("no weekly pool")));
    }

    #[test]
    fn the_plan_reads_from_either_key_spelling() {
        assert_eq!(
            parse_plan(r#"{"subscription_tier_display":"SuperGrok"}"#).as_deref(),
            Some("SuperGrok")
        );
        assert_eq!(
            parse_plan(r#"{"subscriptionTierDisplay":"SuperGrok Lite"}"#).as_deref(),
            Some("SuperGrok Lite")
        );
        assert_eq!(parse_plan("{}"), None);
    }

    const AUTH: &str = r#"{"https://auth.x.ai::b1a00492":{"key":"tok-1","expires_at":"2026-10-12T18:00:00Z",
        "user_id":"sub-1","email":"someone@example.com","refresh_token":"r"}}"#;

    #[test]
    fn the_cli_sign_in_is_read_and_never_printed() {
        let auth = parse_auth(AUTH).unwrap();
        assert!(auth.token == "tok-1" && auth.subject == "sub-1");
        assert!(!format!("{auth:?}").contains("tok-1"));
        assert_eq!(parse_auth(r#"{"https://other::x":{"key":"t","user_id":"s"}}"#), None);
    }

    /// A home folder with an `auth.json`, removed when dropped.
    struct Home(PathBuf);

    impl Home {
        fn with(name: &str, auth: &str) -> Self {
            let path = std::env::temp_dir().join(format!("codexbar-grok-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("auth.json"), auth).unwrap();
            Self(path)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Proxy(Mutex<Vec<HttpResponse>>, Mutex<Vec<String>>);

    impl HttpClient for Proxy {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            assert!(headers.contains(&("x-xai-token-auth", "xai-grok-cli")));
            self.1.lock().unwrap().push(url.to_owned());
            let mut script = self.0.lock().unwrap();
            if script.is_empty() {
                return Err(ProviderError::Network);
            }
            Ok(script.remove(0))
        }
    }

    /// Writes a renewed sign-in when asked, counting the calls.
    struct Renewer(PathBuf, AtomicUsize);

    impl GrokCli for Renewer {
        fn renew(&self, home: &Path) -> bool {
            assert_eq!(home, self.0);
            self.1.fetch_add(1, Ordering::SeqCst);
            std::fs::write(
                home.join("auth.json"),
                AUTH.replace("tok-1", "tok-2").replace("18:00:00", "23:00:00"),
            )
            .unwrap();
            true
        }
    }

    #[test]
    fn a_fetch_renews_through_the_cli_when_refused_and_keeps_the_subject() {
        let home = Home::with("renew", AUTH);
        let proxy = Proxy(
            Mutex::new(vec![
                HttpResponse::new(401, "{}"),
                HttpResponse::new(200, billing(r#","creditUsagePercent":10"#)),
                HttpResponse::new(200, r#"{"subscription_tier_display":"SuperGrok"}"#),
            ]),
            Mutex::new(Vec::new()),
        );
        let renewer = Arc::new(Renewer(home.0.clone(), AtomicUsize::new(0)));
        let provider = GrokProvider::new(
            proxy,
            renewer.clone(),
            home.0.clone(),
            Some("sub-1".into()),
            "gk-1",
            "Me",
        );
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.display_name(), "Grok · Me · SuperGrok");
        assert_eq!(account.metrics()[0].used_fraction(), Some(0.1));
        assert_eq!(renewer.1.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_non_unified_fetch_still_names_the_provider_reported_plan() {
        let home = Home::with("plan-without-pool", AUTH);
        let proxy = Proxy(
            Mutex::new(vec![
                HttpResponse::new(200, r#"{"config":{"isUnifiedBillingUser":false}}"#),
                HttpResponse::new(200, r#"{"subscription_tier_display":"SuperGrok Lite"}"#),
            ]),
            Mutex::new(Vec::new()),
        );
        let provider = GrokProvider::new(
            proxy,
            Arc::new(Renewer(home.0.clone(), AtomicUsize::new(0))),
            home.0.clone(),
            None,
            "gk-plan",
            "Me",
        );
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.plan(), Some("SuperGrok Lite"));
        assert!(account.metrics().is_empty());
    }

    #[test]
    fn a_grok_account_shown_on_the_x_subscription_points_there_without_a_request() {
        let home = Home::with("x-subscription", AUTH);
        let proxy = Proxy(Mutex::new(Vec::new()), Mutex::new(Vec::new()));
        let provider = GrokProvider::new(
            proxy,
            Arc::new(Renewer(home.0.clone(), AtomicUsize::new(0))),
            home.0.clone(),
            None,
            "gk-1",
            "Me",
        )
        .shown_on_x_subscription();
        let account = provider.fetch(now()).unwrap().remove(0);
        assert!(account.metrics().is_empty(), "no second copy of the weekly pool");
        assert!(account.messages()[0].starts_with("Shown on the X Subscription card"));
        assert!(provider.http.1.lock().unwrap().is_empty());
    }

    #[test]
    fn a_403_is_renewed_once_like_a_401() {
        let home = Home::with("renew-403", AUTH);
        let proxy = Proxy(
            Mutex::new(vec![
                HttpResponse::new(403, "{}"),
                HttpResponse::new(200, billing(r#","creditUsagePercent":10"#)),
                HttpResponse::new(200, r#"{"subscription_tier_display":"SuperGrok"}"#),
            ]),
            Mutex::new(Vec::new()),
        );
        let renewer = Arc::new(Renewer(home.0.clone(), AtomicUsize::new(0)));
        let provider = GrokProvider::new(
            proxy,
            renewer.clone(),
            home.0.clone(),
            Some("sub-1".into()),
            "gk-1",
            "Me",
        );
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics()[0].used_fraction(), Some(0.1));
        assert_eq!(renewer.1.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn product_shares_read_the_clis_usage_percent() {
        let body = billing(
            r#","creditUsagePercent":30,"productUsage":[{"product":"PRODUCT_GROK_BUILD","usagePercent":61.2},
                {"product":"Chat","percent":20},{"product":"Broken"}]"#,
        );
        let usage = parse_billing(&body, Some("SuperGrok"), now()).unwrap();
        assert!(
            usage
                .messages
                .contains(&"By product: PRODUCT_GROK_BUILD 61%, Chat 20%.".to_owned()),
            "{:?}",
            usage.messages
        );
    }

    #[test]
    fn another_xai_account_in_the_home_or_none_is_refused() {
        let home = Home::with("other", AUTH);
        let provider = GrokProvider::new(
            Proxy(Mutex::new(Vec::new()), Mutex::new(Vec::new())),
            Arc::new(Renewer(home.0.clone(), AtomicUsize::new(0))),
            home.0.clone(),
            Some("sub-9".into()),
            "gk-1",
            "Me",
        );
        assert_eq!(
            provider.fetch(now()).unwrap_err(),
            ProviderError::Refused { reason: OTHER_ACCOUNT }
        );
        let empty = std::env::temp_dir().join("codexbar-grok-nothing-here");
        let provider = GrokProvider::new(
            Proxy(Mutex::new(Vec::new()), Mutex::new(Vec::new())),
            Arc::new(Renewer(empty.clone(), AtomicUsize::new(0))),
            empty,
            None,
            "gk-1",
            "Me",
        );
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
    }

    #[test]
    fn userinfo_is_found_through_discovery() {
        let proxy = Proxy(
            Mutex::new(vec![
                HttpResponse::new(200, r#"{"userinfo_endpoint":"https://auth.x.ai/oauth2/userinfo"}"#),
                HttpResponse::new(200, r#"{"sub":"sub-1","email":"someone@example.com"}"#),
            ]),
            Mutex::new(Vec::new()),
        );
        // Discovery and userinfo don't carry the CLI header; this fake checks it, so use one that doesn't.
        struct Plain(Proxy);
        impl HttpClient for Plain {
            fn get(&self, url: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
                self.0.get(url, &[("x-xai-token-auth", "xai-grok-cli")])
            }
        }
        assert_eq!(userinfo_subject(&Plain(proxy), "tok").unwrap(), "sub-1");
        let evil = Plain(Proxy(
            Mutex::new(vec![HttpResponse::new(
                200,
                r#"{"userinfo_endpoint":"https://evil.example/userinfo"}"#,
            )]),
            Mutex::new(Vec::new()),
        ));
        assert!(matches!(
            userinfo_subject(&evil, "tok"),
            Err(ProviderError::Unexpected { .. })
        ));
    }
}
