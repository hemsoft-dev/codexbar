//! Consumer Gemini Apps usage (#106): the five-hour and weekly limits shown at `gemini.google.com/usage`, read with a
//! Google website session. Experimental: this is Gemini's private web contract (an obfuscated `batchexecute` RPC),
//! not a published API, and it can change without notice. It is not Gemini API, Gemini CLI or Code Assist quota.
//!
//! The session is two Google cookies, `__Secure-1PSID` and the optional rotating `__Secure-1PSIDTS`, captured from a
//! disposable browser profile (`browser_session`). They are high-value Google credentials: they go only to
//! `gemini.google.com`, never through a redirect, and never into logs, messages or snapshots.

use chrono::{DateTime, Duration, Utc};
use codexbar_core::Metric;
use serde_json::{Value, json};

use crate::pace::elapsed_pace;
use crate::{HttpClient, ProviderError};

const ORIGIN: &str = "https://gemini.google.com";
const USAGE_PAGE: &str = "https://gemini.google.com/usage";
const BATCH: &str = "https://gemini.google.com/_/BardChatUi/data/batchexecute";
/// The usage RPC's id in Gemini's web client.
const RPC_ID: &str = "jSf9Qc";
const SESSION_KIND: &str = "gemini-apps-session-v1";
pub const SESSION_COOKIE: &str = "__Secure-1PSID";
pub const ROTATING_COOKIE: &str = "__Secure-1PSIDTS";
const SIGN_IN_HINT: &str = "Sign in to Gemini Apps again in CodexBar settings.";

/// A Gemini Apps website session: the two cookie values CodexBar keeps, nothing else.
#[derive(Clone, PartialEq, Eq)]
pub struct AppsSession {
    pub psid: String,
    pub psidts: Option<String>,
}

impl std::fmt::Debug for AppsSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppsSession").finish_non_exhaustive()
    }
}

/// A cookie value Google could have set: printable ASCII with no separators.
fn cookie_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic() && b != b';' && b != b',')
}

impl AppsSession {
    pub fn to_secret(&self) -> String {
        json!({ "kind": SESSION_KIND, "psid": self.psid, "psidts": self.psidts }).to_string()
    }

    pub fn from_secret(secret: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(secret).ok()?;
        if value.get("kind")?.as_str()? != SESSION_KIND {
            return None;
        }
        let psid = value.get("psid")?.as_str().filter(|v| cookie_value(v))?.to_owned();
        let psidts = match value.get("psidts") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_str().filter(|v| cookie_value(v))?.to_owned()),
        };
        Some(Self { psid, psidts })
    }

    fn cookie_header(&self) -> String {
        match &self.psidts {
            Some(ts) => format!("{SESSION_COOKIE}={}; {ROTATING_COOKIE}={ts}", self.psid),
            None => format!("{SESSION_COOKIE}={}", self.psid),
        }
    }
}

/// One cookie as a browser reports it (Chrome DevTools `Storage.getCookies`).
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    /// Seconds since the epoch; `None` for a session cookie.
    pub expires: Option<f64>,
}

/// Parses DevTools' `{"cookies":[…]}` result.
pub fn browser_cookies(result: &Value) -> Vec<BrowserCookie> {
    result
        .get("cookies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|cookie| {
            let text = |key: &str| cookie.get(key).and_then(Value::as_str).map(str::to_owned);
            Some(BrowserCookie {
                name: text("name")?,
                value: text("value")?,
                domain: text("domain")?,
                path: text("path")?,
                secure: cookie.get("secure").and_then(Value::as_bool).unwrap_or(false),
                // DevTools reports -1 for a session cookie.
                expires: cookie
                    .get("expires")
                    .and_then(Value::as_f64)
                    .filter(|e| e.is_finite() && *e > 0.0),
            })
        })
        .collect()
}

/// Why browser cookies don't make a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CookieProblem {
    /// Not signed in yet: no usable session cookie.
    Missing,
    /// The browser holds two different values for one cookie; CodexBar won't guess which is right.
    Conflicting,
}

/// The session in `cookies`: only secure, unexpired `.google.com` cookies on the root path named `__Secure-1PSID`
/// (required) and `__Secure-1PSIDTS` (optional) count. Host-only, narrower-path, insecure and unrelated cookies are
/// ignored; two different values for one name fail.
pub fn session_from(cookies: &[BrowserCookie], now: DateTime<Utc>) -> Result<AppsSession, CookieProblem> {
    let pick = |name: &str| -> Result<Option<String>, CookieProblem> {
        let values: Vec<&str> = cookies
            .iter()
            .filter(|c| c.name == name && c.domain == ".google.com" && c.path == "/" && c.secure)
            .filter(|c| c.expires.is_none_or(|e| e > now.timestamp() as f64))
            .filter(|c| cookie_value(&c.value))
            .map(|c| c.value.as_str())
            .collect();
        match values.as_slice() {
            [] => Ok(None),
            [first, rest @ ..] if rest.iter().all(|v| v == first) => Ok(Some((*first).to_owned())),
            _ => Err(CookieProblem::Conflicting),
        }
    };
    let psid = pick(SESSION_COOKIE)?.ok_or(CookieProblem::Missing)?;
    let psidts = pick(ROTATING_COOKIE)?;
    Ok(AppsSession { psid, psidts })
}

/// One Gemini Apps limit.
#[derive(Clone, Debug, PartialEq)]
pub struct AppsWindow {
    pub fraction_used: f64,
    pub resets_at: DateTime<Utc>,
}

/// The limits Gemini returned; a missing one stays missing, never zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppsUsage {
    pub five_hour: Option<AppsWindow>,
    pub weekly: Option<AppsWindow>,
}

/// What a usage response says.
#[derive(Clone, Debug, PartialEq)]
pub enum AppsResponse {
    Usage(AppsUsage),
    /// Gemini refused the session (code 7 on the usage RPC): sign in again.
    Rejected,
    /// A shape CodexBar doesn't know.
    Unknown,
}

/// A quoted JSON string value of `"name":"…"` in the page bootstrap.
fn bootstrap_value(html: &str, name: &str) -> Option<String> {
    let marker = format!("\"{name}\"");
    let mut rest = &html[html.find(&marker)? + marker.len()..];
    rest = rest.trim_start().strip_prefix(':')?.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    // Find the closing quote, honoring escapes, then let JSON decode the string.
    let bytes = rest.as_bytes();
    let mut escaped = false;
    for (ix, byte) in bytes.iter().enumerate().skip(1) {
        match (escaped, byte) {
            (true, _) => escaped = false,
            (false, b'\\') => escaped = true,
            (false, b'"') => return serde_json::from_str::<String>(&rest[..=ix]).ok(),
            _ => {}
        }
    }
    None
}

/// The tokens the usage RPC needs from the usage page: the anti-CSRF `at` token, and the build label and session id
/// when the page has them.
#[derive(Clone, Debug, PartialEq)]
pub struct Bootstrap {
    pub at: String,
    pub build: Option<String>,
    pub session: Option<String>,
}

pub fn parse_bootstrap(html: &str) -> Option<Bootstrap> {
    Some(Bootstrap {
        at: bootstrap_value(html, "SNlM0e").filter(|t| !t.is_empty())?,
        build: bootstrap_value(html, "cfb2h"),
        session: bootstrap_value(html, "FdrFJe"),
    })
}

/// The `wrb.fr` rows of a `batchexecute` response, after its anti-XSSI prefix and length-prefixed chunks.
fn batch_rows(body: &str) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    let mut search = 0;
    while let Some(found) = body[search..].find("[[\"wrb.fr\"") {
        let start = search + found;
        let Some(end) = matching_bracket(&body.as_bytes()[start..]) else {
            break;
        };
        if let Ok(Value::Array(chunk)) = serde_json::from_str::<Value>(&body[start..=start + end]) {
            rows.extend(chunk.into_iter().filter_map(|row| match row {
                Value::Array(row) if row.len() >= 3 && row[0] == "wrb.fr" => Some(row),
                _ => None,
            }));
        }
        search = start + end + 1;
    }
    rows
}

/// The index of the bracket closing the one at 0, skipping brackets inside strings.
fn matching_bracket(bytes: &[u8]) -> Option<usize> {
    let (mut depth, mut in_string, mut escaped) = (0i32, false, false);
    for (ix, byte) in bytes.iter().enumerate() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(ix);
                }
            }
            _ => {}
        }
    }
    None
}

/// `[limit, fractionUsed, period, [[resetSeconds, resetNanos]]]`, period 1 for five hours and 2 for the week.
fn window(tuple: &Value) -> Option<(u8, AppsWindow)> {
    let tuple = tuple.as_array().filter(|t| t.len() >= 4)?;
    let number = |value: &Value| {
        value
            .as_f64()
            .or_else(|| value.as_str()?.parse().ok())
            .filter(|n| n.is_finite())
    };
    number(&tuple[0]).filter(|limit| *limit > 0.0)?;
    let fraction = number(&tuple[1]).filter(|f| (0.0..=1.5).contains(f))?;
    let period = number(&tuple[2]).filter(|p| *p == 1.0 || *p == 2.0)? as u8;
    let reset = tuple[3].as_array()?.first()?.as_array()?;
    let seconds = reset.first().and_then(number).filter(|s| *s >= 1_600_000_000.0)?;
    let resets_at = DateTime::from_timestamp(seconds as i64, 0)?;
    Some((
        period,
        AppsWindow {
            fraction_used: fraction,
            resets_at,
        },
    ))
}

/// Parses a usage RPC response. A rejection on the usage RPC wins over any usage in the same response.
pub fn parse_usage(body: &str) -> AppsResponse {
    let rows = batch_rows(body);
    let ours = || rows.iter().filter(|row| row[1] == RPC_ID);
    if ours().any(|row| row.get(5).and_then(|r| r.as_array()?.first()?.as_i64()) == Some(7)) {
        return AppsResponse::Rejected;
    }
    for row in ours() {
        let Some(payload) = row[2]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        else {
            continue;
        };
        let Some(groups) = payload.as_array() else {
            continue;
        };
        for group in groups {
            let windows: Vec<(u8, AppsWindow)> = group.as_array().into_iter().flatten().filter_map(window).collect();
            if windows.is_empty() {
                continue;
            }
            let find = |period| windows.iter().find(|(p, _)| *p == period).map(|(_, w)| w.clone());
            return AppsResponse::Usage(AppsUsage {
                five_hour: find(1),
                weekly: find(2),
            });
        }
    }
    AppsResponse::Unknown
}

/// The request id `batchexecute` expects: a number that changes between requests.
fn request_id(now: DateTime<Utc>) -> String {
    (100_000 + now.timestamp_millis().rem_euclid(800_000)).to_string()
}

fn status_error(response: &crate::HttpResponse, now: DateTime<Utc>) -> ProviderError {
    match response.status {
        300..=399 | 401 | 403 => ProviderError::Expired { hint: SIGN_IN_HINT },
        // Google's own wait, when it gives one; a minute otherwise.
        429 => ProviderError::RateLimited {
            retry_at: now + Duration::seconds(response.retry_after_secs.map_or(60, |s| s.min(3600) as i64)),
        },
        status => ProviderError::Http { status },
    }
}

/// Reads Gemini Apps usage with `session`: the usage page for its tokens, then the usage RPC.
pub fn fetch<H: HttpClient>(http: &H, session: &AppsSession, now: DateTime<Utc>) -> Result<AppsUsage, ProviderError> {
    let cookie = session.cookie_header();
    let page = http.get(
        USAGE_PAGE,
        &[
            ("Cookie", cookie.as_str()),
            ("Accept", "text/html,application/xhtml+xml"),
        ],
    )?;
    if page.status != 200 {
        return Err(status_error(&page, now));
    }
    // Signed out, Gemini serves a page without the tokens.
    let tokens = parse_bootstrap(&page.body).ok_or(ProviderError::Expired { hint: SIGN_IN_HINT })?;
    let request = json!([[[RPC_ID, "[]", null, "generic"]]]).to_string();
    let query = crate::http::form_body(&[
        ("rpcids", RPC_ID),
        ("source-path", "/usage"),
        ("bl", tokens.build.as_deref().unwrap_or_default()),
        ("f.sid", tokens.session.as_deref().unwrap_or_default()),
        ("hl", "en"),
        ("_reqid", &request_id(now)),
        ("rt", "c"),
    ]);
    let response = http.post_form(
        &format!("{BATCH}?{query}"),
        &[
            ("Cookie", cookie.as_str()),
            ("Accept", "text/plain,*/*"),
            ("Origin", ORIGIN),
        ],
        &[("f.req", &request), ("at", &tokens.at)],
    )?;
    if response.status != 200 {
        return Err(status_error(&response, now));
    }
    match parse_usage(&response.body) {
        AppsResponse::Usage(usage) => Ok(usage),
        AppsResponse::Rejected => Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
        AppsResponse::Unknown => Err(ProviderError::Unexpected {
            detail: "Gemini Apps usage in an unknown shape",
        }),
    }
}

/// The two Gemini Apps metrics, as windows with Gemini's own resets.
pub fn metrics(usage: &AppsUsage, now: DateTime<Utc>) -> Vec<Metric> {
    [
        ("Gemini Apps five-hour", &usage.five_hour, Duration::hours(5)),
        ("Gemini Apps weekly", &usage.weekly, Duration::days(7)),
    ]
    .into_iter()
    .filter_map(|(label, window, length)| {
        let window = window.as_ref()?;
        Some(Metric::Window {
            label: label.to_owned(),
            used: window.fraction_used,
            resets_at: window.resets_at,
            pace: elapsed_pace(window.fraction_used, window.resets_at - length, window.resets_at, now),
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-12T12:00:00Z".parse().unwrap()
    }

    fn cookie(name: &str, value: &str, domain: &str, path: &str, secure: bool, expires: Option<f64>) -> BrowserCookie {
        BrowserCookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: path.into(),
            secure,
            expires,
        }
    }

    #[test]
    fn only_secure_root_google_session_cookies_make_a_session() {
        let later = Some(now().timestamp() as f64 + 3600.0);
        let good = [
            cookie(SESSION_COOKIE, "g.abc", ".google.com", "/", true, later),
            cookie(ROTATING_COOKIE, "sidts-1", ".google.com", "/", true, None),
            cookie("NID", "other", ".google.com", "/", true, later),
        ];
        let session = session_from(&good, now()).unwrap();
        assert!(session.psid == "g.abc" && session.psidts.as_deref() == Some("sidts-1"));
        for ignored in [
            cookie(SESSION_COOKIE, "host", "gemini.google.com", "/", true, later),
            cookie(SESSION_COOKIE, "narrow", ".google.com", "/u", true, later),
            cookie(SESSION_COOKIE, "plain", ".google.com", "/", false, later),
            cookie(
                SESSION_COOKIE,
                "old",
                ".google.com",
                "/",
                true,
                Some(now().timestamp() as f64 - 1.0),
            ),
            cookie(SESSION_COOKIE, "evil", ".google.com.evil.example", "/", true, later),
        ] {
            assert_eq!(
                session_from(std::slice::from_ref(&ignored), now()),
                Err(CookieProblem::Missing),
                "{ignored:?}"
            );
        }
        let conflicting = [
            cookie(SESSION_COOKIE, "one", ".google.com", "/", true, later),
            cookie(SESSION_COOKIE, "two", ".google.com", "/", true, later),
        ];
        assert_eq!(session_from(&conflicting, now()), Err(CookieProblem::Conflicting));
    }

    #[test]
    fn a_session_round_trips_and_never_prints() {
        let session = AppsSession {
            psid: "g.abc".into(),
            psidts: None,
        };
        assert_eq!(AppsSession::from_secret(&session.to_secret()), Some(session.clone()));
        assert!(!format!("{session:?}").contains("g.abc"));
        assert_eq!(
            AppsSession::from_secret(r#"{"kind":"gemini-apps-session-v1","psid":"a;b"}"#),
            None
        );
        assert_eq!(AppsSession::from_secret("cookie=1"), None);
    }

    #[test]
    fn devtools_cookies_parse() {
        let result = json!({ "cookies": [
            { "name": "__Secure-1PSID", "value": "v", "domain": ".google.com", "path": "/", "secure": true, "expires": -1 },
            { "name": "broken" } ] });
        let cookies = browser_cookies(&result);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].expires, None, "-1 is a session cookie");
    }

    const PAGE: &str = r#"<script>WIZ_global_data = {"SNlM0e":"AT-token=","cfb2h":"boq_assistant-bard_20261010","FdrFJe":"-123"};</script>"#;

    fn usage_body(payload: &str) -> String {
        let row = json!([["wrb.fr", "jSf9Qc", payload, null, null, null, "generic"]]).to_string();
        format!(")]}}'\n\n{}\n{row}\n", row.len())
    }

    const PAYLOAD: &str = "[[[100,0.25,1,[[1792000000,0]]],[100,0.5,2,[[1792500000,0]]],[5,0.9,9,[[1792000000,0]]]]]";

    #[test]
    fn the_page_bootstrap_gives_the_rpc_tokens() {
        let tokens = parse_bootstrap(PAGE).unwrap();
        assert_eq!(tokens.at, "AT-token=");
        assert_eq!(tokens.build.as_deref(), Some("boq_assistant-bard_20261010"));
        assert_eq!(tokens.session.as_deref(), Some("-123"));
        assert_eq!(parse_bootstrap("<html>signed out</html>"), None);
    }

    #[test]
    fn usage_maps_periods_not_order_and_keeps_missing_windows_missing() {
        let AppsResponse::Usage(usage) = parse_usage(&usage_body(PAYLOAD)) else {
            panic!("usage");
        };
        assert_eq!(usage.five_hour.as_ref().unwrap().fraction_used, 0.25);
        assert_eq!(usage.weekly.as_ref().unwrap().resets_at.timestamp(), 1_792_500_000);
        let weekly_only = parse_usage(&usage_body("[[[100,0.5,2,[[1792500000,0]]]]]"));
        assert!(matches!(
            weekly_only,
            AppsResponse::Usage(AppsUsage {
                five_hour: None,
                weekly: Some(_)
            })
        ));
        let metrics = metrics(&usage, now());
        let labels: Vec<&str> = metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["Gemini Apps five-hour", "Gemini Apps weekly"]);
    }

    #[test]
    fn a_rejection_wins_and_unknown_shapes_fail() {
        let rejected = format!(
            ")]}}'\n{}",
            json!([
                ["wrb.fr", "jSf9Qc", null, null, null, [7], "generic"],
                ["wrb.fr", "jSf9Qc", PAYLOAD, null]
            ])
        );
        assert_eq!(parse_usage(&rejected), AppsResponse::Rejected);
        let other_rpc = format!(")]}}'\n{}", json!([["wrb.fr", "other", null, null, null, [7]]]));
        assert_eq!(parse_usage(&other_rpc), AppsResponse::Unknown);
        assert_eq!(
            parse_usage(&usage_body("[[[100,7,1,[[1792000000,0]]]]]")),
            AppsResponse::Unknown
        );
        assert_eq!(parse_usage("<html>"), AppsResponse::Unknown);
    }

    /// A request's URL, cookie header and form fields.
    type Seen = (String, String, Vec<(String, String)>);

    /// Answers the page and the RPC, recording what each request carried.
    #[derive(Clone, Default)]
    struct Gemini {
        page: Arc<Mutex<Option<HttpResponse>>>,
        rpc: Arc<Mutex<Option<HttpResponse>>>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl HttpClient for Gemini {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            let cookie = headers
                .iter()
                .find(|(n, _)| *n == "Cookie")
                .map(|(_, v)| (*v).to_owned());
            self.seen
                .lock()
                .unwrap()
                .push((url.to_owned(), cookie.unwrap_or_default(), Vec::new()));
            self.page.lock().unwrap().clone().ok_or(ProviderError::Network)
        }

        fn post_form(
            &self,
            url: &str,
            headers: &[(&str, &str)],
            fields: &[(&str, &str)],
        ) -> Result<HttpResponse, ProviderError> {
            let cookie = headers
                .iter()
                .find(|(n, _)| *n == "Cookie")
                .map(|(_, v)| (*v).to_owned());
            let fields = fields.iter().map(|(n, v)| ((*n).to_owned(), (*v).to_owned())).collect();
            self.seen
                .lock()
                .unwrap()
                .push((url.to_owned(), cookie.unwrap_or_default(), fields));
            self.rpc.lock().unwrap().clone().ok_or(ProviderError::Network)
        }
    }

    fn session() -> AppsSession {
        AppsSession {
            psid: "g.abc".into(),
            psidts: Some("ts".into()),
        }
    }

    #[test]
    fn a_fetch_sends_the_session_only_to_gemini_with_the_page_tokens() {
        let gemini = Gemini::default();
        *gemini.page.lock().unwrap() = Some(HttpResponse::new(200, PAGE));
        *gemini.rpc.lock().unwrap() = Some(HttpResponse::new(200, usage_body(PAYLOAD)));
        let usage = fetch(&gemini, &session(), now()).unwrap();
        assert!(usage.five_hour.is_some() && usage.weekly.is_some());
        let seen = gemini.seen.lock().unwrap();
        assert!(
            seen.iter()
                .all(|(url, _, _)| url.starts_with("https://gemini.google.com/"))
        );
        assert!(
            seen.iter()
                .all(|(_, cookie, _)| cookie == "__Secure-1PSID=g.abc; __Secure-1PSIDTS=ts")
        );
        assert!(seen[1].0.contains("rpcids=jSf9Qc") && seen[1].0.contains("bl=boq_assistant-bard_20261010"));
        assert!(seen[1].2.contains(&("at".to_owned(), "AT-token=".to_owned())));
    }

    #[test]
    fn signed_out_redirected_or_rejected_sessions_need_a_new_sign_in() {
        for (page, rpc) in [
            (HttpResponse::new(200, "<html>Sign in</html>"), None),
            (HttpResponse::new(302, ""), None),
            (HttpResponse::new(200, PAGE), Some(HttpResponse::new(401, ""))),
            (
                HttpResponse::new(200, PAGE),
                Some(HttpResponse::new(
                    200,
                    format!(")]}}'\n{}", json!([["wrb.fr", "jSf9Qc", null, null, null, [7]]])),
                )),
            ),
        ] {
            let gemini = Gemini::default();
            *gemini.page.lock().unwrap() = Some(page);
            *gemini.rpc.lock().unwrap() = rpc;
            assert!(matches!(
                fetch(&gemini, &session(), now()),
                Err(ProviderError::Expired { .. })
            ));
        }
    }

    #[test]
    fn a_rate_limit_waits_as_long_as_google_asks() {
        let limited = |retry_after: Option<u64>| {
            let mut response = HttpResponse::new(429, "");
            response.retry_after_secs = retry_after;
            response
        };
        for (page, rpc, wait) in [
            (limited(Some(120)), None, 120),
            (HttpResponse::new(200, PAGE), Some(limited(None)), 60),
            (limited(Some(86_400)), None, 3600),
        ] {
            let gemini = Gemini::default();
            *gemini.page.lock().unwrap() = Some(page);
            *gemini.rpc.lock().unwrap() = rpc;
            assert_eq!(
                fetch(&gemini, &session(), now()).unwrap_err(),
                ProviderError::RateLimited {
                    retry_at: now() + Duration::seconds(wait)
                }
            );
        }
    }
}
