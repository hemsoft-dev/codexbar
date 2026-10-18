//! Prepaid-credit providers: OpenRouter and Moonshot (Kimi). Both use an API key and report a dollar balance.

use chrono::{DateTime, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Money, Provider};
use serde_json::Value;

use crate::{HttpClient, ProviderError, UsageProvider};

const OPENROUTER_CREDITS: &str = "https://openrouter.ai/api/v1/credits";
const MOONSHOT_BALANCE: &str = "https://api.moonshot.ai/v1/users/me/balance";
const OPENROUTER_KEY_HINT: &str = "Set OPENROUTER_API_KEY or add the key in CodexBar settings.";
const MOONSHOT_KEY_HINT: &str = "Set MOONSHOT_API_KEY or add the key in CodexBar settings.";

/// Dollars as a JSON number or numeric string, rounded to cents.
fn dollars(value: &Value) -> Option<Money> {
    let amount = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    amount
        .is_finite()
        .then(|| Money::from_cents((amount * 100.0).round() as i64))
}

/// Strips a pasted "Bearer " prefix and surrounding whitespace.
fn normalize_key(key: Option<String>) -> Option<String> {
    let key = key?;
    let key = key.trim();
    let key = key
        .strip_prefix("Bearer ")
        .or_else(|| key.strip_prefix("bearer "))
        .unwrap_or(key)
        .trim();
    (!key.is_empty()).then(|| key.to_owned())
}

/// The environment variable wins over the stored key, as in the WPF app.
pub fn resolve_key(env: &str, stored: Option<String>) -> Option<String> {
    normalize_key(std::env::var(env).ok()).or_else(|| normalize_key(stored))
}

fn balance_account(id: &str, provider: Provider, label: &str, remaining: Money, now: DateTime<Utc>) -> AccountSnapshot {
    let metric = Metric::Balance {
        label: label.to_owned(),
        remaining,
        burn_per_day: None,
    };
    AccountSnapshot::new(AccountId::new(id), provider, vec![metric], now)
}

/// OpenRouter: balance is `total_credits - total_usage`.
pub fn parse_openrouter(payload: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let data = json
        .get("data")
        .ok_or(ProviderError::Unexpected { detail: "missing data" })?;
    let credits = data.get("total_credits").and_then(dollars);
    let usage = data.get("total_usage").and_then(dollars);
    let (Some(credits), Some(usage)) = (credits, usage) else {
        return Err(ProviderError::Unexpected {
            detail: "missing credit fields",
        });
    };
    let remaining = Money::from_cents(credits.cents() - usage.cents());
    Ok(balance_account(
        "openrouter",
        Provider::OpenRouter,
        "Credits",
        remaining,
        now,
    ))
}

/// Moonshot: `data.available_balance`, a number or a numeric string.
pub fn parse_moonshot(payload: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let remaining = json
        .pointer("/data/available_balance")
        .and_then(dollars)
        .ok_or(ProviderError::Unexpected {
            detail: "missing balance",
        })?;
    Ok(balance_account(
        "moonshot",
        Provider::Moonshot,
        "Balance",
        remaining,
        now,
    ))
}

/// Moves a parsed account onto a configured account's id and label.
fn relabel(account: AccountSnapshot, configured: &Option<(String, String)>) -> AccountSnapshot {
    match configured {
        Some((id, label)) => {
            let relabeled = AccountSnapshot::new(
                AccountId::new(id.clone()),
                account.provider(),
                account.metrics().to_vec(),
                account.fetched_at(),
            );
            if label.is_empty() {
                relabeled
            } else {
                relabeled.with_label(label.clone())
            }
        }
        None => account,
    }
}

fn status_error(status: u16, hint: &'static str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Expired { hint },
        status => ProviderError::Http { status },
    }
}

pub struct OpenRouterProvider<H: HttpClient> {
    http: H,
    key: Option<String>,
    account: Option<(String, String)>,
}

impl<H: HttpClient> OpenRouterProvider<H> {
    pub fn new(http: H, key: Option<String>) -> Self {
        Self {
            http,
            key: normalize_key(key),
            account: None,
        }
    }

    /// Reports under a configured account's id and label (for several accounts of one provider).
    pub fn with_account(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.account = Some((id.into(), label.into()));
        self
    }

    /// Reports under a configured account's id without a label (the provider's only account), so its history and
    /// preferences keep the same id when a second account is added.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.account = Some((id.into(), String::new()));
        self
    }
}

impl<H: HttpClient> UsageProvider for OpenRouterProvider<H> {
    fn name(&self) -> &'static str {
        "OpenRouter"
    }

    fn account_id(&self) -> Option<&str> {
        self.account.as_ref().map(|(id, _)| id.as_str())
    }

    fn account_label(&self) -> Option<&str> {
        self.account
            .as_ref()
            .map(|(_, label)| label.as_str())
            .filter(|label| !label.is_empty())
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        let key = self.key.as_deref().ok_or(ProviderError::NotSignedIn {
            hint: OPENROUTER_KEY_HINT,
        })?;
        let bearer = format!("Bearer {key}");
        let response = self.http.get(
            OPENROUTER_CREDITS,
            &[("Authorization", &bearer), ("X-Title", "CodexBar")],
        )?;
        match response.status {
            200..=299 => parse_openrouter(&response.body, now).map(|account| vec![relabel(account, &self.account)]),
            status => Err(status_error(status, OPENROUTER_KEY_HINT)),
        }
    }
}

pub struct MoonshotProvider<H: HttpClient> {
    http: H,
    key: Option<String>,
    account: Option<(String, String)>,
}

impl<H: HttpClient> MoonshotProvider<H> {
    pub fn new(http: H, key: Option<String>) -> Self {
        Self {
            http,
            key: normalize_key(key),
            account: None,
        }
    }

    /// Reports under a configured account's id and label (for several accounts of one provider).
    pub fn with_account(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.account = Some((id.into(), label.into()));
        self
    }

    /// Reports under a configured account's id without a label (the provider's only account), so its history and
    /// preferences keep the same id when a second account is added.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.account = Some((id.into(), String::new()));
        self
    }
}

impl<H: HttpClient> UsageProvider for MoonshotProvider<H> {
    fn name(&self) -> &'static str {
        "Moonshot (Kimi)"
    }

    fn account_id(&self) -> Option<&str> {
        self.account.as_ref().map(|(id, _)| id.as_str())
    }

    fn account_label(&self) -> Option<&str> {
        self.account
            .as_ref()
            .map(|(_, label)| label.as_str())
            .filter(|label| !label.is_empty())
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        let key = self.key.as_deref().ok_or(ProviderError::NotSignedIn {
            hint: MOONSHOT_KEY_HINT,
        })?;
        let bearer = format!("Bearer {key}");
        let response = self.http.get(
            MOONSHOT_BALANCE,
            &[("Authorization", &bearer), ("Accept", "application/json")],
        )?;
        match response.status {
            200..=299 => parse_moonshot(&response.body, now).map(|account| vec![relabel(account, &self.account)]),
            status => Err(status_error(status, MOONSHOT_KEY_HINT)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    struct Fixed(HttpResponse);

    impl HttpClient for Fixed {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            Ok(self.0.clone())
        }
    }

    fn remaining(account: &AccountSnapshot) -> i64 {
        match account.primary() {
            Some(Metric::Balance { remaining, .. }) => remaining.cents(),
            other => panic!("expected balance, got {other:?}"),
        }
    }

    #[test]
    fn parse_openrouter_subtracts_usage_from_credits() {
        let account = parse_openrouter(r#"{"data":{"total_credits":50.0,"total_usage":31.58}}"#, now()).unwrap();
        assert_eq!(remaining(&account), 1842);
        assert_eq!(account.display_name(), "OpenRouter");
    }

    #[test]
    fn parse_openrouter_missing_fields_is_unexpected() {
        assert!(parse_openrouter(r#"{"data":{"total_credits":5}}"#, now()).is_err());
        assert!(parse_openrouter(r#"{"credits":5}"#, now()).is_err());
    }

    #[test]
    fn parse_moonshot_accepts_number_or_string() {
        assert_eq!(
            remaining(&parse_moonshot(r#"{"data":{"available_balance":7.85}}"#, now()).unwrap()),
            785
        );
        assert_eq!(
            remaining(&parse_moonshot(r#"{"data":{"available_balance":"7.85"}}"#, now()).unwrap()),
            785
        );
        assert!(parse_moonshot(r#"{"data":{}}"#, now()).is_err());
    }

    #[test]
    fn fetch_with_account_reports_configured_identity() {
        let provider = OpenRouterProvider::new(
            Fixed(HttpResponse::new(
                200,
                r#"{"data":{"total_credits":5,"total_usage":1}}"#,
            )),
            Some("k".into()),
        )
        .with_account("acct-2", "Team");
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.id().as_str(), "acct-2");
        assert_eq!(account.display_name(), "OpenRouter · Team");
    }

    #[test]
    fn fetch_with_id_keeps_the_provider_name() {
        let provider = OpenRouterProvider::new(
            Fixed(HttpResponse::new(
                200,
                r#"{"data":{"total_credits":5,"total_usage":1}}"#,
            )),
            Some("k".into()),
        )
        .with_id("acct-1");
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.id().as_str(), "acct-1");
        assert_eq!(account.display_name(), "OpenRouter");
    }

    #[test]
    fn normalize_key_strips_bearer_and_blank() {
        assert_eq!(normalize_key(Some(" Bearer sk-1 ".into())).as_deref(), Some("sk-1"));
        assert_eq!(normalize_key(Some("   ".into())), None);
        assert_eq!(normalize_key(None), None);
    }

    #[test]
    fn fetch_without_key_is_not_signed_in() {
        let provider = OpenRouterProvider::new(Fixed(HttpResponse::new(200, "")), None);
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
    }

    #[test]
    fn fetch_rejected_key_is_expired_and_other_status_is_http() {
        let rejected = MoonshotProvider::new(Fixed(HttpResponse::new(401, "")), Some("k".into()));
        assert!(matches!(rejected.fetch(now()), Err(ProviderError::Expired { .. })));
        let failing = OpenRouterProvider::new(Fixed(HttpResponse::new(500, "")), Some("k".into()));
        assert_eq!(failing.fetch(now()), Err(ProviderError::Http { status: 500 }));
    }
}
