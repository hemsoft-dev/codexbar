use std::time::Duration;

use crate::ProviderError;

/// A completed HTTP exchange: any status, with the body read as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
    /// `Retry-After` in seconds, when the server sent one.
    pub retry_after_secs: Option<u64>,
    /// When GitHub's primary rate limit resets, when it is used up (`x-ratelimit-remaining: 0` with
    /// `x-ratelimit-reset`, in epoch seconds).
    pub rate_limit_reset: Option<chrono::DateTime<chrono::Utc>>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            retry_after_secs: None,
            rate_limit_reset: None,
        }
    }
}

/// The one HTTP operation providers need. A seam so parsers and status handling are tested without the network.
pub trait HttpClient: Send + Sync {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError>;

    /// POSTs a JSON body. Only Cursor needs it, so test doubles that never post can skip it.
    fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
        let _ = (url, headers, body);
        Err(ProviderError::Network)
    }

    /// POSTs `fields` as an `application/x-www-form-urlencoded` body, as OAuth token endpoints expect (#102).
    fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        fields: &[(&str, &str)],
    ) -> Result<HttpResponse, ProviderError> {
        let _ = (url, headers, fields);
        Err(ProviderError::Network)
    }
}

/// The form-encoded body for `fields`.
pub fn form_body(fields: &[(&str, &str)]) -> String {
    let mut body = form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        body.append_pair(name, value);
    }
    body.finish()
}

/// Blocking HTTPS client with a short timeout. Non-2xx statuses are returned, not raised.
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        Self::build(10)
    }

    /// Returns redirects as responses, for dashboards that bounce an expired session to a login page.
    pub fn without_redirects() -> Self {
        Self::build(0)
    }

    fn build(max_redirects: u32) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .max_redirects(max_redirects)
            .user_agent("CodexBar")
            .build();
        Self { agent: config.into() }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqClient {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.call().map_err(|_| ProviderError::Network)?;
        read_response(response)
    }

    fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
        let mut request = self.agent.post(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send(body).map_err(|_| ProviderError::Network)?;
        read_response(response)
    }

    fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        fields: &[(&str, &str)],
    ) -> Result<HttpResponse, ProviderError> {
        let mut request = self
            .agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send(form_body(fields)).map_err(|_| ProviderError::Network)?;
        read_response(response)
    }
}

/// A `Retry-After` value in seconds from `now`: either form RFC 9110 allows, a number of seconds or an HTTP date
/// (`Tue, 13 Oct 2026 06:00:00 GMT`). A date already past is no wait.
pub fn retry_after(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(secs);
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some((at.with_timezone(&chrono::Utc) - now).num_seconds().max(0) as u64)
}

fn read_response(mut response: ureq::http::Response<ureq::Body>) -> Result<HttpResponse, ProviderError> {
    let status = response.status().as_u16();
    let retry_after_secs = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| retry_after(value, chrono::Utc::now()));
    let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
    let rate_limit_reset = rate_limit_reset(header("x-ratelimit-remaining"), header("x-ratelimit-reset"));
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| ProviderError::Network)?;
    Ok(HttpResponse {
        status,
        body,
        retry_after_secs,
        rate_limit_reset,
    })
}

/// When a used-up primary rate limit resets: GitHub's `x-ratelimit-reset` (epoch seconds), only when
/// `x-ratelimit-remaining` is 0.
fn rate_limit_reset(remaining: Option<&str>, reset: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    if remaining?.trim() != "0" {
        return None;
    }
    chrono::DateTime::from_timestamp(reset?.trim().parse::<i64>().ok()?, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_used_up_primary_rate_limit_says_when_it_resets() {
        assert_eq!(
            rate_limit_reset(Some("0"), Some("1792578000")),
            chrono::DateTime::from_timestamp(1_792_578_000, 0)
        );
        assert_eq!(rate_limit_reset(Some("12"), Some("1792578000")), None, "not used up");
        assert_eq!(rate_limit_reset(Some("0"), None), None);
        assert_eq!(rate_limit_reset(None, Some("1792578000")), None);
    }

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        let now: chrono::DateTime<chrono::Utc> = "2026-10-13T05:59:00Z".parse().unwrap();
        assert_eq!(retry_after("120", now), Some(120));
        assert_eq!(retry_after(" 7 ", now), Some(7));
        assert_eq!(retry_after("Tue, 13 Oct 2026 06:00:00 GMT", now), Some(60));
        assert_eq!(
            retry_after("Tue, 13 Oct 2026 05:00:00 GMT", now),
            Some(0),
            "a past date is no wait"
        );
        assert_eq!(retry_after("soon", now), None);
        assert_eq!(retry_after("-5", now), None);
    }
}
