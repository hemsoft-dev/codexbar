use std::time::Duration;

use crate::ProviderError;

/// A completed HTTP exchange: any status, with the body read as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
    /// `Retry-After` in seconds, when the server sent one.
    pub retry_after_secs: Option<u64>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            retry_after_secs: None,
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

fn read_response(mut response: ureq::http::Response<ureq::Body>) -> Result<HttpResponse, ProviderError> {
    let status = response.status().as_u16();
    let retry_after_secs = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok());
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| ProviderError::Network)?;
    Ok(HttpResponse {
        status,
        body,
        retry_after_secs,
    })
}
