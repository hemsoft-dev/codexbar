//! Optional account metadata, independent of frequent usage polling.

use super::{Credentials, codex_plan_name};
use crate::HttpClient;
use chrono::{DateTime, Duration, Local, Utc};
use codexbar_core::format::DateStyle;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const TTL: Duration = Duration::hours(4);
const UNAVAILABLE: &str = "The account plan lookup is unavailable; showing the plan reported by usage.";

type PlanResult = Result<Option<Plan>, &'static str>;
type CacheKey = (PathBuf, [u8; 32]);
type CacheEntry = Arc<Mutex<Option<Cached>>>;
static CACHE: Mutex<Vec<(CacheKey, CacheEntry)>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct Plan {
    name: String,
    scheduled: Option<(String, DateTime<Utc>)>,
}

impl Plan {
    fn display(&self, now: DateTime<Utc>, style: &dyn DateStyle) -> String {
        match &self.scheduled {
            Some((name, at)) if *at > now => {
                format!(
                    "{} → {name} on {}",
                    self.name,
                    style.month_day(at.with_timezone(&Local).date_naive())
                )
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Clone)]
struct Cached {
    until: DateTime<Utc>,
    value: PlanResult,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn name(value: &Value) -> Option<String> {
    text(value, "plan_display_name")
        .map(str::to_owned)
        .or_else(|| text(value, "plan_type").map(codex_plan_name))
}

fn parse(payload: &str, account_id: &str) -> PlanResult {
    let json: Value = serde_json::from_str(payload).map_err(|_| UNAVAILABLE)?;
    let accounts = json.get("accounts").and_then(Value::as_object).ok_or(UNAVAILABLE)?;
    // Some versions key the personal account as `default`; always bind it to
    // the actual account ID, never choose another workspace's first record.
    let record = accounts
        .get(account_id)
        .or_else(|| {
            accounts
                .values()
                .find(|record| record.get("account").and_then(|a| text(a, "account_id")) == Some(account_id))
        })
        .ok_or(UNAVAILABLE)?;
    let account = record.get("account").ok_or(UNAVAILABLE)?;
    if text(account, "account_id").is_some_and(|id| id != account_id) {
        return Err(UNAVAILABLE);
    }
    let Some(name) = name(account) else { return Ok(None) };
    let scheduled = record
        .get("entitlement")
        .and_then(|e| e.get("scheduled_plan_change"))
        .and_then(|change| {
            let target = self::name(change)?;
            let at = DateTime::parse_from_rfc3339(text(change, "changes_at")?)
                .ok()?
                .with_timezone(&Utc);
            Some((target, at))
        });
    Ok(Some(Plan { name, scheduled }))
}

pub(super) fn lookup(
    http: &impl HttpClient,
    path: &Path,
    credentials: &Credentials,
    now: DateTime<Utc>,
    style: &dyn DateStyle,
) -> Result<Option<String>, &'static str> {
    let account_id = credentials.account_id.as_deref().ok_or(UNAVAILABLE)?;
    // No tokens or raw account identifiers are retained in the cache key.
    let identity: [u8; 32] = Sha256::digest(format!("{}\n{account_id}", credentials.account().as_str())).into();
    let key = (path.to_path_buf(), identity);
    let entry = {
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        match cache.iter().find(|(stored, _)| stored == &key) {
            Some((_, entry)) => entry.clone(),
            None => {
                let entry = Arc::new(Mutex::new(None));
                cache.push((key, entry.clone()));
                entry
            }
        }
    };
    // One request per identity even if refreshes overlap. Other identities do
    // not wait for this network request, and providers can be rebuilt freely.
    let mut cached = entry.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(value) = cached.as_ref().filter(|value| now < value.until) {
        return value
            .value
            .as_ref()
            .map(|plan| plan.as_ref().map(|plan| plan.display(now, style)))
            .map_err(|error| *error);
    }
    let bearer = format!("Bearer {}", credentials.access_token);
    let headers = [
        ("Authorization", bearer.as_str()),
        ("Accept", "application/json"),
        ("ChatGPT-Account-Id", account_id),
    ];
    let response = http.get(ENDPOINT, &headers);
    let mut until = now + TTL;
    let value = match response {
        Ok(response) if (200..300).contains(&response.status) => parse(&response.body, account_id),
        Ok(response) => {
            if let Some(seconds) = response.retry_after_secs {
                let delay = Duration::try_seconds(i64::try_from(seconds).unwrap_or(i64::MAX)).unwrap_or(Duration::MAX);
                until = until.max(now.checked_add_signed(delay).unwrap_or(DateTime::<Utc>::MAX_UTC));
            }
            Err(UNAVAILABLE)
        }
        Err(_) => Err(UNAVAILABLE),
    };
    // Re-read the effective plan at the actual transition, rather than keeping
    // the previous tier for the remainder of a four-hour cache interval.
    if let Ok(Some(Plan {
        scheduled: Some((_, at)),
        ..
    })) = &value
        && *at > now
    {
        until = until.min(*at);
    }
    let result = value
        .as_ref()
        .map(|plan| plan.as_ref().map(|plan| plan.display(now, style)))
        .map_err(|error| *error);
    *cached = Some(Cached { until, value });
    result
}

#[cfg(test)]
mod tests {
    use super::super::CodexProvider;
    use super::*;
    use crate::{HttpResponse, ProviderError, UsageProvider};
    use std::sync::atomic::{AtomicU32, Ordering};

    use codexbar_core::format::English;
    fn lookup(
        http: &impl HttpClient,
        path: &Path,
        credentials: &Credentials,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, &'static str> {
        super::lookup(http, path, credentials, now, &English)
    }
    fn now() -> DateTime<Utc> {
        "2026-10-23T04:00:00Z".parse().unwrap()
    }
    fn payload(id: &str, code: &str) -> String {
        serde_json::json!({"accounts":{id:{"account":{"account_id":id,"plan_type":code}}}}).to_string()
    }
    fn scheduled() -> String {
        serde_json::json!({"accounts":{"a":{
            "account":{"plan_type":"pro","plan_display_name":"Pro"},
            "entitlement":{"subscription_plan":"chatgptpro","renews_at":"2026-10-26T13:33:19Z",
                "scheduled_plan_change":{"plan_type":"prolite","changes_at":"2026-10-26T13:33:19Z"}}
        }}})
        .to_string()
    }
    fn credentials(id: &str) -> Credentials {
        Credentials {
            access_token: "test-token".into(),
            account_id: Some(id.into()),
            id_token: None,
        }
    }
    fn path() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "codex-plan-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    struct Http {
        response: Mutex<HttpResponse>,
        requests: Mutex<Vec<String>>,
    }
    impl Http {
        fn new(response: HttpResponse) -> Self {
            Self {
                response: Mutex::new(response),
                requests: Mutex::new(Vec::new()),
            }
        }
        fn body(&self, value: String) {
            *self.response.lock().unwrap() = HttpResponse::new(200, value);
        }
    }
    impl HttpClient for Http {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            if url == super::super::USAGE_ENDPOINT {
                return Ok(HttpResponse::new(
                    200,
                    include_str!("../../tests/fixtures/codex-usage.json"),
                ));
            }
            assert_eq!(url, ENDPOINT);
            assert!(headers.contains(&("Authorization", "Bearer test-token")));
            assert!(headers.iter().any(|(key, _)| *key == "ChatGPT-Account-Id"));
            self.requests.lock().unwrap().push(url.into());
            Ok(self.response.lock().unwrap().clone())
        }
    }
    impl HttpClient for Arc<Http> {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            self.as_ref().get(url, headers)
        }
    }
    #[test]
    fn account_display_name_wins_and_another_workspace_is_never_selected() {
        let data = r#"{"accounts":{"default":{"account":{"account_id":"a","plan_type":"prolite","plan_display_name":"API display"}},"b":{"account":{"plan_type":"enterprise"}}}}"#;
        assert_eq!(parse(data, "a").unwrap().unwrap().name, "API display");
        assert!(parse(data, "c").is_err());
        assert!(
            parse(
                r#"{"accounts":{"a":{"account":{"account_id":"b","plan_type":"pro"}}}}"#,
                "a"
            )
            .is_err()
        );
        assert_eq!(parse(&payload("a", "prolite"), "a").unwrap().unwrap().name, "Pro Lite");
        assert!(parse("bad JSON", "a").is_err());
    }
    #[test]
    fn schedule_uses_the_explicit_change_time_and_does_not_apply_the_downgrade_early() {
        let plan = parse(&scheduled(), "a").unwrap().unwrap();
        assert_eq!(plan.name, "Pro");
        assert_eq!(plan.display(now(), &English), "Pro → Pro Lite on Oct 26");
        let at = plan.scheduled.as_ref().unwrap().1;
        assert_eq!(at, "2026-10-26T13:33:19Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(plan.display(at, &English), "Pro");
        let missing = scheduled().replace("changes_at", "unrecognized_time");
        assert!(
            parse(&missing, "a").unwrap().unwrap().scheduled.is_none(),
            "renewal alone cannot invent a change date"
        );
    }
    #[test]
    fn scheduled_dates_use_the_selected_style_even_from_cached_metadata() {
        struct DayFirst;
        impl DateStyle for DayFirst {
            fn time(&self, at: chrono::NaiveDateTime) -> String {
                English.time(at)
            }
            fn weekday(&self, at: chrono::NaiveDate) -> String {
                English.weekday(at)
            }
            fn month_day(&self, at: chrono::NaiveDate) -> String {
                format!("{}. Okt.", at.format("%-d"))
            }
        }
        let http = Arc::new(Http::new(HttpResponse::new(200, scheduled())));
        let path = path();
        std::fs::write(&path, r#"{"tokens":{"access_token":"test-token","account_id":"a"}}"#).unwrap();
        let first = CodexProvider::new(http.clone(), path.clone())
            .fetch(now())
            .unwrap()
            .remove(0);
        let localized = CodexProvider::new(http.clone(), path.clone())
            .with_date_style(&DayFirst)
            .fetch(now())
            .unwrap()
            .remove(0);
        assert_eq!(first.plan(), Some("Pro → Pro Lite on Oct 26"));
        assert_eq!(localized.plan(), Some("Pro → Pro Lite on 26. Okt."));
        assert_eq!(
            http.requests.lock().unwrap().len(),
            1,
            "the cached metadata is formatted again"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cache_survives_provider_reconstruction_and_refreshes_after_four_hours() {
        let http = Arc::new(Http::new(HttpResponse::new(200, scheduled())));
        let path = path();
        std::fs::write(&path, r#"{"tokens":{"access_token":"test-token","account_id":"a"}}"#).unwrap();
        let fetch = |at| {
            CodexProvider::new(http.clone(), path.clone())
                .with_account("record-a", "Work")
                .fetch(at)
                .unwrap()
                .remove(0)
        };
        let first = fetch(now());
        assert_eq!(first.plan(), Some("Pro → Pro Lite on Oct 26"));
        http.body(payload("a", "prolite"));
        assert_eq!(fetch(now() + Duration::hours(3)).plan(), first.plan());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        let changed = fetch(now() + Duration::hours(4));
        assert_eq!(changed.plan(), Some("Pro Lite"));
        assert_eq!(first.id(), changed.id());
        assert_eq!(first.label(), changed.label());
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn scheduled_transition_expires_cache_early_and_updates_without_identity_change() {
        let http = Http::new(HttpResponse::new(200, scheduled()));
        let path = path();
        let creds = credentials("a");
        let at = "2026-10-26T13:33:19Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            lookup(&http, &path, &creds, at - Duration::minutes(30))
                .unwrap()
                .as_deref(),
            Some("Pro → Pro Lite on Oct 26")
        );
        http.body(payload("a", "prolite"));
        assert_eq!(lookup(&http, &path, &creds, at).unwrap().as_deref(), Some("Pro Lite"));
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn switching_accounts_never_reuses_another_accounts_cached_plan() {
        let http = Http::new(HttpResponse::new(200, payload("a", "pro")));
        let path = path();
        assert_eq!(
            lookup(&http, &path, &credentials("a"), now()).unwrap().as_deref(),
            Some("Pro")
        );
        http.body(payload("b", "prolite"));
        assert_eq!(
            lookup(&http, &path, &credentials("b"), now()).unwrap().as_deref(),
            Some("Pro Lite")
        );
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
    #[test]
    fn optional_refusal_or_rate_limit_preserves_usage_and_does_not_hammer_the_api() {
        for status in [403, 429] {
            let mut response = HttpResponse::new(status, "");
            response.retry_after_secs = Some(u64::MAX);
            let http = Http::new(response);
            let auth = path();
            std::fs::write(&auth, r#"{"tokens":{"access_token":"test-token","account_id":"a"}}"#).unwrap();
            let provider = CodexProvider::new(http, auth.clone());
            let first = provider.fetch(now()).unwrap().remove(0);
            let second = provider.fetch(now() + Duration::hours(5)).unwrap().remove(0);
            assert_eq!(first.id(), second.id());
            assert_eq!(first.plan(), Some("Pro"));
            assert!(!second.metrics().is_empty());
            assert_eq!(provider.http.requests.lock().unwrap().len(), 1);
            assert!(
                first
                    .messages()
                    .iter()
                    .any(|m| m.contains("plan lookup is unavailable"))
            );
            std::fs::remove_file(auth).unwrap();
        }
    }
}
