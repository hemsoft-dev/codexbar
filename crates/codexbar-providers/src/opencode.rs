//! OpenCode Go limits and Zen balance as one account, read from the OpenCode dashboard with its `auth` cookie.
//! The dashboard is server-rendered SolidJS, so values are read from the hydration script, not an API.

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Money, Provider};

use crate::pace::elapsed_pace;
use crate::{HttpClient, ProviderError, UsageProvider};

const DASHBOARD: &str = "https://opencode.ai/workspace/";
const COOKIE_HINT: &str = "Sign in at opencode.ai and update the dashboard auth cookie in CodexBar settings.";
const BROWSER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Gecko/20100101 Firefox/148.0";
/// Zen balances are stored in units of 1e-8 dollars.
const ZEN_UNITS_PER_CENT: i64 = 1_000_000;

/// The `{...}` object assigned to `<name>:$R[n]=` in the hydration script, if present.
fn hydrated_object<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}:$R[");
    let start = html.find(&marker)?;
    let rest = &html[start + marker.len()..];
    let open = rest.find("]={")? + 2;
    let close = rest[open..].find('}')? + open;
    Some(&rest[open + 1..close])
}

/// An integer field (`key:123`) inside a hydrated object.
fn int_field(object: &str, key: &str) -> Option<i64> {
    object.split(',').find_map(|pair| {
        let (name, value) = pair.split_once(':')?;
        (name.trim() == key).then(|| value.trim().parse().ok())?
    })
}

/// Go windows: rolling 5-hour, weekly, monthly. Each carries `usagePercent` and `resetInSec`.
pub fn parse_go(html: &str, now: DateTime<Utc>) -> Vec<Metric> {
    let windows = [
        ("rollingUsage", "5-hour window", Duration::hours(5)),
        ("weeklyUsage", "Weekly", Duration::days(7)),
        ("monthlyUsage", "Monthly", Duration::days(30)),
    ];
    windows
        .iter()
        .filter_map(|(name, label, length)| {
            let object = hydrated_object(html, name)?;
            let used = (int_field(object, "usagePercent")?.clamp(0, 100)) as f64 / 100.0;
            let resets_at = now + Duration::seconds(int_field(object, "resetInSec")?.max(0));
            let pace = elapsed_pace(used, resets_at - *length, resets_at, now);
            Some(Metric::Window {
                label: (*label).to_owned(),
                used,
                resets_at,
                pace,
                length: Some(*length),
            })
        })
        .collect()
}

/// Zen balance: the first `balance:<int>` in the billing page.
pub fn parse_zen(html: &str) -> Option<Money> {
    let start = html.find("balance:")? + "balance:".len();
    let digits: String = html[start..].chars().take_while(char::is_ascii_digit).collect();
    let raw: i64 = digits.parse().ok()?;
    Some(Money::from_cents(raw / ZEN_UNITS_PER_CENT))
}

/// An OpenAuth sign-in page means the cookie no longer works.
fn is_sign_in_page(html: &str) -> bool {
    let lower = html.to_lowercase();
    lower.contains("openauth") && (lower.contains("sign in") || lower.contains("login"))
}

pub struct OpenCodeProvider<H: HttpClient> {
    http: H,
    workspace: Option<String>,
    go_cookie: Option<String>,
    zen_cookie: Option<String>,
}

impl<H: HttpClient> OpenCodeProvider<H> {
    /// Either cookie may be absent; that half is skipped.
    pub fn new(http: H, workspace: Option<String>, go_cookie: Option<String>, zen_cookie: Option<String>) -> Self {
        Self {
            http,
            workspace,
            go_cookie,
            zen_cookie,
        }
    }

    fn page(&self, workspace: &str, suffix: &str, cookie: &str) -> Result<String, ProviderError> {
        let url = format!("{DASHBOARD}{workspace}/{suffix}");
        let cookie = format!("auth={cookie}");
        let headers = [
            ("User-Agent", BROWSER_AGENT),
            ("Accept", "text/html"),
            ("Cookie", cookie.as_str()),
        ];
        let response = self.http.get(&url, &headers)?;
        match response.status {
            200..=299 if is_sign_in_page(&response.body) => Err(ProviderError::Expired { hint: COOKIE_HINT }),
            200..=299 => Ok(response.body),
            // An expired session is redirected to /console/login.
            300..=399 | 401 | 403 => Err(ProviderError::Expired { hint: COOKIE_HINT }),
            status => Err(ProviderError::Http { status }),
        }
    }
}

impl<H: HttpClient> UsageProvider for OpenCodeProvider<H> {
    fn name(&self) -> &'static str {
        "OpenCode Go + Zen"
    }

    /// Always its one `opencode` account, so a failed cookie fetch is never taken for an OpenCode account signed in
    /// through the Console (#102), which shares the provider name.
    fn account_id(&self) -> Option<&str> {
        Some("opencode")
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        let workspace = self
            .workspace
            .as_deref()
            .ok_or(ProviderError::NotSignedIn { hint: COOKIE_HINT })?;
        if self.go_cookie.is_none() && self.zen_cookie.is_none() {
            return Err(ProviderError::NotSignedIn { hint: COOKIE_HINT });
        }
        let mut metrics = Vec::new();
        let mut first_error = None;
        if let Some(cookie) = &self.go_cookie {
            match self.page(workspace, "go", cookie) {
                Ok(html) => metrics.extend(parse_go(&html, now)),
                Err(err) => first_error = Some(err),
            }
        }
        if let Some(cookie) = &self.zen_cookie {
            match self.page(workspace, "billing", cookie).map(|html| parse_zen(&html)) {
                Ok(Some(remaining)) => metrics.push(Metric::Balance {
                    label: "Zen balance".into(),
                    remaining,
                    burn_per_day: None,
                }),
                Ok(None) => {
                    first_error.get_or_insert(ProviderError::Unexpected {
                        detail: "Zen balance not found",
                    });
                }
                Err(err) => {
                    first_error.get_or_insert(err);
                }
            }
        }
        match (metrics.is_empty(), first_error) {
            (true, Some(err)) => Err(err),
            (true, None) => Err(ProviderError::Unexpected {
                detail: "no OpenCode usage on the dashboard",
            }),
            _ => Ok(vec![AccountSnapshot::new(
                AccountId::new("opencode"),
                Provider::OpenCode,
                metrics,
                now,
            )]),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    const GO_PAGE: &str = r#"<script>_$HY.r["go"]=$R[0]={rollingUsage:$R[1]={usagePercent:38,resetInSec:3600,limit:1},weeklyUsage:$R[2]={resetInSec:432000,usagePercent:12},monthlyUsage:$R[3]={usagePercent:5,resetInSec:1728000}}</script>"#;
    const ZEN_PAGE: &str = r#"<script>$R[4]={id:"wrk",balance:1260000000,currency:"usd"}</script>"#;

    struct ByUrl(HashMap<&'static str, HttpResponse>);

    impl HttpClient for ByUrl {
        fn get(&self, url: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            let suffix = url.rsplit('/').next().unwrap();
            self.0.get(suffix).cloned().ok_or(ProviderError::Network)
        }
    }

    #[test]
    fn parse_go_reads_windows_in_either_field_order() {
        let metrics = parse_go(GO_PAGE, now());
        let labels: Vec<&str> = metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["5-hour window", "Weekly", "Monthly"]);
        assert_eq!(metrics[0].used_fraction(), Some(0.38));
        assert_eq!(metrics[1].used_fraction(), Some(0.12));
        assert_eq!(metrics[0].resets_at(), Some(now() + Duration::hours(1)));
    }

    #[test]
    fn parse_zen_converts_units_to_cents() {
        assert_eq!(parse_zen(ZEN_PAGE), Some(Money::from_cents(1260)));
        assert_eq!(parse_zen("<html>no balance</html>"), None);
    }

    #[test]
    fn fetch_combines_go_windows_and_zen_balance() {
        let http = ByUrl(HashMap::from([
            ("go", HttpResponse::new(200, GO_PAGE)),
            ("billing", HttpResponse::new(200, ZEN_PAGE)),
        ]));
        let provider = OpenCodeProvider::new(http, Some("wrk".into()), Some("c".into()), Some("c".into()));
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(accounts[0].display_name(), "OpenCode Go + Zen");
        assert_eq!(accounts[0].metrics().len(), 4);
    }

    #[test]
    fn fetch_zen_only_skips_go() {
        let http = ByUrl(HashMap::from([("billing", HttpResponse::new(200, ZEN_PAGE))]));
        let provider = OpenCodeProvider::new(http, Some("wrk".into()), None, Some("c".into()));
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(accounts[0].metrics().len(), 1);
    }

    #[test]
    fn fetch_sign_in_page_means_expired_cookie() {
        let page = HttpResponse::new(200, "<title>OpenAuth</title> Sign in to continue");
        let provider = OpenCodeProvider::new(
            ByUrl(HashMap::from([("billing", page)])),
            Some("wrk".into()),
            None,
            Some("c".into()),
        );
        assert!(matches!(provider.fetch(now()), Err(ProviderError::Expired { .. })));
    }

    #[test]
    fn fetch_redirect_to_login_means_expired_cookie() {
        let redirect = HttpResponse::new(302, "");
        let provider = OpenCodeProvider::new(
            ByUrl(HashMap::from([("billing", redirect)])),
            Some("wrk".into()),
            None,
            Some("c".into()),
        );
        assert!(matches!(provider.fetch(now()), Err(ProviderError::Expired { .. })));
    }

    #[test]
    fn fetch_without_workspace_is_not_signed_in() {
        let provider = OpenCodeProvider::new(ByUrl(HashMap::new()), None, Some("c".into()), None);
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
    }
}
