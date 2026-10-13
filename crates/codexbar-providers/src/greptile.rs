//! Greptile code-review activity for an organization (#103), read with an organization API key through Greptile's MCP
//! server. Only the read-only `list_code_reviews` tool is ever called: a refresh never starts a review, changes a
//! repository or touches billing.
//!
//! Greptile's published tools report review activity, not billing. Completed reviews are therefore a count, never a
//! percentage of an allowance, a credit balance or a number of pull requests. A "Reviews used" quota is shown only when
//! a response carries an explicit used-and-allowance pair with a reset; nothing is inferred from a plan.

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider};
use serde_json::{Map, Value, json};

use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

pub const MCP_ENDPOINT: &str = "https://api.greptile.com/mcp";
/// The only tool CodexBar calls: it lists reviews and changes nothing.
pub const READ_ONLY_TOOL: &str = "list_code_reviews";
const PAGE_SIZE: usize = 100;
/// The deepest offset read; a longer history is reported as incomplete rather than counted short.
const MAX_OFFSET: usize = 1_000;
const KEY_HINT: &str = "Add a Greptile organization API key in CodexBar settings, or set GREPTILE_API_KEY.";
const REJECTED_HINT: &str = "Update the Greptile API key in CodexBar settings.";
const NO_PERMISSION: &str = "This Greptile API key can't read the organization's review activity.";
const RATE_LIMIT_WAIT: i64 = 60;

/// Review counts by status. Statuses Greptile adds later are counted as other, never as completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub completed: u64,
    pub pending: u64,
    pub in_progress: u64,
    pub failed: u64,
    pub skipped: u64,
    pub other: u64,
}

impl StatusCounts {
    fn record(&mut self, status: &str) {
        match status.trim().to_uppercase().as_str() {
            "COMPLETED" => self.completed += 1,
            "PENDING" => self.pending += 1,
            "REVIEWING_FILES" | "GENERATING_SUMMARY" | "IN_PROGRESS" | "IN-PROGRESS" => self.in_progress += 1,
            "FAILED" => self.failed += 1,
            "SKIPPED" => self.skipped += 1,
            _ => self.other += 1,
        }
    }

    /// "2 pending, 1 failed", or `None` when every review completed.
    fn others(&self) -> Option<String> {
        let parts: Vec<String> = [
            (self.pending, "pending"),
            (self.in_progress, "in progress"),
            (self.failed, "failed"),
            (self.skipped, "skipped"),
            (self.other, "with another status"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{} {name}", codexbar_core::group_thousands(count)))
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// An explicit review allowance a response carried. Not part of Greptile's published schema; accepted only as a
/// complete used-and-allowance pair, so unrelated metadata can't become a gauge.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewQuota {
    pub used: u64,
    pub allowance: u64,
    pub period_start: Option<DateTime<Utc>>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// One page of `list_code_reviews`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPage {
    /// (id, status) of each review on the page.
    pub reviews: Vec<(String, String)>,
    pub total: Option<usize>,
    pub quota: Option<ReviewQuota>,
    pub truncated: bool,
}

/// Why a page couldn't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    Rejected,
    NoPermission,
    RateLimited,
    Failed,
    Malformed,
}

/// The JSON-RPC request for one page: a `tools/call` of the read-only listing tool and nothing else.
pub fn page_request(offset: usize) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": format!("codexbar.greptile.reviews.{offset}"),
        "method": "tools/call",
        "params": { "name": READ_ONLY_TOOL, "arguments": { "limit": PAGE_SIZE, "offset": offset } },
    })
}

/// The JSON objects in a response: one JSON body, or each `data:` event of an event stream.
fn response_objects(body: &str) -> Vec<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return vec![value];
    }
    let mut objects = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let mut flush = |data: &mut Vec<&str>| {
        if data.is_empty() {
            return;
        }
        let payload = data.join("\n");
        data.clear();
        if payload != "[DONE]"
            && let Ok(value) = serde_json::from_str::<Value>(&payload)
        {
            objects.push(value);
        }
    };
    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            flush(&mut data);
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.trim());
        }
    }
    flush(&mut data);
    objects
}

fn has_reviews(value: &Value) -> bool {
    value
        .get("codeReviews")
        .and_then(Value::as_array)
        .is_some_and(|reviews| reviews.iter().all(Value::is_object))
}

/// The object holding `codeReviews`: the root, the result, its structured content, or JSON text content.
fn review_payload(root: &Value) -> Option<Value> {
    if has_reviews(root) {
        return Some(root.clone());
    }
    let result = root.get("result")?;
    if has_reviews(result) {
        return Some(result.clone());
    }
    if let Some(structured) = result.get("structuredContent").filter(|s| has_reviews(s)) {
        return Some(structured.clone());
    }
    result
        .get("content")?
        .as_array()?
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| serde_json::from_str::<Value>(item.get("text")?.as_str()?).ok())
        .find(has_reviews)
}

fn classify(message: &str) -> PageError {
    let message = message.to_lowercase();
    if message.contains("auth") || message.contains("api key") || message.contains("unauthorized") {
        PageError::Rejected
    } else if message.contains("permission") || message.contains("forbidden") || message.contains("organization") {
        PageError::NoPermission
    } else if message.contains("rate") || message.contains("too many") {
        PageError::RateLimited
    } else {
        PageError::Failed
    }
}

/// The text of a tool result that reported an error (`isError`).
fn tool_error(root: &Value) -> Option<String> {
    let result = root.get("result")?;
    if result.get("isError").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    Some(text)
}

fn whole(value: Option<&Value>) -> Option<u64> {
    let number = match value? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (number.is_finite() && number >= 0.0 && number.fract() == 0.0 && number <= u64::MAX as f64).then_some(number as u64)
}

fn instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::String(text) => DateTime::parse_from_rfc3339(text).ok().map(|t| t.with_timezone(&Utc)),
        Value::Number(number) => {
            let seconds = number.as_f64().filter(|s| s.is_finite() && *s > 0.0)?;
            let seconds = if seconds >= 100_000_000_000.0 {
                seconds / 1000.0
            } else {
                seconds
            };
            DateTime::from_timestamp(seconds as i64, 0)
        }
        _ => None,
    }
}

/// Keys lowercased with only their letters, so `reviews_used`, `reviewsUsed` and `Reviews-Used` match.
fn normalized(object: &Map<String, Value>) -> Map<String, Value> {
    let mut values = Map::new();
    for (key, value) in object {
        let key: String = key
            .chars()
            .filter(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_lowercase();
        if !key.is_empty() && !values.contains_key(&key) {
            values.insert(key, value.clone());
        }
    }
    values
}

fn first<'a>(values: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| values.get(*key))
}

/// An explicit review-named used-and-allowance pair. Credit-named fields are never read as reviews.
fn review_quota(payload: &Value) -> Option<ReviewQuota> {
    ["reviewUsage", "billingUsage", "usage", "quota"]
        .iter()
        .filter_map(|name| payload.get(*name)?.as_object())
        .find_map(|container| {
            let values = normalized(container);
            let used = whole(first(&values, &["reviewsused", "usedreviews", "completedreviewsused"]))?;
            let allowance = whole(first(
                &values,
                &["includedreviews", "reviewallowance", "reviewlimit", "reviewslimit"],
            ))
            .filter(|allowance| *allowance > 0)?;
            Some(ReviewQuota {
                used,
                allowance,
                period_start: instant(first(&values, &["billingperiodstart", "periodstart"])),
                resets_at: instant(first(
                    &values,
                    &["billingperiodend", "periodend", "resetat", "resetsat"],
                )),
            })
        })
}

/// Parses one response body into a page, or why it can't be used.
pub fn parse_page(body: &str) -> Result<ReviewPage, PageError> {
    let mut outcome = Err(PageError::Malformed);
    for root in response_objects(body) {
        if let Some(error) = root.get("error") {
            let message = error.get("message").and_then(Value::as_str).unwrap_or_default();
            return Err(classify(message));
        }
        if let Some(message) = tool_error(&root) {
            return Err(classify(&message));
        }
        let Some(payload) = review_payload(&root) else {
            continue;
        };
        outcome = page_from(&payload);
        if outcome.is_ok() {
            return outcome;
        }
    }
    outcome
}

fn page_from(payload: &Value) -> Result<ReviewPage, PageError> {
    let mut reviews = Vec::new();
    for review in payload
        .get("codeReviews")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = review
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or(PageError::Malformed)?;
        let status = review.get("status").and_then(Value::as_str).unwrap_or("UNKNOWN");
        reviews.push((id.to_owned(), status.to_owned()));
    }
    let total = match payload.get("total") {
        None | Some(Value::Null) => None,
        value => Some(whole(value).ok_or(PageError::Malformed)? as usize),
    };
    let truncated = match payload.get("truncated") {
        None => false,
        Some(Value::Bool(truncated)) => *truncated,
        Some(_) => return Err(PageError::Malformed),
    };
    Ok(ReviewPage {
        reviews,
        total,
        quota: review_quota(payload),
        truncated,
    })
}

/// The organization's review activity, read to the end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Activity {
    pub counts: StatusCounts,
    pub quota: Option<ReviewQuota>,
}

/// One Greptile organization, by its API key.
pub struct GreptileProvider<H: HttpClient> {
    http: H,
    key: Option<String>,
    account: String,
    label: String,
}

impl<H: HttpClient> GreptileProvider<H> {
    pub fn new(http: H, key: Option<String>, account: impl Into<String>, label: impl Into<String>) -> Self {
        let key = key
            .map(|key| key.trim().trim_start_matches("Bearer ").trim().to_owned())
            .filter(|key| !key.is_empty());
        Self {
            http,
            key,
            account: account.into(),
            label: label.into(),
        }
    }

    fn page(&self, key: &str, offset: usize, now: DateTime<Utc>) -> Result<ReviewPage, ProviderError> {
        let bearer = format!("Bearer {key}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ];
        let response = self
            .http
            .post_json(MCP_ENDPOINT, &headers, &page_request(offset).to_string())?;
        let rate_limited = |retry: Option<u64>| ProviderError::RateLimited {
            retry_at: now + Duration::seconds(retry.map_or(RATE_LIMIT_WAIT, |s| s.min(3600) as i64)),
        };
        match response.status {
            200..=299 => {}
            401 => return Err(ProviderError::Expired { hint: REJECTED_HINT }),
            403 => return Err(ProviderError::Refused { reason: NO_PERMISSION }),
            429 => return Err(rate_limited(response.retry_after_secs)),
            status => return Err(ProviderError::Http { status }),
        }
        parse_page(&response.body).map_err(|error| match error {
            PageError::Rejected => ProviderError::Expired { hint: REJECTED_HINT },
            PageError::NoPermission => ProviderError::Refused { reason: NO_PERMISSION },
            PageError::RateLimited => rate_limited(None),
            PageError::Failed => ProviderError::Unexpected {
                detail: "Greptile couldn't complete the read-only review listing",
            },
            PageError::Malformed => ProviderError::Unexpected {
                detail: "Greptile review activity in an unknown shape",
            },
        })
    }

    /// Reads every page, deduplicating reviews by id, until a short page. A truncated page, fewer unique reviews than
    /// the stated total, or a history deeper than the offset cap is an error: a partial count is never shown as
    /// complete.
    pub fn activity(&self, now: DateTime<Utc>) -> Result<Activity, ProviderError> {
        let key = self
            .key
            .as_deref()
            .ok_or(ProviderError::NotSignedIn { hint: KEY_HINT })?;
        let incomplete = ProviderError::Unexpected {
            detail: "Greptile returned incomplete review pages",
        };
        let mut seen = std::collections::HashSet::new();
        let mut activity = Activity::default();
        let mut expected: Option<usize> = None;
        let mut offset = 0;
        while offset <= MAX_OFFSET {
            let page = self.page(key, offset, now)?;
            if page.truncated {
                return Err(incomplete);
            }
            expected = match (expected, page.total) {
                (Some(known), Some(total)) => Some(known.max(total)),
                (known, total) => known.or(total),
            };
            if activity.quota.is_none() {
                activity.quota = page.quota.clone();
            }
            for (id, status) in &page.reviews {
                if seen.insert(id.clone()) {
                    activity.counts.record(status);
                }
            }
            // Only a short page ends the history. A full page is followed even when its `total` says it was the last,
            // so a `total` that counts only that page can never stop the scan early.
            if page.reviews.len() < PAGE_SIZE {
                return if expected.is_none_or(|total| seen.len() >= total) {
                    Ok(activity)
                } else {
                    Err(incomplete)
                };
            }
            offset += page.reviews.len();
        }
        Err(incomplete)
    }

    fn usage(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let activity = self.activity(now)?;
        let mut metrics = Vec::new();
        let mut messages = Vec::new();
        match &activity.quota {
            Some(quota) => match quota.resets_at {
                Some(resets_at) => {
                    let used = quota.used as f64 / quota.allowance as f64;
                    metrics.push(Metric::Quota {
                        label: "Reviews used".into(),
                        used: quota.used,
                        limit: quota.allowance,
                        resets_at,
                        pace: quota
                            .period_start
                            .and_then(|start| elapsed_pace(used, start, resets_at, now)),
                    });
                }
                None => messages.push(format!(
                    "Greptile reports {} of {} reviews used this billing period, without saying when it resets.",
                    codexbar_core::group_thousands(quota.used),
                    codexbar_core::group_thousands(quota.allowance)
                )),
            },
            None => messages.push(
                "Shows review history, not remaining credits: Greptile returned no billing allowance.".to_owned(),
            ),
        }
        metrics.push(Metric::Activity {
            label: "Completed reviews".into(),
            count: activity.counts.completed,
        });
        if let Some(others) = activity.counts.others() {
            messages.push(format!("Also {others}."));
        }
        let mut account = AccountSnapshot::new(AccountId::new(self.account.clone()), Provider::Greptile, metrics, now);
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        Ok(messages.into_iter().fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> UsageProvider for GreptileProvider<H> {
    fn name(&self) -> &'static str {
        "Greptile"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.usage(now).map(|account| vec![account])
    }

    /// Every failure is this account's own, so it shows on its row.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        Ok(vec![match self.usage(now) {
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-12T12:00:00Z".parse().unwrap()
    }

    /// Answers posts from a script and records each request body and its headers.
    #[derive(Clone, Default)]
    struct Mcp {
        script: Arc<Mutex<VecDeque<HttpResponse>>>,
        bodies: Arc<Mutex<Vec<Value>>>,
        authorization: Arc<Mutex<Vec<String>>>,
    }

    impl Mcp {
        fn answering(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
            let mcp = Self::default();
            mcp.script.lock().unwrap().extend(responses);
            mcp
        }

        fn requests(&self) -> Vec<Value> {
            self.bodies.lock().unwrap().clone()
        }
    }

    impl HttpClient for Mcp {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            panic!("Greptile is only ever posted to");
        }

        fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
            assert_eq!(url, MCP_ENDPOINT);
            self.bodies.lock().unwrap().push(serde_json::from_str(body).unwrap());
            if let Some((_, value)) = headers.iter().find(|(name, _)| *name == "Authorization") {
                self.authorization.lock().unwrap().push((*value).to_owned());
            }
            self.script.lock().unwrap().pop_front().ok_or(ProviderError::Network)
        }
    }

    fn reviews(range: std::ops::Range<usize>, status: &str) -> Vec<Value> {
        range
            .map(|i| json!({ "id": format!("r{i}"), "status": status }))
            .collect()
    }

    fn page(reviews: Vec<Value>, extra: Value) -> HttpResponse {
        let mut payload = json!({ "codeReviews": reviews });
        if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            payload.extend(extra.clone());
        }
        let text = payload.to_string();
        HttpResponse::new(
            200,
            json!({ "jsonrpc": "2.0", "id": "x", "result": { "content": [ { "type": "text", "text": text } ] } })
                .to_string(),
        )
    }

    fn provider(mcp: &Mcp) -> GreptileProvider<Mcp> {
        GreptileProvider::new(mcp.clone(), Some("grep-key".into()), "gr-1", "Acme")
    }

    #[test]
    fn a_refresh_only_ever_calls_the_read_only_listing_tool() {
        let mcp = Mcp::answering([
            page(reviews(0..100, "COMPLETED"), json!({ "total": 150 })),
            page(reviews(100..150, "COMPLETED"), json!({ "total": 150 })),
        ]);
        provider(&mcp).fetch(now()).unwrap();
        let requests = mcp.requests();
        assert_eq!(requests.len(), 2);
        for (request, offset) in requests.iter().zip([0, 100]) {
            assert_eq!(request["method"], "tools/call");
            assert_eq!(request["params"]["name"], READ_ONLY_TOOL);
            assert_eq!(
                request["params"]["arguments"],
                json!({ "limit": 100, "offset": offset })
            );
        }
        assert!(
            mcp.authorization
                .lock()
                .unwrap()
                .iter()
                .all(|value| value == "Bearer grep-key")
        );
    }

    #[test]
    fn completed_reviews_are_a_count_and_other_statuses_are_named() {
        let mut list = reviews(0..5, "COMPLETED");
        list.extend(reviews(5..7, "PENDING"));
        list.extend(reviews(7..8, "REVIEWING_FILES"));
        list.extend(reviews(8..9, "FAILED"));
        list.extend(reviews(9..10, "SKIPPED"));
        list.extend(reviews(10..11, "SOMETHING_NEW"));
        let mcp = Mcp::answering([page(list, json!({}))]);
        let account = provider(&mcp).fetch(now()).unwrap().remove(0);
        assert_eq!(account.id().as_str(), "gr-1");
        assert_eq!(account.display_name(), "Greptile · Acme");
        assert_eq!(
            account.metrics(),
            [Metric::Activity {
                label: "Completed reviews".into(),
                count: 5
            }]
        );
        assert_eq!(account.primary().and_then(Metric::used_fraction), None, "no percentage");
        assert_eq!(
            account.messages(),
            [
                "Shows review history, not remaining credits: Greptile returned no billing allowance.",
                "Also 2 pending, 1 in progress, 1 failed, 1 skipped, 1 with another status."
            ]
        );
    }

    #[test]
    fn repeated_review_ids_count_once() {
        let mut first = reviews(0..100, "COMPLETED");
        first[99] = json!({ "id": "r0", "status": "COMPLETED" });
        let mcp = Mcp::answering([page(first, json!({})), page(reviews(100..110, "COMPLETED"), json!({}))]);
        let activity = provider(&mcp).activity(now()).unwrap();
        assert_eq!(activity.counts.completed, 109);
    }

    #[test]
    fn empty_activity_is_zero_reviews_and_no_allowance() {
        let mcp = Mcp::answering([page(vec![], json!({ "total": 0 }))]);
        let account = provider(&mcp).fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics()[0].history_value(), Some(0.0));
        assert_eq!(account.metrics().len(), 1, "no gauge is invented");
    }

    #[test]
    fn incomplete_scans_are_errors_not_short_counts() {
        let incomplete = |mcp: Mcp| {
            matches!(
                provider(&mcp).fetch(now()),
                Err(ProviderError::Unexpected { detail }) if detail.contains("incomplete")
            )
        };
        assert!(incomplete(Mcp::answering([page(
            reviews(0..10, "COMPLETED"),
            json!({ "truncated": true })
        )])));
        // Fewer reviews than the stated total.
        assert!(incomplete(Mcp::answering([page(
            reviews(0..10, "COMPLETED"),
            json!({ "total": 30 })
        )])));
        // Repeated ids leave fewer unique reviews than the total.
        let mut repeated = reviews(0..100, "COMPLETED");
        repeated[1] = json!({ "id": "r0", "status": "COMPLETED" });
        assert!(incomplete(Mcp::answering([
            page(repeated, json!({ "total": 100 })),
            page(vec![], json!({ "total": 100 })),
        ])));
        // A history deeper than the offset cap.
        let full: Vec<HttpResponse> = (0..12)
            .map(|i| page(reviews(i * 100..i * 100 + 100, "COMPLETED"), json!({})))
            .collect();
        let mcp = Mcp::answering(full);
        assert!(incomplete(mcp.clone()));
        assert_eq!(mcp.requests().len(), 11, "pages stop at the offset cap");
    }

    #[test]
    fn a_total_that_counts_only_its_page_never_stops_the_scan_early() {
        let mcp = Mcp::answering([
            page(reviews(0..100, "COMPLETED"), json!({ "total": 100 })),
            page(reviews(100..140, "COMPLETED"), json!({ "total": 40 })),
        ]);
        let activity = provider(&mcp).activity(now()).unwrap();
        assert_eq!(activity.counts.completed, 140);
        assert_eq!(mcp.requests().len(), 2);
    }

    #[test]
    fn event_streams_and_other_envelopes_parse() {
        let payload = json!({ "codeReviews": [ { "id": "a", "status": "COMPLETED" } ], "total": 1 }).to_string();
        let stream = format!(
            "event: message\r\ndata: {}\r\n\r\n",
            json!({ "jsonrpc": "2.0", "result": { "content": [ { "type": "text", "text": payload } ] } })
        );
        assert_eq!(parse_page(&stream).unwrap().reviews.len(), 1);
        let structured = json!({ "result": { "structuredContent": { "codeReviews": [], "total": 0 } } }).to_string();
        assert_eq!(parse_page(&structured).unwrap().total, Some(0));
        assert_eq!(
            parse_page(r#"{"codeReviews":[{"status":"COMPLETED"}]}"#),
            Err(PageError::Malformed)
        );
        assert_eq!(
            parse_page(r#"{"codeReviews":[],"truncated":"no"}"#),
            Err(PageError::Malformed)
        );
        assert_eq!(parse_page("<html>"), Err(PageError::Malformed));
    }

    #[test]
    fn a_returned_review_allowance_is_shown_only_with_a_reset() {
        let quota = json!({ "usage": { "reviews_used": 12, "included_reviews": "50", "periodStart": "2026-10-01T00:00:00Z",
            "periodEnd": "2026-11-01T00:00:00Z" } });
        let mcp = Mcp::answering([page(reviews(0..3, "COMPLETED"), quota)]);
        let account = provider(&mcp).fetch(now()).unwrap().remove(0);
        assert!(matches!(
            account.primary(),
            Some(Metric::Quota { used: 12, limit: 50, label, .. }) if label == "Reviews used"
        ));
        assert_eq!(account.metrics()[0].resets_at(), "2026-11-01T00:00:00Z".parse().ok());

        let no_reset = json!({ "quota": { "reviewsUsed": 12, "reviewLimit": 50 } });
        let mcp = Mcp::answering([page(reviews(0..3, "COMPLETED"), no_reset)]);
        let account = provider(&mcp).fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 1, "no quota without a reset");
        assert!(account.messages()[0].starts_with("Greptile reports 12 of 50 reviews used"));
    }

    #[test]
    fn credit_named_or_malformed_allowances_never_become_a_quota() {
        for extra in [
            json!({ "usage": { "creditsUsed": 12, "includedCredits": 50, "periodEnd": "2026-11-01T00:00:00Z" } }),
            json!({ "usage": { "reviewsUsed": true, "includedReviews": 50 } }),
            json!({ "usage": { "reviewsUsed": 12, "includedReviews": 0 } }),
            json!({ "usage": { "reviewsUsed": -1, "includedReviews": 50 } }),
            json!({ "usage": { "reviewsUsed": 1.5, "includedReviews": 50 } }),
        ] {
            let parsed = parse_page(&page(vec![], extra.clone()).body).unwrap();
            assert_eq!(parsed.quota, None, "{extra}");
        }
    }

    #[test]
    fn refusals_rate_limits_and_failures_are_the_accounts_own() {
        let failure = |response: HttpResponse| match provider(&Mcp::answering([response]))
            .fetch_outcomes(now())
            .unwrap()
            .remove(0)
        {
            AccountOutcome::Failed { account, error, .. } => {
                assert_eq!(account.as_str(), "gr-1");
                error
            }
            AccountOutcome::Fresh(_) => panic!("a failure"),
        };
        assert!(matches!(
            failure(HttpResponse::new(401, "")),
            ProviderError::Expired { .. }
        ));
        assert_eq!(
            failure(HttpResponse::new(403, "")),
            ProviderError::Refused { reason: NO_PERMISSION }
        );
        let mut limited = HttpResponse::new(429, "");
        limited.retry_after_secs = Some(30);
        assert_eq!(
            failure(limited),
            ProviderError::RateLimited {
                retry_at: now() + Duration::seconds(30)
            }
        );
        assert_eq!(failure(HttpResponse::new(503, "")), ProviderError::Http { status: 503 });
        let rpc = |message: &str| {
            HttpResponse::new(
                200,
                json!({ "jsonrpc": "2.0", "error": { "code": -32000, "message": message } }).to_string(),
            )
        };
        assert!(matches!(failure(rpc("Invalid API key")), ProviderError::Expired { .. }));
        assert!(matches!(
            failure(rpc("Rate limit exceeded")),
            ProviderError::RateLimited { .. }
        ));
        let tool_error = HttpResponse::new(
            200,
            json!({ "result": { "isError": true, "content": [ { "type": "text", "text": "Forbidden" } ] } })
                .to_string(),
        );
        assert_eq!(failure(tool_error), ProviderError::Refused { reason: NO_PERMISSION });
    }

    #[test]
    fn without_a_key_nothing_is_sent() {
        let mcp = Mcp::default();
        let provider = GreptileProvider::new(mcp.clone(), Some("  ".into()), "gr-1", "Acme");
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
        assert!(mcp.requests().is_empty());
    }
}
