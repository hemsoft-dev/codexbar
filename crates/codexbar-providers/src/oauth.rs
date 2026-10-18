//! Native-app sign-in (#77): OAuth 2.0 authorization code with PKCE (RFC 7636), through the system browser and a
//! loopback redirect (RFC 8252). Provider-neutral: a provider supplies an `OAuthClient` once its client registration
//! and redirect are approved (`docs/OAUTH.md`).
//!
//! - The callback listener binds only to 127.0.0.1 (or ::1) on a port the system picks, and lives only for one sign-in.
//! - Requests that aren't the expected callback (another path, a wrong `state`, garbage) are answered and ignored, so
//!   another local process can't end or hijack a sign-in; only the matching `state` completes it.
//! - Errors never carry tokens, codes or response bodies: a provider's error is reduced to its short code.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::HttpClient;

/// One provider's approved native-app registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClient {
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    /// The path of the loopback redirect, such as `/callback`.
    pub redirect_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OAuthError {
    /// The system browser couldn't be opened.
    Browser,
    /// No loopback port could be opened.
    Listener,
    Timeout,
    Cancelled,
    /// The user or the provider declined, with the provider's short error code.
    Denied(String),
    /// The token endpoint answered with this status.
    Exchange {
        status: u16,
    },
    /// The token endpoint answered something that isn't a token response.
    InvalidResponse,
    Network,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Browser => f.write_str("The browser couldn't be opened for sign-in."),
            Self::Listener => f.write_str("CodexBar couldn't listen for the sign-in to finish."),
            Self::Timeout => f.write_str("Sign-in took too long and was stopped."),
            Self::Cancelled => f.write_str("Sign-in was cancelled."),
            Self::Denied(code) => write!(f, "Sign-in was declined ({code})."),
            Self::Exchange { status } => write!(f, "The sign-in couldn't be completed (HTTP {status})."),
            Self::InvalidResponse => f.write_str("The sign-in service sent an unexpected answer."),
            Self::Network => f.write_str("The sign-in service couldn't be reached."),
        }
    }
}

impl std::error::Error for OAuthError {}

/// What a sign-in or renewal yields. Kept in Windows Credential Manager through `to_secret`; never logged (`Debug`
/// redacts the tokens).
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl TokenSet {
    /// The form stored as the account's secret.
    pub fn to_secret(&self) -> String {
        json!({
            "accessToken": self.access_token,
            "refreshToken": self.refresh_token,
            "expiresAt": self.expires_at.map(|at| at.to_rfc3339()),
        })
        .to_string()
    }

    pub fn from_secret(secret: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(secret).ok()?;
        Some(Self {
            access_token: value.get("accessToken")?.as_str()?.to_owned(),
            refresh_token: value.get("refreshToken").and_then(Value::as_str).map(str::to_owned),
            expires_at: value
                .get("expiresAt")
                .and_then(Value::as_str)
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&Utc)),
        })
    }

    /// True when the access token expires within `margin` of `now`, so it should be renewed first.
    pub fn expires_within(&self, margin: chrono::Duration, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at - margin <= now)
    }
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    pub fn new() -> Result<Self, OAuthError> {
        Ok(Self::from_verifier(random_token(32)?))
    }

    fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// Opens a URL in the user's browser.
pub trait Browser {
    fn open(&self, url: &str) -> Result<(), OAuthError>;
}

/// The default browser (`platform::open_url`; no shell parses the URL).
pub struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), OAuthError> {
        crate::platform::open_url(url).map_err(|_| OAuthError::Browser)
    }
}

/// One sign-in in progress: the listener, PKCE pair and state, and the URL to open.
pub struct Authorization {
    listener: TcpListener,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    redirect_path: String,
    url: String,
}

impl Authorization {
    /// Opens the loopback listener on a port the system picks and builds the authorization URL.
    pub fn begin(client: &OAuthClient) -> Result<Self, OAuthError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .or_else(|_| TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0))))
            .map_err(|_| OAuthError::Listener)?;
        let address = listener.local_addr().map_err(|_| OAuthError::Listener)?;
        let host = match address {
            SocketAddr::V4(_) => "127.0.0.1".to_owned(),
            SocketAddr::V6(_) => "[::1]".to_owned(),
        };
        let redirect_path = if client.redirect_path.starts_with('/') {
            client.redirect_path.clone()
        } else {
            format!("/{}", client.redirect_path)
        };
        let redirect_uri = format!("http://{host}:{}{redirect_path}", address.port());
        let pkce = Pkce::new()?;
        let state = random_token(24)?;
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", &client.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", pkce.challenge())
            .append_pair("code_challenge_method", "S256")
            .finish();
        let separator = if client.authorize_url.contains('?') { '&' } else { '?' };
        let url = format!("{}{separator}{query}", client.authorize_url);
        Ok(Self {
            listener,
            pkce,
            state,
            redirect_uri,
            redirect_path,
            url,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.listener.local_addr().ok()
    }

    /// Waits for the callback with the matching state and returns its authorization code, with the PKCE verifier the
    /// exchange needs. Stops at `timeout` or when `cancel` is set, including while a slow connection is being read.
    /// The browser's answer for the matching callback waits until `Callback::finish`, so it can say how sign-in ended.
    pub fn wait(self, timeout: Duration, cancel: &AtomicBool) -> Result<Callback, OAuthError> {
        self.listener.set_nonblocking(true).map_err(|_| OAuthError::Listener)?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(OAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(OAuthError::Timeout);
            }
            match self.listener.accept() {
                Ok((stream, peer)) if peer.ip().is_loopback() => match self.handle(stream, deadline, cancel) {
                    Handled::Ignored => {}
                    Handled::Stopped(err) => return Err(err),
                    Handled::Code(code, stream) => {
                        return Ok(Callback {
                            code,
                            verifier: self.pkce.verifier.clone(),
                            redirect_uri: self.redirect_uri.clone(),
                            reply: Some(stream),
                        });
                    }
                },
                // Bound to loopback only; anything else is dropped unanswered.
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }

    /// Reads and answers one request.
    fn handle(&self, mut stream: TcpStream, deadline: Instant, cancel: &AtomicBool) -> Handled {
        let Some(head) = read_head(&mut stream, deadline, cancel) else {
            if cancel.load(Ordering::SeqCst) {
                return Handled::Stopped(OAuthError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Handled::Stopped(OAuthError::Timeout);
            }
            respond(&mut stream, 400, "This isn't a sign-in request.");
            return Handled::Ignored;
        };
        let target = head
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|rest| rest.split(' ').next());
        let Some(target) = target else {
            respond(&mut stream, 400, "This isn't a sign-in request.");
            return Handled::Ignored;
        };
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != self.redirect_path {
            respond(&mut stream, 404, "Not found.");
            return Handled::Ignored;
        }
        let pairs: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        if !get("state").is_some_and(|state| constant_time_eq(state.as_bytes(), self.state.as_bytes())) {
            respond(
                &mut stream,
                400,
                "This sign-in link doesn't match. Start the sign-in again from CodexBar.",
            );
            return Handled::Ignored;
        }
        if let Some(error) = get("error") {
            respond(&mut stream, 200, "Sign-in was declined. You can close this window.");
            return Handled::Stopped(OAuthError::Denied(short_code(error)));
        }
        match get("code").filter(|code| !code.is_empty()) {
            Some(code) => Handled::Code(code.to_owned(), stream),
            None => {
                respond(&mut stream, 400, "The sign-in answer was incomplete.");
                Handled::Ignored
            }
        }
    }
}

enum Handled {
    /// Not the callback: answered, keep waiting.
    Ignored,
    /// The sign-in ends without a code.
    Stopped(OAuthError),
    /// The callback's code, with its connection still open for the final answer.
    Code(String, TcpStream),
}

/// The longest one request's head may take to arrive, so one slow connection can't hold the listener.
const HEAD_READ_LIMIT: Duration = Duration::from_secs(3);

/// Reads a request head, giving up at the sign-in's deadline, on cancel, or after `HEAD_READ_LIMIT`, however slowly
/// the bytes trickle in. `None` when no complete head arrived.
fn read_head(stream: &mut TcpStream, deadline: Instant, cancel: &AtomicBool) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let stop = deadline.min(Instant::now() + HEAD_READ_LIMIT);
    let mut buffer = [0u8; 8192];
    let mut read = 0;
    while read < buffer.len() {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        let left = stop.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        // Short reads, so the deadline and cancel are checked between them.
        let _ = stream.set_read_timeout(Some(left.min(Duration::from_millis(200))));
        match stream.read(&mut buffer[read..]) {
            Ok(0) => break,
            Ok(n) => {
                read += n;
                if buffer[..read].windows(4).any(|end| end == b"\r\n\r\n") {
                    return Some(String::from_utf8_lossy(&buffer[..read]).into_owned());
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    (read > 0).then(|| String::from_utf8_lossy(&buffer[..read]).into_owned())
}

/// What a successful callback carries into the token exchange. The browser is told the outcome by `finish`, once the
/// exchange is done; dropped unfinished, it's told to return to CodexBar.
pub struct Callback {
    pub code: String,
    pub verifier: String,
    pub redirect_uri: String,
    reply: Option<TcpStream>,
}

impl Callback {
    /// Tells the browser how sign-in ended.
    pub fn finish(&mut self, signed_in: bool) {
        if let Some(mut stream) = self.reply.take() {
            if signed_in {
                respond(
                    &mut stream,
                    200,
                    "Signed in. You can close this window and return to CodexBar.",
                );
            } else {
                respond(
                    &mut stream,
                    200,
                    "Sign-in couldn't be completed. Return to CodexBar to see why.",
                );
            }
        }
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(mut stream) = self.reply.take() {
            respond(&mut stream, 200, "Return to CodexBar to finish signing in.");
        }
    }
}

/// Exchanges an authorization code for tokens.
pub fn exchange(
    http: &impl HttpClient,
    client: &OAuthClient,
    callback: &Callback,
    now: DateTime<Utc>,
) -> Result<TokenSet, OAuthError> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", &callback.code)
        .append_pair("redirect_uri", &callback.redirect_uri)
        .append_pair("client_id", &client.client_id)
        .append_pair("code_verifier", &callback.verifier)
        .finish();
    token_request(http, client, &body, None, now)
}

/// Renews tokens with a refresh token. A provider that doesn't rotate refresh tokens keeps the old one.
pub fn refresh(
    http: &impl HttpClient,
    client: &OAuthClient,
    refresh_token: &str,
    now: DateTime<Utc>,
) -> Result<TokenSet, OAuthError> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("client_id", &client.client_id)
        .finish();
    token_request(http, client, &body, Some(refresh_token), now)
}

/// The whole sign-in: open the browser, wait for the callback, exchange the code, then tell the browser the outcome.
pub fn sign_in(
    http: &impl HttpClient,
    browser: &dyn Browser,
    client: &OAuthClient,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<TokenSet, OAuthError> {
    let authorization = Authorization::begin(client)?;
    browser.open(authorization.url())?;
    let mut callback = authorization.wait(timeout, cancel)?;
    let tokens = exchange(http, client, &callback, Utc::now());
    callback.finish(tokens.is_ok());
    tokens
}

fn token_request(
    http: &impl HttpClient,
    client: &OAuthClient,
    body: &str,
    previous_refresh: Option<&str>,
    now: DateTime<Utc>,
) -> Result<TokenSet, OAuthError> {
    let headers = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Accept", "application/json"),
    ];
    let response = http
        .post_json(&client.token_url, &headers, body)
        .map_err(|_| OAuthError::Network)?;
    if !(200..300).contains(&response.status) {
        // The body is never shown: token endpoints echo request details.
        return Err(OAuthError::Exchange {
            status: response.status,
        });
    }
    let value: Value = serde_json::from_str(&response.body).map_err(|_| OAuthError::InvalidResponse)?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(OAuthError::InvalidResponse)?
        .to_owned();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .or_else(|| previous_refresh.map(str::to_owned));
    // Only Bearer tokens are understood (RFC 6749 section 7.1). The type is required, but some providers leave it out
    // of refresh responses; a missing type is read as Bearer, any other type is refused.
    if value
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
    {
        return Err(OAuthError::InvalidResponse);
    }
    // A lifetime that can't be a date is a malformed answer, not a reason to crash.
    let expires_at = match value
        .get("expires_in")
        .and_then(|seconds| seconds.as_i64().or_else(|| seconds.as_str()?.parse().ok()))
    {
        // A negative lifetime is malformed; zero means the token is already expired, so it is renewed at once.
        Some(seconds) if seconds < 0 => return Err(OAuthError::InvalidResponse),
        Some(seconds) => Some(
            chrono::Duration::try_seconds(seconds)
                .and_then(|lifetime| now.checked_add_signed(lifetime))
                .ok_or(OAuthError::InvalidResponse)?,
        ),
        None => None,
    };
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at,
    })
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>CodexBar</title><p style=\"font-family:Segoe UI,sans-serif\">{message}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// A provider's error parameter, kept only if it's a short code (`access_denied`); descriptions are dropped.
fn short_code(error: &str) -> String {
    let code: String = error
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .take(40)
        .collect();
    if code.is_empty() { "error".to_owned() } else { code }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_token(bytes: usize) -> Result<String, OAuthError> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| OAuthError::Listener)?;
    Ok(base64url(&buffer))
}

/// Base64url without padding (RFC 4648 §5).
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpResponse, ProviderError};
    use std::sync::Mutex;

    fn client() -> OAuthClient {
        OAuthClient {
            authorize_url: "https://auth.example/authorize".into(),
            token_url: "https://auth.example/token".into(),
            client_id: "codexbar-test".into(),
            scopes: vec!["usage.read".into(), "offline_access".into()],
            redirect_path: "/callback".into(),
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-09T12:00:00Z".parse().unwrap()
    }

    /// Sends one raw request to the listener and returns the response's status line.
    fn send(addr: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response.lines().next().unwrap_or_default().to_owned()
    }

    fn get(path_and_query: &str) -> String {
        format!("GET {path_and_query} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    fn query_value(url: &str, key: &str) -> String {
        let query = url.split_once('?').unwrap().1;
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .find(|(name, _)| name == key)
            .unwrap()
            .1
    }

    #[test]
    fn pkce_matches_the_rfc_7636_example() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(pkce.challenge(), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let fresh = Pkce::new().unwrap();
        assert!((43..=128).contains(&fresh.verifier.len()), "RFC 7636 verifier length");
        assert!(fresh != Pkce::new().unwrap(), "random each time");
    }

    #[test]
    fn the_listener_is_loopback_only_on_a_system_chosen_port() {
        let authorization = Authorization::begin(&client()).unwrap();
        let address = authorization.local_addr().unwrap();
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        let url = authorization.url();
        assert!(url.starts_with("https://auth.example/authorize?response_type=code&"));
        assert_eq!(query_value(url, "code_challenge_method"), "S256");
        assert_eq!(query_value(url, "client_id"), "codexbar-test");
        assert_eq!(query_value(url, "scope"), "usage.read offline_access");
        // 127.0.0.1, or [::1] where only IPv6 loopback is available (RFC 8252 section 7.3).
        let host = if address.is_ipv4() {
            "127.0.0.1".to_owned()
        } else {
            format!("[{}]", address.ip())
        };
        assert_eq!(
            query_value(url, "redirect_uri"),
            format!("http://{host}:{}/callback", address.port())
        );
        // Two sign-ins never share a port.
        let other = Authorization::begin(&client()).unwrap();
        assert_ne!(other.local_addr().unwrap().port(), address.port());
    }

    #[test]
    fn hostile_and_malformed_requests_are_ignored_until_the_real_callback() {
        let authorization = Authorization::begin(&client()).unwrap();
        let addr = authorization.local_addr().unwrap();
        let state = query_value(authorization.url(), "state");
        let cancel = AtomicBool::new(false);
        let probe = std::thread::spawn(move || {
            let statuses = vec![
                send(addr, "garbage\r\n\r\n"),
                send(addr, &get("/favicon.ico")),
                send(addr, &get("/callback?code=stolen&state=wrong")),
                send(addr, &get("/callback?code=nostate")),
                send(addr, &get(&format!("/callback?state={state}"))),
                send(addr, &get(&format!("/callback?code=the-code&state={state}"))),
            ];
            statuses
        });
        let callback = authorization.wait(Duration::from_secs(10), &cancel).unwrap();
        assert_eq!(callback.code, "the-code");
        // The success connection stays open for the final answer; dropping the callback sends it.
        drop(callback);
        let statuses = probe.join().unwrap();
        assert!(statuses[0].contains("400"), "{statuses:?}");
        assert!(statuses[1].contains("404"));
        assert!(statuses[2].contains("400"), "a wrong state never completes the sign-in");
        assert!(statuses[3].contains("400"));
        assert!(statuses[4].contains("400"), "no code");
        assert!(statuses[5].contains("200"));
    }

    #[test]
    fn a_refusal_ends_the_sign_in_with_only_the_error_code() {
        let authorization = Authorization::begin(&client()).unwrap();
        let addr = authorization.local_addr().unwrap();
        let state = query_value(authorization.url(), "state");
        let cancel = AtomicBool::new(false);
        let probe = std::thread::spawn(move || {
            send(
                addr,
                &get(&format!(
                    "/callback?error=access_denied&error_description=%3Cscript%3E&state={state}"
                )),
            )
        });
        let result = authorization.wait(Duration::from_secs(10), &cancel);
        probe.join().unwrap();
        let err = result.err().unwrap();
        assert_eq!(err, OAuthError::Denied("access_denied".into()));
        assert_eq!(err.to_string(), "Sign-in was declined (access_denied).");
    }

    #[test]
    fn waiting_stops_at_the_timeout_and_on_cancel() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let result = Authorization::begin(&client())
            .unwrap()
            .wait(Duration::from_millis(150), &cancel);
        assert_eq!(result.err(), Some(OAuthError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
        cancel.store(true, Ordering::SeqCst);
        let result = Authorization::begin(&client())
            .unwrap()
            .wait(Duration::from_secs(30), &cancel);
        assert_eq!(result.err(), Some(OAuthError::Cancelled));
    }

    struct Token {
        status: u16,
        body: &'static str,
        sent: Mutex<Vec<(String, String)>>,
    }

    impl Token {
        fn new(status: u16, body: &'static str) -> Self {
            Self {
                status,
                body,
                sent: Mutex::default(),
            }
        }
    }

    impl HttpClient for Token {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            unreachable!()
        }

        fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<HttpResponse, ProviderError> {
            assert_eq!(url, "https://auth.example/token");
            assert!(headers.contains(&("Content-Type", "application/x-www-form-urlencoded")));
            *self.sent.lock().unwrap() = form_urlencoded::parse(body.as_bytes()).into_owned().collect();
            Ok(HttpResponse::new(self.status, self.body))
        }
    }

    fn callback() -> Callback {
        Callback {
            code: "the-code".into(),
            verifier: "the-verifier".into(),
            redirect_uri: "http://127.0.0.1:5000/callback".into(),
            reply: None,
        }
    }

    #[test]
    fn the_code_is_exchanged_with_its_verifier() {
        let http = Token::new(
            200,
            r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"token_type":"Bearer"}"#,
        );
        let tokens = exchange(&http, &client(), &callback(), now()).unwrap();
        assert!(tokens.access_token == "at-1" && tokens.refresh_token.as_deref() == Some("rt-1"));
        assert_eq!(tokens.expires_at, Some(now() + chrono::Duration::hours(1)));
        let sent = http.sent.lock().unwrap().clone();
        let field = |key: &str| {
            sent.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(field("grant_type").as_deref(), Some("authorization_code"));
        assert_eq!(field("code_verifier").as_deref(), Some("the-verifier"));
        assert_eq!(field("redirect_uri").as_deref(), Some("http://127.0.0.1:5000/callback"));
        assert_eq!(field("client_id").as_deref(), Some("codexbar-test"));
    }

    #[test]
    fn token_errors_never_show_the_response_body() {
        let http = Token::new(400, r#"{"error":"invalid_grant","echo":"code=the-code"}"#);
        let err = exchange(&http, &client(), &callback(), now()).err().unwrap();
        assert_eq!(err, OAuthError::Exchange { status: 400 });
        assert!(!err.to_string().contains("the-code"));
        let http = Token::new(200, r#"{"token_type":"Bearer"}"#);
        assert_eq!(
            exchange(&http, &client(), &callback(), now()).err(),
            Some(OAuthError::InvalidResponse)
        );
    }

    #[test]
    fn unusable_token_types_and_lifetimes_are_refused() {
        let dpop = Token::new(200, r#"{"access_token":"at","token_type":"DPoP"}"#);
        assert_eq!(
            exchange(&dpop, &client(), &callback(), now()).err(),
            Some(OAuthError::InvalidResponse)
        );
        let huge = Token::new(
            200,
            r#"{"access_token":"at","token_type":"bearer","expires_in":9223372036854775807}"#,
        );
        assert_eq!(
            exchange(&huge, &client(), &callback(), now()).err(),
            Some(OAuthError::InvalidResponse)
        );
        let negative = Token::new(200, r#"{"access_token":"at","expires_in":-5}"#);
        assert_eq!(
            exchange(&negative, &client(), &callback(), now()).err(),
            Some(OAuthError::InvalidResponse)
        );
        // Zero is "already expired": renew before use, never "doesn't expire".
        let zero = Token::new(200, r#"{"access_token":"at","expires_in":0}"#);
        let tokens = exchange(&zero, &client(), &callback(), now()).unwrap();
        assert_eq!(tokens.expires_at, Some(now()));
        assert!(tokens.expires_within(chrono::Duration::zero(), now()));
    }

    #[test]
    fn a_slow_connection_cannot_hold_the_sign_in_past_its_deadline() {
        let authorization = Authorization::begin(&client()).unwrap();
        let addr = authorization.local_addr().unwrap();
        let cancel = AtomicBool::new(false);
        // Drips one byte at a time, never finishing a request.
        let dripper = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            for _ in 0..40 {
                if stream.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let result = authorization.wait(Duration::from_millis(600), &cancel);
        assert_eq!(result.err(), Some(OAuthError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        dripper.join().unwrap();
    }

    #[test]
    fn the_browser_hears_success_only_after_the_exchange() {
        let authorization = Authorization::begin(&client()).unwrap();
        let addr = authorization.local_addr().unwrap();
        let state = query_value(authorization.url(), "state");
        let probe = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(get(&format!("/callback?code=c&state={state}")).as_bytes())
                .unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        });
        let mut callback = authorization
            .wait(Duration::from_secs(10), &AtomicBool::new(false))
            .unwrap();
        let failing = Token::new(500, "{}");
        let result = exchange(&failing, &client(), &callback, now());
        callback.finish(result.is_ok());
        let page = probe.join().unwrap();
        assert!(page.contains("couldn't be completed"), "{page}");
        assert!(!page.contains("Signed in"));
    }

    #[test]
    fn renewal_keeps_the_old_refresh_token_unless_rotated() {
        let http = Token::new(200, r#"{"access_token":"at-2","expires_in":"60"}"#);
        let tokens = refresh(&http, &client(), "rt-1", now()).unwrap();
        assert!(tokens.refresh_token.as_deref() == Some("rt-1"));
        assert!(tokens.expires_within(chrono::Duration::minutes(5), now()));
        let rotated = Token::new(200, r#"{"access_token":"at-3","refresh_token":"rt-2"}"#);
        let tokens = refresh(&rotated, &client(), "rt-1", now()).unwrap();
        assert!(tokens.refresh_token.as_deref() == Some("rt-2"));
        assert_eq!(tokens.expires_at, None);
    }

    #[test]
    fn tokens_round_trip_as_a_secret_and_never_print() {
        let tokens = TokenSet {
            access_token: "at-secret".into(),
            refresh_token: Some("rt-secret".into()),
            expires_at: Some(now()),
        };
        assert!(TokenSet::from_secret(&tokens.to_secret()) == Some(tokens.clone()));
        let printed = format!("{tokens:?}");
        assert!(!printed.contains("at-secret") && !printed.contains("rt-secret"));
        assert_eq!(TokenSet::from_secret("not json"), None);
    }

    struct FakeBrowser(Mutex<Option<String>>);

    impl Browser for FakeBrowser {
        fn open(&self, url: &str) -> Result<(), OAuthError> {
            // Plays the provider: redirect straight back with a code for the same state.
            let redirect = query_value(url, "redirect_uri");
            let state = query_value(url, "state");
            let target = redirect.split_once("://").unwrap().1;
            let (host, path) = target.split_once('/').unwrap();
            let addr: SocketAddr = host.parse().unwrap();
            let request = get(&format!("/{path}?code=browser-code&state={state}"));
            std::thread::spawn(move || {
                send(addr, &request);
            });
            *self.0.lock().unwrap() = Some(url.to_owned());
            Ok(())
        }
    }

    #[test]
    fn a_whole_sign_in_opens_the_browser_and_exchanges_its_code() {
        let http = Token::new(
            200,
            r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}"#,
        );
        let browser = FakeBrowser(Mutex::default());
        let tokens = sign_in(
            &http,
            &browser,
            &client(),
            Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(tokens.access_token == "at-1");
        let sent = http.sent.lock().unwrap().clone();
        assert!(
            sent.iter()
                .any(|(name, value)| name == "code" && value == "browser-code")
        );
        assert!(browser.0.lock().unwrap().is_some());
    }

    struct BrokenBrowser;

    impl Browser for BrokenBrowser {
        fn open(&self, _: &str) -> Result<(), OAuthError> {
            Err(OAuthError::Browser)
        }
    }

    #[test]
    fn a_browser_that_will_not_open_fails_at_once() {
        let http = Token::new(200, "{}");
        let result = sign_in(
            &http,
            &BrokenBrowser,
            &client(),
            Duration::from_secs(30),
            &AtomicBool::new(false),
        );
        assert_eq!(result.err(), Some(OAuthError::Browser));
    }

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }
}
