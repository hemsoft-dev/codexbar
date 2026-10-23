//! ChatGPT / Codex subscription limits, read with the ChatGPT sign-in the Codex CLI keeps in a Codex home's
//! `auth.json`: the CLI's own `~/.codex`, or a home CodexBar signed in through the Codex CLI (#78). Codex writes the
//! file; this module only reads it, and asks the Codex CLI to renew it shortly before it expires.

mod subscription;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, TimeZone, Utc};
use codexbar_core::format::{DateStyle, English};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Pace, Provider};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::codex_app_server::{AppServer, AppServerError};
use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const SIGN_IN_HINT: &str = "Run `codex` and sign in with ChatGPT.";
/// The hint for a Codex home CodexBar signed in (#78).
pub const MANAGED_SIGN_IN_HINT: &str = "Sign in again in Settings > Accounts.";
/// The account of a sign-in whose user can't be read (no ID token).
const LEGACY_ID: &str = "codex-chatgpt";
/// A sign-in is renewed when its access token expires within this long.
const RENEW_AHEAD: Duration = Duration::hours(24);
/// At most one renewal is tried per home in this long, so a failing one isn't retried on every refresh.
const RENEW_BACKOFF: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// The parts of the Codex sign-in this provider uses.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    access_token: String,
    account_id: Option<String>,
    id_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field("account_id", &self.account_id.as_ref().map(|_| "<present>"))
            .field("id_token", &self.id_token.as_ref().map(|_| "<present>"))
            .finish()
    }
}

impl Credentials {
    /// The dashboard account this sign-in belongs to: `codex-` and a short hash of the ChatGPT user and workspace, so
    /// each identity keeps its own history, label and alerts, and signing a home in to another account never inherits
    /// them (#78). A sign-in without a readable ID token keeps the legacy id.
    pub fn account(&self) -> AccountId {
        let Some(user) = self
            .claims()
            .and_then(|claims| crate::jwt::string_claim(&claims, "sub"))
        else {
            return AccountId::new(LEGACY_ID);
        };
        let workspace = self.account_id.as_deref().unwrap_or_default();
        let digest = Sha256::digest(format!("{user}\n{workspace}").as_bytes());
        let short: String = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect();
        AccountId::new(format!("codex-{short}"))
    }

    /// The signed-in email, from the ID token.
    pub fn email(&self) -> Option<String> {
        crate::jwt::string_claim(&self.claims()?, "email")
    }

    fn claims(&self) -> Option<Value> {
        crate::jwt::claims(self.id_token.as_deref()?)
    }

    fn expires_at(&self) -> Option<DateTime<Utc>> {
        crate::jwt::expires_at(&self.access_token)
    }
}

/// The account the sign-in in `path` belongs to, when there is one.
pub fn signed_in_account(path: &Path) -> Option<AccountId> {
    read_credentials(path).ok().map(|credentials| credentials.account())
}

/// The email the sign-in in `path` belongs to, when it is known.
pub fn signed_in_email(path: &Path) -> Option<String> {
    read_credentials(path).ok()?.email()
}

/// `$CODEX_HOME/auth.json`, or `~/.codex/auth.json`.
pub fn default_auth_path() -> PathBuf {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::platform::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"));
    home.join("auth.json")
}

/// Reads the access token (and account id, when present) from a Codex `auth.json`.
pub fn read_credentials(path: &Path) -> Result<Credentials, ProviderError> {
    let not_signed_in = ProviderError::NotSignedIn { hint: SIGN_IN_HINT };
    let text = std::fs::read_to_string(path).map_err(|_| not_signed_in.clone())?;
    let json: Value = serde_json::from_str(&text).map_err(|_| not_signed_in.clone())?;
    let tokens = json.get("tokens").ok_or_else(|| not_signed_in.clone())?;
    let token = |snake: &str, camel: &str| {
        tokens
            .get(snake)
            .or_else(|| tokens.get(camel))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let access_token = token("access_token", "accessToken").ok_or(not_signed_in)?;
    Ok(Credentials {
        access_token,
        account_id: token("account_id", "accountId"),
        id_token: token("id_token", "idToken"),
    })
}

/// Maps a `wham/usage` response onto one account. Windows are ordered shortest first, so the 5-hour window is primary.
pub fn parse_usage(payload: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let rate_limit = json.get("rate_limit").ok_or(ProviderError::Unexpected {
        detail: "no rate limits",
    })?;

    let mut windows: Vec<Window> = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| rate_limit.get(*key).and_then(Window::from_json))
        .collect();
    if windows.is_empty() {
        return Err(ProviderError::Unexpected {
            detail: "no usage windows for this account",
        });
    }
    windows.sort_by_key(|window| window.duration_secs);

    let mut metrics: Vec<Metric> = windows.iter().map(|window| window.to_metric(now)).collect();
    metrics.extend(json.get("credits").and_then(parse_credits));
    let mut account =
        AccountSnapshot::new(AccountId::new("codex-chatgpt"), Provider::Codex, metrics, now).with_credits_placed();
    if let Some(plan) = json
        .get("plan_type")
        .and_then(Value::as_str)
        .filter(|plan| !plan.is_empty())
    {
        account = account.with_plan(codex_plan_name(plan));
        if let Some(price) = codexbar_core::sus::known_price(Provider::Codex, &plan.to_ascii_lowercase()) {
            account = account.with_subscription_price(price);
        }
    }
    Ok(account)
}

/// The account's credits from `wham/usage`'s optional `credits` object (#150): `unlimited`, or a finite, non-negative
/// `balance` sent as a decimal string or a number. Anything else (missing, null, malformed, negative) is no metric,
/// never a zero; `has_credits` alone never supplies a number. Like the Codex CLI's own status view, `unlimited` wins;
/// otherwise `has_credits: false` means no usable credits whatever the balance says, so a stale balance can't carry a
/// used-up window.
fn parse_credits(credits: &Value) -> Option<Metric> {
    let balance = if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
        None
    } else if credits.get("has_credits").and_then(Value::as_bool) == Some(false) {
        return None;
    } else {
        let balance = match credits.get("balance")? {
            Value::String(text) => text.trim().parse::<f64>().ok()?,
            Value::Number(number) => number.as_f64()?,
            _ => return None,
        };
        if !(balance.is_finite() && balance >= 0.0) {
            return None;
        }
        Some(balance)
    };
    Some(Metric::Credits {
        label: "Credits".to_owned(),
        balance,
        start: None,
    })
}

/// Explicit plan-code names. The live account API display name takes priority.
fn codex_plan_name(plan: &str) -> String {
    match plan.to_ascii_lowercase().as_str() {
        "pro" => "Pro".to_owned(),
        "prolite" => "Pro Lite".to_owned(),
        "promax" => "Pro Max".to_owned(),
        "hc" | "ent26" | "enterprise" => "Enterprise".to_owned(),
        "enterprise_cbp_automation" => "Enterprise (Automation)".to_owned(),
        "edu" | "education" => "Edu".to_owned(),
        "sci" => "Academic Researchers".to_owned(),
        "sci_plus" => "Academic Researchers Plus".to_owned(),
        _ => plan_name(plan),
    }
}

/// "pro" -> "Pro", "prolite" -> "Pro Lite", "team_plus" -> "Team Plus".
pub fn plan_name(plan: &str) -> String {
    match plan.to_lowercase().as_str() {
        "prolite" => "Pro Lite".to_owned(),
        other => other
            .split('_')
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

struct Window {
    used: f64,
    resets_at: DateTime<Utc>,
    duration_secs: i64,
}

impl Window {
    fn from_json(value: &Value) -> Option<Self> {
        let used = value.get("used_percent")?.as_f64()?;
        let resets_at = Utc.timestamp_opt(value.get("reset_at")?.as_i64()?, 0).single()?;
        // An implausible length, even one too large to be a duration, drops the window rather than panicking (#226).
        let duration_secs = value
            .get("limit_window_seconds")?
            .as_i64()
            .filter(|secs| codexbar_core::window_length_from_seconds(*secs).is_some())?;
        Some(Self {
            used: (used / 100.0).clamp(0.0, 1.0),
            resets_at,
            duration_secs,
        })
    }

    fn label(&self) -> String {
        match self.duration_secs {
            18_000 => "5-hour window".to_owned(),
            604_800 => "Weekly".to_owned(),
            secs => format!("{}-hour window", (secs / 3600).max(1)),
        }
    }

    fn pace(&self, now: DateTime<Utc>) -> Option<Pace> {
        elapsed_pace(
            self.used,
            self.resets_at - Duration::seconds(self.duration_secs),
            self.resets_at,
            now,
        )
    }

    fn to_metric(&self, now: DateTime<Utc>) -> Metric {
        Metric::Window {
            label: self.label(),
            used: self.used,
            resets_at: self.resets_at,
            pace: self.pace(now),
            length: Some(Duration::seconds(self.duration_secs)),
        }
    }
}

/// Asks the Codex CLI to renew a Codex home's sign-in. A seam, so tests never start Codex.
pub trait Renewer: Send + Sync {
    fn renew(&self, home: &Path) -> Result<(), AppServerError>;
}

/// Renews through `codex app-server`, which runs Codex's own token refresh and writes the home's `auth.json`.
pub struct CodexCliRenewer;

impl Renewer for CodexCliRenewer {
    fn renew(&self, home: &Path) -> Result<(), AppServerError> {
        AppServer::start(home)?.renew()
    }
}

/// When each home last had a renewal tried. Process-wide, because the dashboard builds its providers anew for every
/// refresh.
static LAST_RENEWAL: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());
/// The account each sign-in file last fetched for, so a switch between refreshes is noticed.
static LAST_ACCOUNTS: Mutex<Vec<(PathBuf, AccountId)>> = Mutex::new(Vec::new());

/// The Codex provider for one Codex home, generic over HTTP so tests never touch the network.
pub struct CodexProvider<H: HttpClient> {
    http: H,
    auth_path: PathBuf,
    /// The dashboard account and name this adapter reports, when it serves one configured account of several.
    account: Option<(String, String)>,
    record_id: Option<String>,
    hint: &'static str,
    renewer: Option<Arc<dyn Renewer>>,
    date_style: &'static dyn DateStyle,
}

impl<H: HttpClient> CodexProvider<H> {
    pub fn new(http: H, auth_path: PathBuf) -> Self {
        Self {
            http,
            auth_path,
            account: None,
            record_id: None,
            hint: SIGN_IN_HINT,
            renewer: None,
            date_style: &English,
        }
    }

    /// Records the configured source without changing the credential-derived dashboard identity.
    pub fn with_subscription_record(mut self, id: impl Into<String>) -> Self {
        self.record_id = Some(id.into());
        self
    }

    pub fn with_account(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.account = Some((id.into(), label.into()));
        self
    }

    /// A home CodexBar signed in: signing in again happens in Settings, not with `codex`.
    pub fn managed(mut self) -> Self {
        self.hint = MANAGED_SIGN_IN_HINT;
        self
    }

    /// Renews sign-ins that are about to expire, or were refused, through `renewer`.
    pub fn with_renewer(mut self, renewer: Arc<dyn Renewer>) -> Self {
        self.renewer = Some(renewer);
        self
    }

    /// The app's date formatter, so account metadata follows its locale contract.
    pub fn with_date_style(mut self, style: &'static dyn DateStyle) -> Self {
        self.date_style = style;
        self
    }

    fn read(&self) -> Result<Credentials, ProviderError> {
        let not_signed_in = ProviderError::NotSignedIn { hint: self.hint };
        match read_credentials(&self.auth_path) {
            Ok(credentials) => Ok(credentials),
            // Codex may be replacing the file right now; one more read after a moment sees the finished file.
            Err(_) if self.auth_path.is_file() => {
                std::thread::sleep(std::time::Duration::from_millis(150));
                read_credentials(&self.auth_path).map_err(|_| not_signed_in)
            }
            Err(_) => Err(not_signed_in),
        }
    }

    /// Tries a renewal, at most once per home in [`RENEW_BACKOFF`]. True when Codex renewed the sign-in.
    fn renew(&self) -> bool {
        let (Some(renewer), Some(home)) = (&self.renewer, self.auth_path.parent()) else {
            return false;
        };
        {
            let mut last = LAST_RENEWAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            match last.iter_mut().find(|(path, _)| *path == self.auth_path) {
                Some((_, at)) if now.duration_since(*at) < RENEW_BACKOFF => return false,
                Some((_, at)) => *at = now,
                None => last.push((self.auth_path.clone(), now)),
            }
        }
        renewer.renew(home).is_ok()
    }

    fn usage(&self, credentials: &Credentials, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let bearer = format!("Bearer {}", credentials.access_token);
        let mut headers = vec![("Authorization", bearer.as_str()), ("Accept", "application/json")];
        if let Some(account_id) = credentials.account_id.as_deref() {
            headers.push(("ChatGPT-Account-Id", account_id));
        }
        let response = self.http.get(USAGE_ENDPOINT, &headers)?;
        match response.status {
            200..=299 => parse_usage(&response.body, now),
            401 | 403 => Err(ProviderError::Expired { hint: self.hint }),
            status => Err(ProviderError::Http { status }),
        }
    }

    fn label_for(&self) -> Option<String> {
        self.account
            .as_ref()
            .map(|(_, label)| label.trim())
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
    }
}

impl<H: HttpClient> UsageProvider for CodexProvider<H> {
    fn name(&self) -> &'static str {
        "ChatGPT · Codex"
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

    /// No sign-in fails the adapter as a whole. A sign-in that expires within a day is renewed first, and a refused
    /// one is renewed and tried once more. Once the sign-in is read its account is known, so a failed fetch is that
    /// account's failure, never another identity's.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        let mut credentials = self.read()?;
        let mut renewed = false;
        if credentials.expires_at().is_some_and(|at| at - now < RENEW_AHEAD) && self.renew() {
            renewed = true;
            credentials = self.read()?;
        }
        let mut result = self.usage(&credentials, now);
        if matches!(result, Err(ProviderError::Expired { .. })) && !renewed && self.renew() {
            credentials = self.read()?;
            result = self.usage(&credentials, now);
        }
        let id = credentials.account();
        let outcome = match result {
            Ok(mut usage) => {
                match subscription::lookup(&self.http, &self.auth_path, &credentials, now, self.date_style) {
                    Ok(Some(plan)) => {
                        usage = usage.with_plan(plan.name).with_subscription_price(plan.price);
                    }
                    Ok(None) => {}
                    Err(message) => usage = usage.with_message(message),
                }
                let mut account = AccountSnapshot::new(id.clone(), Provider::Codex, usage.metrics().to_vec(), now)
                    .with_subscription_record(self.record_id.clone());
                if let Some(label) = self.label_for() {
                    account = account.with_label(label);
                }
                if let Some(plan) = usage.plan() {
                    account = account.with_plan(plan);
                }
                if let Some(price) = usage.subscription_price() {
                    account = account.with_subscription_price(price.clone());
                }
                for message in usage.messages() {
                    account = account.with_message(message.clone());
                }
                let mut last = LAST_ACCOUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match last.iter_mut().find(|(path, _)| *path == self.auth_path) {
                    Some((_, previous)) => {
                        if *previous != id {
                            account = account.with_message(
                                "Signed in to a different ChatGPT account. It is a new account here; the previous one's usage isn't carried over.",
                            );
                        }
                        *previous = id;
                    }
                    None => last.push((self.auth_path.clone(), id)),
                }
                AccountOutcome::Fresh(account)
            }
            Err(error) => AccountOutcome::Failed {
                account: id,
                label: self.label_for(),
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

    /// The home's sign-in; a configured account that is signed out still holds the identity it was signed in to, so
    /// its saved usage is shown rather than set aside.
    fn signed_in_account(&self) -> Option<AccountId> {
        signed_in_account(&self.auth_path).or_else(|| self.account.as_ref().map(|(id, _)| AccountId::new(id.clone())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::HttpResponse;
    use codexbar_core::Severity;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/codex-usage.json");

    struct FakeHttp {
        response: Result<HttpResponse, ProviderError>,
        seen_headers: Mutex<Vec<(String, String)>>,
    }

    impl FakeHttp {
        fn status(status: u16, body: &str) -> Self {
            Self {
                response: Ok(HttpResponse::new(status, body)),
                seen_headers: Mutex::default(),
            }
        }
    }

    impl HttpClient for FakeHttp {
        fn get(&self, _: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            *self.seen_headers.lock().unwrap() = headers.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
            self.response.clone()
        }
    }

    fn auth_file(contents: &str) -> tempfile_lite::TempFile {
        tempfile_lite::TempFile::with_contents(contents)
    }

    #[test]
    fn parse_usage_fixture_orders_windows_and_projects_pace() {
        let account = parse_usage(FIXTURE, now()).unwrap();
        assert_eq!(account.display_name(), "ChatGPT · Codex (Pro)");
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, ["5-hour window", "Weekly"]);
        assert_eq!(account.primary().unwrap().used_fraction(), Some(0.91));
        // 91% used, 4h 22m into the window: about 21% per hour, so the limit lands in ~26 minutes, before the reset.
        assert_eq!(account.assess(now()).severity(), Severity::LimitSoon);
    }

    /// A weekly window `used` percent full, with `credits` as the raw JSON of the `credits` field (or none).
    fn with_credits(used: u32, credits: Option<&str>) -> AccountSnapshot {
        let credits = credits
            .map(|credits| format!(r#","credits":{credits}"#))
            .unwrap_or_default();
        let payload = format!(
            r#"{{"rate_limit":{{"secondary_window":{{"used_percent":{used},"reset_at":1893456000,"limit_window_seconds":604800}}}}{credits}}}"#
        );
        parse_usage(&payload, now()).unwrap()
    }

    fn credits_of(account: &AccountSnapshot) -> Option<Option<f64>> {
        account.metrics().iter().find_map(|metric| match metric {
            Metric::Credits { balance, .. } => Some(*balance),
            _ => None,
        })
    }

    #[test]
    fn parse_usage_reads_credit_balances_as_counts() {
        let text = with_credits(40, Some(r#"{"has_credits":true,"unlimited":false,"balance":"62500"}"#));
        assert_eq!(credits_of(&text), Some(Some(62_500.0)));
        assert_eq!(
            text.primary().unwrap().label(),
            "Weekly",
            "an open window stays the headline"
        );
        let credits = text.metrics().last().unwrap();
        assert_eq!(credits.used_display(), "62,500 left");
        assert_eq!(credits.used_fraction(), None, "never a percentage");
        let number = with_credits(40, Some(r#"{"has_credits":true,"balance":1234.5}"#));
        assert_eq!(credits_of(&number), Some(Some(1234.5)));
        let zero = with_credits(40, Some(r#"{"has_credits":true,"balance":"0"}"#));
        assert_eq!(credits_of(&zero), Some(Some(0.0)), "zero is a real balance");
        let unavailable = with_credits(100, Some(r#"{"has_credits":false,"balance":"5000"}"#));
        assert_eq!(
            credits_of(&unavailable),
            None,
            "has_credits: false wins over a stale balance"
        );
        assert_eq!(
            unavailable.primary().unwrap().label(),
            "Weekly",
            "the used-up window stays the headline"
        );
    }

    #[test]
    fn parse_usage_unlimited_credits_have_no_number() {
        let account = with_credits(100, Some(r#"{"has_credits":true,"unlimited":true,"balance":null}"#));
        assert_eq!(credits_of(&account), Some(None));
        assert_eq!(account.primary().unwrap().used_display(), "Unlimited");
        let flagged = with_credits(100, Some(r#"{"has_credits":false,"unlimited":true}"#));
        assert_eq!(
            credits_of(&flagged),
            Some(None),
            "unlimited wins over has_credits: false"
        );
        assert_eq!(account.primary().unwrap().progress(), None, "no bar");
    }

    #[test]
    fn parse_usage_unusable_credits_are_skipped_never_zero() {
        for credits in [
            None,
            Some("null"),
            Some(r#"{"has_credits":true}"#),
            Some(r#"{"has_credits":true,"balance":null}"#),
            Some(r#"{"balance":"lots"}"#),
            Some(r#"{"balance":"-5"}"#),
            Some(r#"{"balance":-5}"#),
            Some(r#"{"balance":"NaN"}"#),
            Some(r#"{"balance":"inf"}"#),
            Some(r#"{"balance":true}"#),
            Some(r#""1000""#),
        ] {
            let account = with_credits(30, credits);
            assert_eq!(credits_of(&account), None, "{credits:?}");
            assert_eq!(account.metrics().len(), 1, "{credits:?}: the window still refreshes");
        }
    }

    #[test]
    fn parse_usage_exhausted_window_with_credits_puts_credits_first() {
        let account = with_credits(100, Some(r#"{"has_credits":true,"balance":"500"}"#));
        assert!(matches!(account.primary(), Some(Metric::Credits { .. })));
        assert_eq!(account.assess(now()).severity(), Severity::Normal, "not blocked");
        let broke = with_credits(100, Some(r#"{"has_credits":true,"balance":"0"}"#));
        assert_eq!(broke.primary().unwrap().label(), "Weekly");
        assert_eq!(broke.assess(now()).severity(), Severity::LimitSoon, "blocked");
        assert_eq!(broke.assess(now()).label(), None, "but no pill");
    }

    #[test]
    fn parse_usage_bad_window_is_skipped() {
        let payload = r#"{"rate_limit":{"primary_window":{"used_percent":"bad"},
            "secondary_window":{"used_percent":22,"reset_at":1893456000,"limit_window_seconds":604800}}}"#;
        let account = parse_usage(payload, now()).unwrap();
        assert_eq!(account.metrics().len(), 1);
        assert_eq!(account.primary().unwrap().label(), "Weekly");
    }

    #[test]
    fn parse_usage_no_windows_returns_error() {
        let payload = r#"{"rate_limit":{"primary_window":null,"secondary_window":null}}"#;
        assert!(matches!(
            parse_usage(payload, now()),
            Err(ProviderError::Unexpected { .. })
        ));
    }

    #[test]
    fn parse_usage_over_limit_and_custom_duration_clamps_and_labels() {
        let payload = r#"{"rate_limit":{"primary_window":{"used_percent":150,"reset_at":1893456000,"limit_window_seconds":32400}}}"#;
        let account = parse_usage(payload, now()).unwrap();
        assert_eq!(account.primary().unwrap().label(), "9-hour window");
        assert_eq!(account.primary().unwrap().used_fraction(), Some(1.0));
    }

    #[test]
    fn parse_usage_window_just_started_has_no_pace() {
        let resets_at = (now() + Duration::hours(5) - Duration::minutes(2)).timestamp();
        let payload = format!(
            r#"{{"rate_limit":{{"primary_window":{{"used_percent":5,"reset_at":{resets_at},"limit_window_seconds":18000}}}}}}"#
        );
        let account = parse_usage(&payload, now()).unwrap();
        assert_eq!(account.primary().unwrap().time_to_limit(), None);
        // The window's length comes from `limit_window_seconds` (#223).
        assert_eq!(account.primary().unwrap().window_length(), Some(Duration::hours(5)));
    }

    #[test]
    fn an_implausible_window_length_is_dropped_without_panicking() {
        // A length too large to be a duration must not crash the parse (#226).
        let payload = r#"{"rate_limit":{"primary_window":{"used_percent":5,"reset_at":1893456000,"limit_window_seconds":9223372036854775807},
            "secondary_window":{"used_percent":22,"reset_at":1893456000,"limit_window_seconds":604800}}}"#;
        let account = parse_usage(payload, now()).unwrap();
        let lengths: Vec<_> = account.metrics().iter().map(Metric::window_length).collect();
        assert_eq!(lengths, [Some(Duration::days(7))]);
    }

    #[test]
    fn parse_usage_weekly_window_early_has_no_pace_and_stays_normal() {
        // 2% used 20 minutes into a weekly window must not project a lockout.
        let resets_at = (now() + Duration::days(7) - Duration::minutes(20)).timestamp();
        let payload = format!(
            r#"{{"rate_limit":{{"primary_window":{{"used_percent":2,"reset_at":{resets_at},"limit_window_seconds":604800}}}}}}"#
        );
        let account = parse_usage(&payload, now()).unwrap();
        assert_eq!(account.primary().unwrap().time_to_limit(), None);
        assert_eq!(account.assess(now()).severity(), Severity::Normal);
    }

    #[test]
    fn a_codex_plan_transition_refreshes_without_changing_identity_or_the_account_name() {
        struct Plans(Mutex<Vec<String>>);
        impl HttpClient for Plans {
            fn get(&self, url: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
                if url != USAGE_ENDPOINT {
                    return Ok(HttpResponse::new(403, ""));
                }
                Ok(HttpResponse::new(200, self.0.lock().unwrap().remove(0)))
            }
        }
        let file = auth_file(r#"{"tokens":{"access_token":"tok","account_id":"plan-transition"}}"#);
        let http = Plans(Mutex::new(vec![
            FIXTURE.to_owned(),
            FIXTURE.replace("\"pro\"", "\"prolite\""),
        ]));
        let provider = CodexProvider::new(http, file.path().to_path_buf()).with_account("record-plan", "Work");
        let first = provider.fetch(now()).unwrap().remove(0);
        let next = provider.fetch(now() + Duration::minutes(2)).unwrap().remove(0);
        assert_eq!(first.plan(), Some("Pro"));
        assert_eq!(next.plan(), Some("Pro Lite"));
        assert_eq!(next.id(), first.id());
        assert_eq!(next.label(), Some("Work"));
        assert!(
            !next
                .messages()
                .iter()
                .any(|message| message.contains("different ChatGPT account"))
        );
    }

    #[test]
    fn reported_plan_variants_change_without_guessing_a_multiplier() {
        for (raw, expected) in [("pro", "Pro"), ("prolite", "Pro Lite"), ("promax", "Pro Max")] {
            let payload = FIXTURE.replace("\"pro\"", &format!("\"{raw}\""));
            let account = parse_usage(&payload, now()).unwrap();
            assert_eq!(account.plan(), Some(expected));
            assert!(!account.display_name().contains("20x"));
            assert!(account.messages().is_empty());
        }
    }

    #[test]
    fn plan_name_known_and_unknown_formats() {
        assert_eq!(plan_name("prolite"), "Pro Lite");
        assert_eq!(plan_name("plus"), "Plus");
        assert_eq!(plan_name("team_plus"), "Team Plus");
    }

    #[test]
    fn read_credentials_snake_and_camel_case_tokens() {
        let snake = auth_file(r#"{"tokens":{"access_token":"tok","account_id":"acct"}}"#);
        let creds = read_credentials(snake.path()).unwrap();
        assert_eq!(creds.access_token, "tok");
        assert_eq!(creds.account_id.as_deref(), Some("acct"));

        let camel = auth_file(r#"{"tokens":{"accessToken":"tok2"}}"#);
        assert_eq!(read_credentials(camel.path()).unwrap().account_id, None);
    }

    #[test]
    fn read_credentials_missing_or_blank_returns_not_signed_in() {
        assert!(matches!(
            read_credentials(Path::new("Z:/definitely/missing/auth.json")),
            Err(ProviderError::NotSignedIn { .. })
        ));
        let blank = auth_file(r#"{"tokens":{"access_token":"  "}}"#);
        assert!(matches!(
            read_credentials(blank.path()),
            Err(ProviderError::NotSignedIn { .. })
        ));
    }

    #[test]
    fn credentials_debug_redacts_token() {
        let file = auth_file(r#"{"tokens":{"access_token":"secret-token","account_id":"acct"}}"#);
        let debug = format!("{:?}", read_credentials(file.path()).unwrap());
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("acct"));
    }

    #[test]
    fn fetch_success_sends_bearer_and_account_headers() {
        let file = auth_file(r#"{"tokens":{"access_token":"tok","account_id":"acct"}}"#);
        let provider = CodexProvider::new(FakeHttp::status(200, FIXTURE), file.path().to_path_buf())
            .with_subscription_record("automatic-record");
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].subscription_record(), Some("automatic-record"));
        assert_eq!(accounts[0].id(), &read_credentials(file.path()).unwrap().account());
        let headers = provider.http.seen_headers.lock().unwrap().clone();
        assert!(headers.contains(&("Authorization".into(), "Bearer tok".into())));
        assert!(headers.contains(&("ChatGPT-Account-Id".into(), "acct".into())));
    }

    #[test]
    fn fetch_unauthorized_returns_expired() {
        let file = auth_file(r#"{"tokens":{"access_token":"tok"}}"#);
        let provider = CodexProvider::new(FakeHttp::status(401, ""), file.path().to_path_buf());
        assert!(matches!(provider.fetch(now()), Err(ProviderError::Expired { .. })));
    }

    #[test]
    fn fetch_server_error_returns_status() {
        let file = auth_file(r#"{"tokens":{"access_token":"tok"}}"#);
        let provider = CodexProvider::new(FakeHttp::status(503, ""), file.path().to_path_buf());
        assert_eq!(provider.fetch(now()), Err(ProviderError::Http { status: 503 }));
    }

    #[test]
    fn provider_error_display_never_includes_token() {
        let message = ProviderError::Expired { hint: SIGN_IN_HINT }.to_string();
        assert_eq!(message, "Sign-in expired. Run `codex` and sign in with ChatGPT.");
    }

    /// Answers each GET with the next status in turn, recording the bearer token each one carried.
    struct Sequence {
        statuses: Mutex<Vec<u16>>,
        bearers: Mutex<Vec<String>>,
    }

    impl Sequence {
        fn new(statuses: &[u16]) -> Self {
            Self {
                statuses: Mutex::new(statuses.iter().rev().copied().collect()),
                bearers: Mutex::default(),
            }
        }
    }

    impl HttpClient for Sequence {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            if url != USAGE_ENDPOINT {
                return Ok(HttpResponse::new(403, ""));
            }
            let bearer = headers
                .iter()
                .find(|(name, _)| *name == "Authorization")
                .map(|(_, v)| (*v).to_owned());
            self.bearers.lock().unwrap().push(bearer.unwrap_or_default());
            let status = self.statuses.lock().unwrap().pop().unwrap_or(500);
            Ok(HttpResponse::new(status, if status == 200 { FIXTURE } else { "" }))
        }
    }

    /// Stands in for Codex: a renewal rewrites the sign-in file with `renewed`, or fails.
    struct FakeRenewer {
        path: PathBuf,
        renewed: Option<String>,
        calls: Mutex<u32>,
    }

    impl Renewer for FakeRenewer {
        fn renew(&self, home: &Path) -> Result<(), AppServerError> {
            assert_eq!(Some(home), self.path.parent());
            *self.calls.lock().unwrap() += 1;
            match &self.renewed {
                Some(contents) => {
                    std::fs::write(&self.path, contents).unwrap();
                    Ok(())
                }
                None => Err(AppServerError::Refused("refresh token expired".to_owned())),
            }
        }
    }

    fn renewer(file: &tempfile_lite::TempFile, renewed: Option<String>) -> Arc<FakeRenewer> {
        Arc::new(FakeRenewer {
            path: file.path().to_path_buf(),
            renewed,
            calls: Mutex::new(0),
        })
    }

    fn id_token(sub: &str, email: &str) -> String {
        crate::jwt::tests::token(&serde_json::json!({"sub": sub, "email": email}))
    }

    /// A sign-in whose access token expires at `exp` (Unix seconds).
    fn sign_in(access: &str, exp: i64, sub: &str) -> String {
        let access = crate::jwt::tests::token(&serde_json::json!({"exp": exp, "tag": access}));
        serde_json::json!({"tokens": {
            "access_token": access,
            "account_id": "workspace-1",
            "id_token": id_token(sub, "dev@example.com"),
        }})
        .to_string()
    }

    fn bearer_of(contents: &str) -> String {
        let json: Value = serde_json::from_str(contents).unwrap();
        format!("Bearer {}", json["tokens"]["access_token"].as_str().unwrap())
    }

    #[test]
    fn each_chatgpt_identity_is_its_own_account() {
        let read = |contents: String| {
            let file = auth_file(&contents);
            read_credentials(file.path()).unwrap()
        };
        let first = read(sign_in("a", 0, "user-1"));
        let other_user = read(sign_in("a", 0, "user-2"));
        assert!(first.account().as_str().starts_with("codex-") && first.account().as_str().len() == 18);
        assert_ne!(first.account(), other_user.account());
        assert_eq!(first.account(), read(sign_in("renewed", 1, "user-1")).account());
        let other_workspace = read(sign_in("a", 0, "user-1").replace("workspace-1", "workspace-2"));
        assert_ne!(first.account(), other_workspace.account());
        assert_eq!(first.email().as_deref(), Some("dev@example.com"));
        // No ID token: the legacy id, and no email.
        let legacy = read(r#"{"tokens":{"access_token":"tok"}}"#.to_owned());
        assert_eq!(legacy.account().as_str(), LEGACY_ID);
        assert_eq!(legacy.email(), None);
        let file = auth_file(&sign_in("a", 0, "user-1"));
        assert_eq!(signed_in_account(file.path()), Some(first.account()));
        assert_eq!(signed_in_email(file.path()).as_deref(), Some("dev@example.com"));
    }

    #[test]
    fn a_sign_in_about_to_expire_is_renewed_before_fetching() {
        let soon = (now() + Duration::hours(2)).timestamp();
        let later = (now() + Duration::days(9)).timestamp();
        let file = auth_file(&sign_in("old", soon, "user-1"));
        let renewed = sign_in("new", later, "user-1");
        let renewer = renewer(&file, Some(renewed.clone()));
        let provider =
            CodexProvider::new(Sequence::new(&[200]), file.path().to_path_buf()).with_renewer(renewer.clone());
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(*renewer.calls.lock().unwrap(), 1);
        assert_eq!(*provider.http.bearers.lock().unwrap(), [bearer_of(&renewed)]);
        assert_eq!(accounts[0].id(), &read_credentials(file.path()).unwrap().account());
    }

    #[test]
    fn a_fresh_sign_in_is_not_renewed() {
        let later = (now() + Duration::days(9)).timestamp();
        let file = auth_file(&sign_in("tok", later, "user-1"));
        let renewer = renewer(&file, Some(String::new()));
        let provider =
            CodexProvider::new(Sequence::new(&[200]), file.path().to_path_buf()).with_renewer(renewer.clone());
        provider.fetch(now()).unwrap();
        assert_eq!(*renewer.calls.lock().unwrap(), 0);
    }

    #[test]
    fn a_refused_sign_in_is_renewed_and_tried_once_more() {
        let later = (now() + Duration::days(9)).timestamp();
        let file = auth_file(&sign_in("revoked", later, "user-1"));
        let renewed = sign_in("new", later, "user-1");
        let renewer = renewer(&file, Some(renewed.clone()));
        let provider =
            CodexProvider::new(Sequence::new(&[401, 200]), file.path().to_path_buf()).with_renewer(renewer.clone());
        assert_eq!(provider.fetch(now()).unwrap().len(), 1);
        assert_eq!(*renewer.calls.lock().unwrap(), 1);
        let bearers = provider.http.bearers.lock().unwrap().clone();
        assert_eq!(bearers.len(), 2);
        assert!(bearers[1] == bearer_of(&renewed), "the retry uses the renewed token");
    }

    #[test]
    fn a_failed_renewal_is_the_account_s_expiry_and_waits_before_trying_again() {
        let soon = (now() + Duration::hours(1)).timestamp();
        let file = auth_file(&sign_in("old", soon, "user-1"));
        let renewer = renewer(&file, None);
        let provider = CodexProvider::new(Sequence::new(&[401, 401]), file.path().to_path_buf())
            .with_renewer(renewer.clone())
            .managed()
            .with_account("record-1", "Work");
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { account, label, error } = &outcomes[0] else {
            panic!("expected a failure");
        };
        assert_eq!(account, &read_credentials(file.path()).unwrap().account());
        assert_eq!(label.as_deref(), Some("Work"));
        assert_eq!(
            error,
            &ProviderError::Expired {
                hint: MANAGED_SIGN_IN_HINT
            }
        );
        // The 401 doesn't trigger a second renewal right after the failed one, nor does the next refresh.
        assert_eq!(*renewer.calls.lock().unwrap(), 1);
        provider.fetch_outcomes(now()).unwrap();
        assert_eq!(*renewer.calls.lock().unwrap(), 1);
    }

    #[test]
    fn a_managed_home_without_a_sign_in_says_to_sign_in_from_settings() {
        let missing = std::env::temp_dir()
            .join("codexbar-test-missing-home")
            .join("auth.json");
        let provider = CodexProvider::new(Sequence::new(&[]), missing).managed();
        assert_eq!(
            provider.fetch_outcomes(now()).err(),
            Some(ProviderError::NotSignedIn {
                hint: MANAGED_SIGN_IN_HINT
            })
        );
    }

    #[test]
    fn a_configured_account_is_named_after_its_label_and_plan() {
        let later = (now() + Duration::days(9)).timestamp();
        let file = auth_file(&sign_in("tok", later, "user-1"));
        let provider =
            CodexProvider::new(Sequence::new(&[200]), file.path().to_path_buf()).with_account("record-1", "Work");
        assert_eq!(provider.account_id(), Some("record-1"));
        assert_eq!(provider.account_label(), Some("Work"));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.label(), Some("Work"));
        assert_eq!(account.plan(), Some("Pro"));
        assert_eq!(provider.signed_in_account().as_ref(), Some(account.id()));
    }

    #[test]
    fn signing_a_home_in_to_another_identity_starts_a_separate_account() {
        let later = (now() + Duration::days(9)).timestamp();
        let file = auth_file(&sign_in("tok", later, "user-1"));
        let fetch = || {
            CodexProvider::new(Sequence::new(&[200]), file.path().to_path_buf())
                .fetch(now())
                .unwrap()
                .remove(0)
        };
        let first = fetch();
        assert!(
            !first
                .messages()
                .iter()
                .any(|message| message.contains("different ChatGPT account"))
        );
        std::fs::write(file.path(), sign_in("tok", later, "user-2")).unwrap();
        let second = fetch();
        assert_ne!(second.id(), first.id());
        assert!(
            second
                .messages()
                .iter()
                .any(|text| text.contains("different ChatGPT account"))
        );
        assert!(
            !fetch()
                .messages()
                .iter()
                .any(|message| message.contains("different ChatGPT account"))
        );
    }

    /// A minimal self-deleting temp file, to avoid a dev-dependency for three tests.
    mod tempfile_lite {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);

        pub struct TempFile(PathBuf);

        impl TempFile {
            pub fn with_contents(contents: &str) -> Self {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("codexbar-test-{}-{n}.json", std::process::id()));
                std::fs::write(&path, contents).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
}
