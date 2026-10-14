//! OpenCode Go limits and Zen balance through the OpenCode Console (#102), signed in with the Console's device grant
//! instead of a copied dashboard cookie. The user approves CodexBar on the Console's own device page in their browser
//! and picks the workspace there; CodexBar never sees a password, a browser cookie or the page.
//!
//! These are OpenCode's first-party Console endpoints, not a published API. A changed shape fails visibly; nothing
//! falls back to cookies. Every request stays on the Console origin, and redirects are treated as failures.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Money, Provider};
use serde_json::{Value, json};

use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

pub const CONSOLE: &str = "https://opencode.ai/console/";
/// CodexBar for Windows' own public client label in the device grant. The iOS app has its own; it is never borrowed.
pub const CLIENT_ID: &str = "codexbar-windows";
const DEVICE_PAGE: &str = "https://opencode.ai/console/device";
const SECRET_KIND: &str = "opencode-console-v1";
const SIGN_IN_HINT: &str = "Sign in to OpenCode in CodexBar settings.";
/// A token is renewed when it expires within this long.
const RENEW_MARGIN: Duration = Duration::seconds(60);
/// The Console counts money in microcents: 100,000,000 to the dollar.
const MICROCENTS_PER_CENT: i128 = 1_000_000;
/// The longest token lifetime accepted: a changed or broken `expires_in` fails as a shape, never overflows a date.
const MAX_TOKEN_SECONDS: f64 = 366.0 * 24.0 * 3600.0;
/// How much longer to wait between polls after the Console says to slow down (RFC 8628 section 3.5).
const SLOW_DOWN: StdDuration = StdDuration::from_secs(5);

/// One account's Console sign-in, saved in Windows Credential Manager. It names the workspace and user it was approved
/// for, so a renewal or a later sign-in can never move the account to someone else.
#[derive(Clone, PartialEq, Eq)]
pub struct ConsoleCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub workspace_id: String,
    pub user_id: String,
}

impl std::fmt::Debug for ConsoleCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleCredential")
            .field("expires_at", &self.expires_at)
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}

impl ConsoleCredential {
    pub fn to_secret(&self) -> String {
        json!({
            "kind": SECRET_KIND,
            "accessToken": self.access_token,
            "refreshToken": self.refresh_token,
            "expiresAt": self.expires_at.timestamp(),
            "workspaceId": self.workspace_id,
            "userId": self.user_id,
        })
        .to_string()
    }

    /// A saved sign-in, or `None` for anything else (a dashboard cookie, a damaged entry).
    pub fn from_secret(secret: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(secret).ok()?;
        if value.get("kind")?.as_str()? != SECRET_KIND {
            return None;
        }
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let credential = Self {
            access_token: text("accessToken").filter(|t| is_token(t))?,
            refresh_token: text("refreshToken").filter(|t| is_token(t))?,
            expires_at: DateTime::from_timestamp(value.get("expiresAt")?.as_i64()?, 0)?,
            workspace_id: text("workspaceId").filter(|w| valid_workspace(w))?,
            user_id: text("userId").filter(|u| !u.trim().is_empty())?,
        };
        Some(credential)
    }

    fn expires_soon(&self, now: DateTime<Utc>) -> bool {
        self.expires_at - now <= RENEW_MARGIN
    }
}

fn is_token(token: &str) -> bool {
    !token.is_empty() && !token.chars().any(char::is_whitespace)
}

/// An OpenCode workspace id: `wrk_` or `org_` and letters and digits.
pub fn valid_workspace(id: &str) -> bool {
    id.strip_prefix("wrk_")
        .or_else(|| id.strip_prefix("org_"))
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Why a sign-in ended without a saved account. Messages are user-facing and never contain a code or token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    Cancelled,
    Denied,
    Expired,
    Network,
    /// The Console answered in a shape CodexBar doesn't know (a changed contract, an unexpected page).
    Unexpected,
    /// A token arrived, but its workspace or user couldn't be confirmed.
    Unverified,
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "The sign-in was cancelled.",
            Self::Denied => "The approval was declined on the OpenCode page. Nothing was saved.",
            Self::Expired => "The approval wasn't completed in time. Try again for a new code.",
            Self::Network => "Couldn't reach OpenCode. Check your connection and try again.",
            Self::Unexpected => "OpenCode answered in a way CodexBar doesn't recognize. Nothing was saved.",
            Self::Unverified => "OpenCode's approval couldn't be verified. Nothing was saved.",
        })
    }
}

impl std::error::Error for SignInError {}

/// A device approval waiting for the user: the page to approve it on and the code that page shows.
#[derive(Clone)]
pub struct DeviceChallenge {
    device_code: String,
    pub user_code: Option<String>,
    pub url: String,
    pub expires_at: DateTime<Utc>,
    pub interval: StdDuration,
}

impl std::fmt::Debug for DeviceChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceChallenge")
            .field("expires_at", &self.expires_at)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// The device flow takes form-encoded fields, as OpenCode documents it (and RFC 8628 specifies).
fn post(http: &impl HttpClient, path: &str, fields: &[(&str, &str)]) -> Result<crate::HttpResponse, SignInError> {
    let url = format!("{CONSOLE}{path}");
    http.post_form(&url, &[("Accept", "application/json")], fields)
        .map_err(|_| SignInError::Network)
}

/// Asks the Console for a device approval with CodexBar's own client label, allowing a workspace (org) choice.
pub fn begin(http: &impl HttpClient, now: DateTime<Utc>) -> Result<DeviceChallenge, SignInError> {
    let response = post(
        http,
        "auth/device/code",
        &[("client_id", CLIENT_ID), ("supports_org_scope", "true")],
    )?;
    if response.status != 200 {
        return Err(SignInError::Unexpected);
    }
    let value: Value = serde_json::from_str(&response.body).map_err(|_| SignInError::Unexpected)?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let device_code = text("device_code")
        .filter(|c| is_token(c))
        .ok_or(SignInError::Unexpected)?;
    let url = text("verification_uri_complete")
        .and_then(verification_url)
        .ok_or(SignInError::Unexpected)?;
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0 && *s <= 900.0)
        .ok_or(SignInError::Unexpected)?;
    let interval = value
        .get("interval")
        .and_then(Value::as_f64)
        .filter(|s| *s > 0.0 && *s <= 60.0)
        .ok_or(SignInError::Unexpected)?;
    let user_code = text("user_code")
        .filter(|code| code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_owned);
    Ok(DeviceChallenge {
        device_code: device_code.to_owned(),
        user_code,
        url,
        expires_at: now + Duration::seconds(expires_in as i64),
        interval: StdDuration::from_secs_f64(interval.max(5.0)),
    })
}

/// The approval page, only when it is exactly the Console's device page over HTTPS. The Console sends it relative.
pub fn verification_url(address: &str) -> Option<String> {
    let url = if address.starts_with('/') {
        format!("https://opencode.ai{address}")
    } else {
        address.to_owned()
    };
    let rest = url.strip_prefix(DEVICE_PAGE)?;
    let clean = !url.contains('#') && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    ((rest.is_empty() || rest.starts_with('?')) && clean).then_some(url)
}

enum Poll {
    Pending { slow_down: bool },
    Approved(String),
}

fn poll(http: &impl HttpClient, challenge: &DeviceChallenge) -> Result<Poll, SignInError> {
    let response = post(
        http,
        "auth/device/token",
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &challenge.device_code),
            ("client_id", CLIENT_ID),
        ],
    )?;
    match response.status {
        200 => Ok(Poll::Approved(response.body)),
        400 => {
            let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => Ok(Poll::Pending { slow_down: false }),
                Some("slow_down") => Ok(Poll::Pending { slow_down: true }),
                Some("access_denied") => Err(SignInError::Denied),
                Some("expired_token") => Err(SignInError::Expired),
                _ => Err(SignInError::Unexpected),
            }
        }
        _ => Err(SignInError::Unexpected),
    }
}

/// Sleeps up to `duration`, waking early when `cancel` is set.
pub fn cancellable_sleep(duration: StdDuration, cancel: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    while !cancel.load(Ordering::SeqCst) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(StdDuration::from_millis(100)));
    }
}

/// Polls until the user approves on the Console page, then confirms the workspace and user the token belongs to. The
/// Console's interval is honored and grows on `slow_down`; no poll is sent after the code expires or once `cancel` is
/// set. `clock` and `sleep` are seams for tests.
pub fn wait_for_approval(
    http: &impl HttpClient,
    challenge: &DeviceChallenge,
    cancel: &AtomicBool,
    clock: &dyn Fn() -> DateTime<Utc>,
    sleep: &dyn Fn(StdDuration, &AtomicBool),
) -> Result<ConsoleCredential, SignInError> {
    let mut interval = challenge.interval;
    loop {
        let left = (challenge.expires_at - clock()).to_std().unwrap_or_default();
        if left.is_zero() {
            return Err(SignInError::Expired);
        }
        sleep(interval.min(left), cancel);
        if cancel.load(Ordering::SeqCst) {
            return Err(SignInError::Cancelled);
        }
        if clock() >= challenge.expires_at {
            return Err(SignInError::Expired);
        }
        match poll(http, challenge)? {
            Poll::Pending { slow_down } => {
                if slow_down {
                    interval += SLOW_DOWN;
                }
            }
            Poll::Approved(body) => {
                let issued_at = clock();
                if cancel.load(Ordering::SeqCst) {
                    return Err(SignInError::Cancelled);
                }
                return verify(http, &body, issued_at);
            }
        }
    }
}

/// A token response from the device or refresh grant.
struct Grant {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    workspace_id: Option<String>,
}

fn grant(body: &str) -> Option<Grant> {
    let value: Value = serde_json::from_str(body).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    if text("token_type")? != "Bearer" {
        return None;
    }
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s > 0.0 && *s <= MAX_TOKEN_SECONDS)?;
    let workspace_id = match value.get("org_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        Some(_) => return None,
    };
    Some(Grant {
        access_token: text("access_token").filter(|t| is_token(t))?,
        refresh_token: text("refresh_token").filter(|t| is_token(t))?,
        expires_in: expires_in as i64,
        workspace_id,
    })
}

fn get(
    http: &impl HttpClient,
    path: &str,
    token: &str,
    workspace: Option<&str>,
) -> Result<crate::HttpResponse, ProviderError> {
    let url = format!("{CONSOLE}{path}");
    let bearer = format!("Bearer {token}");
    let mut headers = vec![("Accept", "application/json"), ("Authorization", bearer.as_str())];
    if let Some(workspace) = workspace {
        headers.push(("x-org-id", workspace));
    }
    http.get(&url, &headers)
}

/// Confirms who approved: the token must name a workspace, and the Console's session must name a user in it.
fn verify(http: &impl HttpClient, body: &str, issued_at: DateTime<Utc>) -> Result<ConsoleCredential, SignInError> {
    let grant = grant(body).ok_or(SignInError::Unverified)?;
    let workspace = grant
        .workspace_id
        .filter(|w| valid_workspace(w))
        .ok_or(SignInError::Unverified)?;
    let session = get(http, "auth/session", &grant.access_token, None).map_err(|_| SignInError::Network)?;
    if session.status != 200 {
        return Err(SignInError::Unverified);
    }
    let value: Value = serde_json::from_str(&session.body).map_err(|_| SignInError::Unverified)?;
    let user = value
        .pointer("/user/id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or(SignInError::Unverified)?;
    match value.get("org_id") {
        None | Some(Value::Null) => {}
        Some(Value::String(id)) if *id == workspace => {}
        Some(_) => return Err(SignInError::Unverified),
    }
    Ok(ConsoleCredential {
        access_token: grant.access_token,
        refresh_token: grant.refresh_token,
        expires_at: issued_at
            .checked_add_signed(Duration::seconds(grant.expires_in))
            .ok_or(SignInError::Unverified)?,
        workspace_id: workspace,
        user_id: user.to_owned(),
    })
}

/// The outcome of renewing a sign-in with its refresh token.
#[derive(Debug)]
pub enum Renewal {
    Renewed(ConsoleCredential),
    /// OpenCode refused the refresh token: the user has to sign in again.
    Rejected,
    /// OpenCode couldn't be asked right now (offline, rate-limited, a server error).
    Unavailable,
}

/// Renews `credential`. The new tokens must still belong to the same workspace; the user can't change on renewal.
pub fn renew(http: &impl HttpClient, credential: &ConsoleCredential, now: DateTime<Utc>) -> Renewal {
    let response = match post(
        http,
        "auth/device/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &credential.refresh_token),
            ("client_id", CLIENT_ID),
        ],
    ) {
        Ok(response) => response,
        Err(_) => return Renewal::Unavailable,
    };
    match response.status {
        200 => match grant(&response.body) {
            Some(grant)
                if grant
                    .workspace_id
                    .as_deref()
                    .is_none_or(|w| w == credential.workspace_id) =>
            {
                match now.checked_add_signed(Duration::seconds(grant.expires_in)) {
                    Some(expires_at) => Renewal::Renewed(ConsoleCredential {
                        access_token: grant.access_token,
                        refresh_token: grant.refresh_token,
                        expires_at,
                        workspace_id: credential.workspace_id.clone(),
                        user_id: credential.user_id.clone(),
                    }),
                    None => Renewal::Rejected,
                }
            }
            _ => Renewal::Rejected,
        },
        400 | 401 | 403 => Renewal::Rejected,
        _ => Renewal::Unavailable,
    }
}

/// A Console answer in a shape CodexBar doesn't know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownShape;

/// Go in a workspace: its windows, or why there are none.
#[derive(Debug, PartialEq)]
pub enum GoStatus {
    /// The windows with a reset time, and a note for each one OpenCode gave without one.
    Subscribed(Vec<Metric>, Vec<String>),
    NotSubscribed,
    /// Go in this workspace is another member's subscription; their usage isn't shown.
    OtherMember,
}

/// A microcent amount: the Console sends whole numbers as strings.
/// A whole number of microcents, sent as a string or a JSON integer.
fn microcents(value: Option<&Value>) -> Option<i128> {
    let value = value?;
    if let Some(whole) = value.as_i64() {
        return Some(whole.into());
    }
    let text = value.as_str()?;
    let digits = text.strip_prefix('-').unwrap_or(text);
    if text.len() > 38 || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Parses `/api/go/status` for `user`. `Err` when any window is missing or malformed: partial windows aren't shown as
/// if they were all there.
pub fn parse_go_status(body: &str, user: &str, now: DateTime<Utc>) -> Result<GoStatus, UnknownShape> {
    let value: Value = serde_json::from_str(body).map_err(|_| UnknownShape)?;
    if value.is_null() {
        return Ok(GoStatus::NotSubscribed);
    }
    // The device token is bound to its workspace, and not every response names the subscriber. When one is named and
    // it isn't this user, Go is another member's and isn't shown.
    match value.get("subscriberUserId") {
        None | Some(Value::Null) => {}
        Some(Value::String(subscriber)) if subscriber == user => {}
        Some(Value::String(_)) => return Ok(GoStatus::OtherMember),
        Some(_) => return Err(UnknownShape),
    }
    let access = value.get("access").ok_or(UnknownShape)?;
    if access.is_null() {
        return Ok(GoStatus::NotSubscribed);
    }
    let meters = access.get("meters").and_then(Value::as_object).ok_or(UnknownShape)?;
    // The same labels as the dashboard-cookie path, so an account moving to the Console keeps its history keys.
    let windows = [
        ("fiveHour", "5-hour window", Duration::hours(5)),
        ("week", "Weekly", Duration::days(7)),
        ("month", "Monthly", Duration::days(30)),
    ];
    let mut metrics = Vec::new();
    let mut notes = Vec::new();
    for (key, label, length) in windows {
        let meter = match meters.get(key) {
            Some(meter) if meter.is_object() => meter,
            // A plan without a weekly or monthly limit leaves that meter out; it stays absent, not zero.
            None | Some(Value::Null) if key != "fiveHour" => continue,
            _ => return Err(UnknownShape),
        };
        let used = microcents(meter.get("usedMicroCents"))
            .filter(|u| *u >= 0)
            .ok_or(UnknownShape)?;
        let limit = microcents(meter.get("limitMicroCents"))
            .filter(|l| *l > 0)
            .ok_or(UnknownShape)?;
        let (raw_start, raw_reset) = if key == "month" {
            (access.get("startsAt"), access.get("endsAt"))
        } else {
            (meter.get("startsAt"), meter.get("resetsAt"))
        };
        let Some(resets_at) = instant(raw_reset) else {
            // No reset time is invented. A 5-hour window that hasn't started has none yet; any other window without
            // one keeps its usage as a note, and the other windows stay.
            let idle = key == "fiveHour"
                && meter.get("resetsAt").is_some_and(Value::is_null)
                && meter.get("startsAt").is_some_and(Value::is_null)
                && used == 0;
            if !idle {
                notes.push(format!(
                    "{label}: {:.0}% used; OpenCode gave no reset time.",
                    used as f64 / limit as f64 * 100.0
                ));
            }
            continue;
        };
        let used = used as f64 / limit as f64;
        let start = instant(raw_start).unwrap_or(resets_at - length);
        metrics.push(Metric::Window {
            label: label.to_owned(),
            used,
            resets_at,
            pace: elapsed_pace(used, start, resets_at, now),
        });
    }
    Ok(GoStatus::Subscribed(metrics, notes))
}

/// Parses `/api/billing/status`: the Zen balance, which can be negative.
pub fn parse_balance(body: &str) -> Option<Money> {
    let value: Value = serde_json::from_str(body).ok()?;
    let micro = microcents(value.get("balanceMicroCents"))?;
    // To the nearest cent, halves away from zero.
    let half = MICROCENTS_PER_CENT / 2 * micro.signum();
    i64::try_from((micro + half) / MICROCENTS_PER_CENT)
        .ok()
        .map(Money::from_cents)
}

/// The saved sign-in couldn't be read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotUnavailable;

/// Where an account's sign-in is kept (Windows Credential Manager in the app). A seam for tests.
pub trait CredentialSlot: Send + Sync {
    /// The saved secret: `Ok(None)` when signed out, `Err` when it couldn't be read.
    fn load(&self) -> Result<Option<String>, SlotUnavailable>;
    fn save(&self, secret: &str) -> Result<(), SlotUnavailable>;
}

/// Renewals are rare; one at a time across the app, so two refreshes never spend the same refresh token. Process-wide,
/// because the dashboard builds its providers anew for every refresh.
static RENEWALS: Mutex<()> = Mutex::new(());

/// Every change to a saved Console sign-in: a renewal's last check and write, and the app's sign-in, sign-out and
/// removal. Held only around those, never across a network call, so a sign-out can't slip between a renewal's check
/// and its write and be undone by it.
static SLOT_WRITES: Mutex<()> = Mutex::new(());

/// Runs `change` to a saved Console sign-in with no renewal writing at the same time.
pub fn exclusive<T>(change: impl FnOnce() -> T) -> T {
    let _writes = SLOT_WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    change()
}

/// Renewals Credential Manager refused to save, by account, with the sign-in each one replaced. A refresh token works
/// once and reusing one ends the session, so an unsaved renewal must not be lost: it is used and saved again on the
/// next refresh, as long as the saved sign-in is still the one it replaced.
static UNSAVED: Mutex<Vec<(String, ConsoleCredential, ConsoleCredential)>> = Mutex::new(Vec::new());

const UNSAVED_MESSAGE: &str = "The renewed OpenCode sign-in couldn't be saved to Windows Credential Manager. CodexBar \
                               keeps it for now and tries again at the next refresh.";

/// One OpenCode account signed in through the Console: its Go windows and Zen balance, in one dashboard account.
pub struct OpenCodeConsoleProvider<H: HttpClient> {
    http: H,
    slot: Box<dyn CredentialSlot>,
    /// The dashboard account it reports (the settings record's id) and its name.
    account: String,
    label: String,
}

impl<H: HttpClient> OpenCodeConsoleProvider<H> {
    pub fn new(http: H, slot: Box<dyn CredentialSlot>, account: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            http,
            slot,
            account: account.into(),
            label: label.into(),
        }
    }

    fn saved(&self) -> Result<ConsoleCredential, ProviderError> {
        match self.slot.load() {
            Ok(Some(secret)) => {
                ConsoleCredential::from_secret(&secret).ok_or(ProviderError::NotSignedIn { hint: SIGN_IN_HINT })
            }
            Ok(None) => Err(ProviderError::NotSignedIn { hint: SIGN_IN_HINT }),
            Err(SlotUnavailable) => Err(ProviderError::Unexpected {
                detail: concat!(
                    "the saved OpenCode sign-in couldn't be read from ",
                    codexbar_core::secret_store_name!()
                ),
            }),
        }
    }

    /// A renewal kept from an earlier refresh: saved now if possible. `None` once the saved sign-in changed (signed
    /// out, signed in again), which drops it. The flag says whether it is still unsaved.
    fn unsaved(&self, saved: &ConsoleCredential) -> Option<(ConsoleCredential, bool)> {
        let mut unsaved = UNSAVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let ix = unsaved.iter().position(|(account, _, _)| *account == self.account)?;
        let (_, replaced, renewed) = unsaved.remove(ix);
        if replaced != *saved {
            return None;
        }
        let _writes = SLOT_WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Checked again with writes held: a sign-out since `saved` was read drops the renewal.
        match self.slot.load() {
            Ok(current) if current.as_deref().and_then(ConsoleCredential::from_secret).as_ref() != Some(&replaced) => {
                return None;
            }
            _ => {}
        }
        if self.slot.save(&renewed.to_secret()).is_ok() {
            return Some((renewed, false));
        }
        unsaved.push((self.account.clone(), replaced, renewed.clone()));
        Some((renewed, true))
    }

    /// The sign-in to fetch with, renewed first when it is about to expire, and whether a renewal is still unsaved. A
    /// renewal is only saved over the sign-in it renewed: one removed or replaced meanwhile (signed out, signed in
    /// again) is never brought back.
    fn current(&self, now: DateTime<Utc>) -> Result<(ConsoleCredential, bool), ProviderError> {
        let _one_at_a_time = RENEWALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = self.saved()?;
        // An unsaved renewal holds the only refresh token that still works, so it is the one renewed from.
        let (latest, pending) = self.unsaved(&saved).unwrap_or_else(|| (saved.clone(), false));
        if !latest.expires_soon(now) {
            return Ok((latest, pending));
        }
        match renew(&self.http, &latest, now) {
            Renewal::Renewed(renewed) => {
                let mut unsaved = UNSAVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                unsaved.retain(|(account, _, _)| *account != self.account);
                // The check and the write happen with writes held, so nothing changes the sign-in between them.
                let _writes = SLOT_WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match self.slot.load() {
                    // Removed or replaced meanwhile: the renewal is dropped, never written over it.
                    Ok(current)
                        if current.as_deref().and_then(ConsoleCredential::from_secret) != Some(saved.clone()) =>
                    {
                        return Err(ProviderError::NotSignedIn { hint: SIGN_IN_HINT });
                    }
                    Ok(_) => {}
                    // Unreadable: the old refresh token is spent, so the renewal is kept until it can be checked.
                    Err(SlotUnavailable) => {
                        unsaved.push((self.account.clone(), saved, renewed.clone()));
                        return Ok((renewed, true));
                    }
                }
                if self.slot.save(&renewed.to_secret()).is_err() {
                    unsaved.push((self.account.clone(), saved, renewed.clone()));
                    return Ok((renewed, true));
                }
                Ok((renewed, false))
            }
            Renewal::Rejected => Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
            // Until it actually expires, the current token still works.
            Renewal::Unavailable if latest.expires_at > now => Ok((latest, pending)),
            Renewal::Unavailable => Err(ProviderError::Network),
        }
    }

    fn read(&self, path: &str, credential: &ConsoleCredential) -> Result<String, ProviderError> {
        let response = get(
            &self.http,
            path,
            &credential.access_token,
            Some(&credential.workspace_id),
        )?;
        match response.status {
            200 => Ok(response.body),
            401 => Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
            // The workspace or the user's permission, which signing in again doesn't change.
            403 => Err(ProviderError::Refused {
                reason: "OpenCode refused this workspace's usage: the account lacks permission there, or the \
                         workspace changed.",
            }),
            status => Err(ProviderError::Http { status }),
        }
    }

    fn usage(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let (credential, unsaved) = self.current(now)?;
        let go = self.read("api/go/status", &credential).and_then(|body| {
            parse_go_status(&body, &credential.user_id, now).map_err(|UnknownShape| ProviderError::Unexpected {
                detail: "OpenCode Go limits in an unknown shape",
            })
        });
        let zen = self.read("api/billing/status", &credential).and_then(|body| {
            parse_balance(&body).ok_or(ProviderError::Unexpected {
                detail: "OpenCode Zen balance in an unknown shape",
            })
        });
        let mut metrics = Vec::new();
        let mut messages = Vec::new();
        if unsaved {
            messages.push(UNSAVED_MESSAGE.to_owned());
        }
        let mut errors = Vec::new();
        match go {
            Ok(GoStatus::Subscribed(windows, notes)) => {
                metrics.extend(windows);
                messages.extend(notes);
            }
            Ok(GoStatus::NotSubscribed) => messages.push("OpenCode Go isn't subscribed in this workspace.".to_owned()),
            Ok(GoStatus::OtherMember) => {
                messages.push("OpenCode Go in this workspace is another member's subscription.".to_owned());
            }
            Err(err) => {
                messages.push(format!("Go limits couldn't be read: {err}"));
                errors.push(err);
            }
        }
        match zen {
            Ok(balance) => metrics.push(Metric::Balance {
                label: "Zen balance".into(),
                remaining: balance,
                burn_per_day: None,
            }),
            Err(err) => {
                messages.push(format!("Zen balance couldn't be read: {err}"));
                errors.push(err);
            }
        }
        if metrics.is_empty() {
            // A rejected sign-in says so first; it is what the user can act on.
            let expired = errors.iter().position(|e| matches!(e, ProviderError::Expired { .. }));
            return Err(match expired {
                Some(ix) => errors.swap_remove(ix),
                None => errors.into_iter().next().unwrap_or(ProviderError::Unexpected {
                    detail: "no OpenCode Go limits or Zen balance",
                }),
            });
        }
        let mut account = AccountSnapshot::new(AccountId::new(self.account.clone()), Provider::OpenCode, metrics, now);
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        Ok(messages.into_iter().fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> UsageProvider for OpenCodeConsoleProvider<H> {
    fn name(&self) -> &'static str {
        "OpenCode Go + Zen"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.usage(now).map(|account| vec![account])
    }

    /// Every failure, signed out included, is this account's own, so it shows on its row rather than as the provider's.
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

    fn signed_in_account(&self) -> Option<AccountId> {
        Some(AccountId::new(self.account.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-09-22T10:00:00Z".parse().unwrap()
    }

    type Script = VecDeque<(&'static str, &'static str, HttpResponse)>;
    /// Method, URL, body and headers of each request sent.
    type Sent = Vec<(String, String, String, Vec<(String, String)>)>;

    /// Answers each request from a script of (method, path suffix) → response, recording what was sent.
    #[derive(Clone, Default)]
    struct Console {
        script: Arc<Mutex<Script>>,
        sent: Arc<Mutex<Sent>>,
    }

    impl Console {
        fn on(self, method: &'static str, path: &'static str, status: u16, body: &str) -> Self {
            self.script
                .lock()
                .unwrap()
                .push_back((method, path, HttpResponse::new(status, body)));
            self
        }

        fn answer(
            &self,
            method: &str,
            url: &str,
            headers: &[(&str, &str)],
            body: &str,
        ) -> Result<HttpResponse, ProviderError> {
            self.sent.lock().unwrap().push((
                method.to_owned(),
                url.to_owned(),
                body.to_owned(),
                headers
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect(),
            ));
            let mut script = self.script.lock().unwrap();
            let (expected_method, path, response) = script.pop_front().expect("an unscripted request");
            assert_eq!(method, expected_method);
            assert!(url.ends_with(path), "{url} should end with {path}");
            assert!(url.starts_with(CONSOLE), "every request stays on the Console");
            Ok(response)
        }

        fn bodies(&self) -> Vec<Value> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|(_, _, body, _)| {
                    // Form fields, as the device flow sends them.
                    let fields: serde_json::Map<String, Value> = form_urlencoded::parse(body.as_bytes())
                        .map(|(name, value)| (name.into_owned(), Value::String(value.into_owned())))
                        .collect();
                    Value::Object(fields)
                })
                .collect()
        }

        fn header(&self, request: usize, name: &str) -> Option<String> {
            self.sent.lock().unwrap()[request]
                .3
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        }

        fn requests(&self) -> usize {
            self.sent.lock().unwrap().len()
        }
    }

    impl HttpClient for Console {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            self.answer("GET", url, headers, "")
        }

        fn post_json(&self, _: &str, _: &[(&str, &str)], _: &str) -> Result<HttpResponse, ProviderError> {
            panic!("the device flow posts forms, not JSON");
        }

        fn post_form(
            &self,
            url: &str,
            headers: &[(&str, &str)],
            fields: &[(&str, &str)],
        ) -> Result<HttpResponse, ProviderError> {
            self.answer("POST", url, headers, &crate::http::form_body(fields))
        }
    }

    const CHALLENGE: &str = r#"{"device_code":"dev-123","user_code":"HSPH-FPQV","verification_uri":"/console/device",
        "verification_uri_complete":"/console/device?user_code=HSPH-FPQV&client_id=codexbar-windows","expires_in":600,
        "interval":5}"#;
    const TOKEN: &str = r#"{"access_token":"at-1","refresh_token":"rt-1","token_type":"Bearer","expires_in":3600,
        "org_id":"wrk_01ABC"}"#;
    const SESSION: &str = r#"{"user":{"id":"usr_1","email":"someone@example.com"},"org_id":"wrk_01ABC"}"#;

    fn challenge() -> DeviceChallenge {
        begin(
            &Console::default().on("POST", "auth/device/code", 200, CHALLENGE),
            now(),
        )
        .unwrap()
    }

    fn no_sleep(_: StdDuration, _: &AtomicBool) {}

    fn credential(expires_at: DateTime<Utc>) -> ConsoleCredential {
        ConsoleCredential {
            access_token: "at-1".into(),
            refresh_token: "rt-1".into(),
            expires_at,
            workspace_id: "wrk_01ABC".into(),
            user_id: "usr_1".into(),
        }
    }

    #[test]
    fn begin_asks_with_codexbars_own_client_label_and_keeps_the_device_page() {
        let console = Console::default().on("POST", "auth/device/code", 200, CHALLENGE);
        let challenge = begin(&console, now()).unwrap();
        assert_eq!(
            console.bodies()[0],
            json!({ "client_id": "codexbar-windows", "supports_org_scope": "true" })
        );
        assert_eq!(
            challenge.url,
            "https://opencode.ai/console/device?user_code=HSPH-FPQV&client_id=codexbar-windows"
        );
        assert_eq!(challenge.user_code.as_deref(), Some("HSPH-FPQV"));
        assert_eq!(challenge.expires_at, now() + Duration::seconds(600));
        assert_eq!(challenge.interval, StdDuration::from_secs(5));
    }

    #[test]
    fn only_the_consoles_own_https_device_page_is_opened() {
        assert!(verification_url("/console/device?user_code=A").is_some());
        assert!(verification_url("https://opencode.ai/console/device").is_some());
        for address in [
            "http://opencode.ai/console/device",
            "https://opencode.ai.evil.example/console/device",
            "https://opencode.ai@evil.example/console/device",
            "https://evil.example/console/device",
            "https://opencode.ai/console/devices",
            "https://opencode.ai/console/login",
            "https://opencode.ai/console/device#x",
            "//evil.example/console/device",
            "javascript:alert(1)",
        ] {
            assert_eq!(verification_url(address), None, "{address}");
        }
    }

    #[test]
    fn a_challenge_outside_the_contract_is_refused() {
        for body in [
            r#"{"device_code":"d","verification_uri_complete":"https://evil.example/console/device","expires_in":600,"interval":5}"#,
            r#"{"device_code":"d","verification_uri_complete":"/console/device","expires_in":0,"interval":5}"#,
            r#"{"device_code":"d","verification_uri_complete":"/console/device","expires_in":600,"interval":0}"#,
            r#"{"device_code":"","verification_uri_complete":"/console/device","expires_in":600,"interval":5}"#,
            "not json",
        ] {
            let console = Console::default().on("POST", "auth/device/code", 200, body);
            assert_eq!(begin(&console, now()).unwrap_err(), SignInError::Unexpected, "{body}");
        }
        let console = Console::default().on("POST", "auth/device/code", 503, "{}");
        assert_eq!(begin(&console, now()).unwrap_err(), SignInError::Unexpected);
    }

    #[test]
    fn approval_waits_while_pending_slows_down_when_asked_and_verifies_the_user() {
        let console = Console::default()
            .on("POST", "auth/device/token", 400, r#"{"error":"authorization_pending"}"#)
            .on("POST", "auth/device/token", 400, r#"{"error":"slow_down"}"#)
            .on("POST", "auth/device/token", 200, TOKEN)
            .on("GET", "auth/session", 200, SESSION);
        let waits = Mutex::new(Vec::new());
        let sleep = |duration: StdDuration, _: &AtomicBool| waits.lock().unwrap().push(duration.as_secs());
        let credential = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &sleep).unwrap();
        assert_eq!(
            *waits.lock().unwrap(),
            [5, 5, 10],
            "slow_down adds five seconds from then on"
        );
        assert_eq!(credential.workspace_id, "wrk_01ABC");
        assert_eq!(credential.user_id, "usr_1");
        assert_eq!(credential.expires_at, now() + Duration::seconds(3600));
        let bodies = console.bodies();
        assert_eq!(bodies[0]["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
        assert_eq!(bodies[0]["device_code"], "dev-123");
        assert_eq!(bodies[0]["client_id"], "codexbar-windows");
        assert_eq!(console.header(3, "Authorization").as_deref(), Some("Bearer at-1"));
    }

    #[test]
    fn denial_expiry_and_unknown_errors_end_the_sign_in() {
        for (body, expected) in [
            (r#"{"error":"access_denied"}"#, SignInError::Denied),
            (r#"{"error":"expired_token"}"#, SignInError::Expired),
            (r#"{"error":"invalid_grant"}"#, SignInError::Unexpected),
        ] {
            let console = Console::default().on("POST", "auth/device/token", 400, body);
            let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &no_sleep);
            assert_eq!(result.unwrap_err(), expected, "{body}");
        }
    }

    #[test]
    fn no_poll_is_sent_after_the_code_expires() {
        let console = Console::default();
        let clock = Mutex::new(now());
        let later = || {
            let mut clock = clock.lock().unwrap();
            *clock += Duration::seconds(700);
            *clock
        };
        let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &later, &no_sleep);
        assert_eq!(result.unwrap_err(), SignInError::Expired);
        assert_eq!(console.requests(), 0);
    }

    #[test]
    fn cancelling_stops_before_the_next_poll() {
        let console = Console::default();
        let cancel = AtomicBool::new(false);
        let sleep = |_: StdDuration, cancel: &AtomicBool| cancel.store(true, Ordering::SeqCst);
        let result = wait_for_approval(&console, &challenge(), &cancel, &now, &sleep);
        assert_eq!(result.unwrap_err(), SignInError::Cancelled);
        assert_eq!(console.requests(), 0);
    }

    #[test]
    fn a_token_without_a_workspace_or_with_a_mismatched_session_is_not_kept() {
        let no_workspace = r#"{"access_token":"a","refresh_token":"r","token_type":"Bearer","expires_in":60}"#;
        let console = Console::default().on("POST", "auth/device/token", 200, no_workspace);
        let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &no_sleep);
        assert_eq!(result.unwrap_err(), SignInError::Unverified);

        let console = Console::default().on("POST", "auth/device/token", 200, TOKEN).on(
            "GET",
            "auth/session",
            200,
            r#"{"user":{"id":"usr_1"},"org_id":"wrk_02OTHER"}"#,
        );
        let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &no_sleep);
        assert_eq!(result.unwrap_err(), SignInError::Unverified);

        let console =
            Console::default()
                .on("POST", "auth/device/token", 200, TOKEN)
                .on("GET", "auth/session", 401, "{}");
        let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &no_sleep);
        assert_eq!(result.unwrap_err(), SignInError::Unverified);
    }

    #[test]
    fn a_saved_sign_in_round_trips_and_other_secrets_are_not_one() {
        let saved = credential(now());
        assert_eq!(ConsoleCredential::from_secret(&saved.to_secret()), Some(saved));
        assert_eq!(ConsoleCredential::from_secret("Fe26.2**cookie"), None);
        let bad_workspace = credential(now()).to_secret().replace("wrk_01ABC", "wrk_01 ABC");
        assert_eq!(ConsoleCredential::from_secret(&bad_workspace), None);
        assert!(
            !format!("{:?}", credential(now())).contains("at-1"),
            "Debug never shows a token"
        );
    }

    const GO: &str = r#"{"subscriberUserId":"usr_1","access":{"startsAt":"2026-09-01T00:00:00Z","endsAt":"2026-10-01T00:00:00Z","meters":{
        "fiveHour":{"usedMicroCents":"25","limitMicroCents":"100","startsAt":"2026-09-22T08:00:00Z","resetsAt":"2026-09-22T13:00:00Z"},
        "week":{"usedMicroCents":"50","limitMicroCents":"100","startsAt":"2026-09-20T00:00:00Z","resetsAt":"2026-09-27T00:00:00Z"},
        "month":{"usedMicroCents":"75","limitMicroCents":"100"}}}}"#;

    #[test]
    fn go_windows_keep_their_labels_and_the_servers_resets() {
        let GoStatus::Subscribed(metrics, _) = parse_go_status(GO, "usr_1", now()).unwrap() else {
            panic!("subscribed");
        };
        let labels: Vec<&str> = metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["5-hour window", "Weekly", "Monthly"]);
        assert_eq!(metrics[0].used_fraction(), Some(0.25));
        assert_eq!(metrics[2].used_fraction(), Some(0.75));
        assert_eq!(metrics[0].resets_at(), "2026-09-22T13:00:00Z".parse().ok());
        assert_eq!(metrics[2].resets_at(), "2026-10-01T00:00:00Z".parse().ok());
    }

    #[test]
    fn an_idle_five_hour_window_gets_no_invented_reset() {
        let idle = GO.replace(
            r#""usedMicroCents":"25","limitMicroCents":"100","startsAt":"2026-09-22T08:00:00Z","resetsAt":"2026-09-22T13:00:00Z""#,
            r#""usedMicroCents":"0","limitMicroCents":"100","startsAt":null,"resetsAt":null"#,
        );
        let GoStatus::Subscribed(metrics, _) = parse_go_status(&idle, "usr_1", now()).unwrap() else {
            panic!("subscribed");
        };
        let labels: Vec<&str> = metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["Weekly", "Monthly"]);
        // A used window without a reset keeps its usage as a note, and the other windows stay.
        let used = idle.replace(r#""usedMicroCents":"0""#, r#""usedMicroCents":"5""#);
        let GoStatus::Subscribed(metrics, notes) = parse_go_status(&used, "usr_1", now()).unwrap() else {
            panic!("subscribed");
        };
        assert_eq!(metrics.len(), 2);
        assert_eq!(notes, ["5-hour window: 5% used; OpenCode gave no reset time."]);
    }

    #[test]
    fn balances_round_to_the_nearest_cent_and_lifetimes_are_bounded() {
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":"2786781005"}"#),
            Some(Money::from_cents(2787))
        );
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":"-2786781005"}"#),
            Some(Money::from_cents(-2787))
        );
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":"2786481005"}"#),
            Some(Money::from_cents(2786))
        );
        let forever = TOKEN.replace(r#""expires_in":3600"#, r#""expires_in":1e300"#);
        let console = Console::default().on("POST", "auth/device/token", 200, &forever);
        let result = wait_for_approval(&console, &challenge(), &AtomicBool::new(false), &now, &no_sleep);
        assert_eq!(result.unwrap_err(), SignInError::Unverified);
        let console = Console::default().on("POST", "auth/device/token", 200, &forever);
        assert!(matches!(renew(&console, &credential(now()), now()), Renewal::Rejected));
    }

    #[test]
    fn go_for_no_one_another_member_or_a_broken_shape() {
        assert_eq!(parse_go_status("null", "usr_1", now()), Ok(GoStatus::NotSubscribed));
        assert_eq!(
            parse_go_status(r#"{"subscriberUserId":"usr_1","access":null}"#, "usr_1", now()),
            Ok(GoStatus::NotSubscribed)
        );
        assert_eq!(parse_go_status(GO, "usr_2", now()), Ok(GoStatus::OtherMember));
        assert_eq!(
            parse_go_status(r#"{"subscriberUserId":"usr_1","access":{"meters":{}}}"#, "usr_1", now()),
            Err(UnknownShape)
        );
        // Whole numbers may come as JSON integers; fractions and booleans may not.
        let numeric = GO.replace(r#""usedMicroCents":"50""#, r#""usedMicroCents":50"#);
        assert!(matches!(parse_go_status(&numeric, "usr_1", now()), Ok(GoStatus::Subscribed(m, _)) if m.len() == 3));
        let fraction = GO.replace(r#""usedMicroCents":"50""#, r#""usedMicroCents":50.5"#);
        assert_eq!(parse_go_status(&fraction, "usr_1", now()), Err(UnknownShape));
        // Not every response names the subscriber; one that does must name this user.
        let unnamed = GO.replace(r#""subscriberUserId":"usr_1","#, "");
        assert!(matches!(parse_go_status(&unnamed, "usr_1", now()), Ok(GoStatus::Subscribed(m, _)) if m.len() == 3));
        let odd = GO.replace(r#""subscriberUserId":"usr_1""#, r#""subscriberUserId":7"#);
        assert_eq!(parse_go_status(&odd, "usr_1", now()), Err(UnknownShape));
        let zero_limit = GO.replace(
            r#""usedMicroCents":"50","limitMicroCents":"100""#,
            r#""usedMicroCents":"50","limitMicroCents":"0""#,
        );
        assert_eq!(parse_go_status(&zero_limit, "usr_1", now()), Err(UnknownShape));
    }

    #[test]
    fn zen_balance_converts_microcents() {
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":"2434000000"}"#),
            Some(Money::from_cents(2434))
        );
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":"-100000000"}"#),
            Some(Money::from_cents(-100))
        );
        assert_eq!(parse_balance(r#"{"balanceMicroCents":"NaN"}"#), None);
        assert_eq!(parse_balance(r#"{"balanceMicroCents":true}"#), None);
        assert_eq!(
            parse_balance(r#"{"balanceMicroCents":2434000000}"#),
            Some(Money::from_cents(2434))
        );
        assert_eq!(parse_balance(r#"{"balanceMicroCents":24.5}"#), None);
    }

    #[derive(Clone, Default)]
    struct Slot {
        secret: Arc<Mutex<Option<String>>>,
        saves: Arc<AtomicUsize>,
        unreadable: bool,
    }

    impl Slot {
        fn holding(credential: &ConsoleCredential) -> Self {
            let slot = Self::default();
            *slot.secret.lock().unwrap() = Some(credential.to_secret());
            slot
        }

        fn saved(&self) -> Option<ConsoleCredential> {
            self.secret
                .lock()
                .unwrap()
                .as_deref()
                .and_then(ConsoleCredential::from_secret)
        }
    }

    impl CredentialSlot for Slot {
        fn load(&self) -> Result<Option<String>, SlotUnavailable> {
            if self.unreadable {
                return Err(SlotUnavailable);
            }
            Ok(self.secret.lock().unwrap().clone())
        }

        fn save(&self, secret: &str) -> Result<(), SlotUnavailable> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            *self.secret.lock().unwrap() = Some(secret.to_owned());
            Ok(())
        }
    }

    const BALANCE: &str = r#"{"balanceMicroCents":"2500000000"}"#;

    fn provider(console: &Console, slot: &Slot) -> OpenCodeConsoleProvider<Console> {
        OpenCodeConsoleProvider::new(console.clone(), Box::new(slot.clone()), "oc-1", "Team")
    }

    #[test]
    fn a_fetch_reads_go_and_zen_for_the_saved_workspace() {
        let console =
            Console::default()
                .on("GET", "api/go/status", 200, GO)
                .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::hours(1)));
        let accounts = provider(&console, &slot).fetch(now()).unwrap();
        let account = &accounts[0];
        assert_eq!(account.id().as_str(), "oc-1");
        assert_eq!(account.display_name(), "OpenCode Go + Zen · Team");
        assert_eq!(account.metrics().len(), 4);
        assert_eq!(console.header(0, "x-org-id").as_deref(), Some("wrk_01ABC"));
        assert_eq!(console.header(1, "Authorization").as_deref(), Some("Bearer at-1"));
    }

    #[test]
    fn go_and_zen_fail_independently() {
        let console =
            Console::default()
                .on("GET", "api/go/status", 500, "")
                .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::hours(1)));
        let account = provider(&console, &slot).fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 1);
        assert_eq!(
            account.messages(),
            ["Go limits couldn't be read: The usage service returned HTTP 500."]
        );

        let console =
            Console::default()
                .on("GET", "api/go/status", 200, "null")
                .on("GET", "api/billing/status", 200, "{}");
        let account = provider(&console, &slot).fetch(now()).unwrap_err();
        assert!(matches!(account, ProviderError::Unexpected { .. }));
    }

    #[test]
    fn signed_out_or_rejected_is_the_accounts_own_failure() {
        let outcomes = provider(&Console::default(), &Slot::default())
            .fetch_outcomes(now())
            .unwrap();
        assert!(matches!(
            &outcomes[0],
            AccountOutcome::Failed { account, error: ProviderError::NotSignedIn { .. }, .. } if account.as_str() == "oc-1"
        ));
        let console = Console::default()
            .on("GET", "api/go/status", 401, "")
            .on("GET", "api/billing/status", 401, "");
        let slot = Slot::holding(&credential(now() + Duration::hours(1)));
        assert!(matches!(
            provider(&console, &slot).fetch(now()),
            Err(ProviderError::Expired { .. })
        ));
        let unreadable = Slot {
            unreadable: true,
            ..Slot::default()
        };
        assert!(matches!(
            provider(&Console::default(), &unreadable).fetch(now()),
            Err(ProviderError::Unexpected { .. })
        ));
    }

    #[test]
    fn an_expiring_sign_in_is_renewed_and_saved_for_the_same_workspace() {
        let console = Console::default()
            .on(
                "POST",
                "auth/device/token",
                200,
                &TOKEN.replace("at-1", "at-2").replace("rt-1", "rt-2"),
            )
            .on("GET", "api/go/status", 200, GO)
            .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        provider(&console, &slot).fetch(now()).unwrap();
        let body = &console.bodies()[0];
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], "rt-1");
        let saved = slot.saved().unwrap();
        assert!(
            saved.access_token == "at-2" && saved.refresh_token == "rt-2",
            "the renewal is saved"
        );
        assert_eq!(
            (saved.workspace_id.as_str(), saved.user_id.as_str()),
            ("wrk_01ABC", "usr_1")
        );
        assert_eq!(console.header(1, "Authorization").as_deref(), Some("Bearer at-2"));
    }

    #[test]
    fn a_renewal_for_another_workspace_or_a_refused_one_needs_a_new_sign_in() {
        let other = TOKEN.replace("wrk_01ABC", "wrk_02OTHER");
        let console = Console::default().on("POST", "auth/device/token", 200, &other);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        assert!(matches!(
            provider(&console, &slot).fetch(now()),
            Err(ProviderError::Expired { .. })
        ));
        assert_eq!(slot.saves.load(Ordering::SeqCst), 0);

        let console = Console::default().on("POST", "auth/device/token", 400, r#"{"error":"invalid_grant"}"#);
        assert!(matches!(
            provider(&console, &slot).fetch(now()),
            Err(ProviderError::Expired { .. })
        ));
    }

    #[test]
    fn an_unavailable_renewal_uses_the_token_only_until_it_expires() {
        let console = Console::default()
            .on("POST", "auth/device/token", 503, "")
            .on("GET", "api/go/status", 200, GO)
            .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        assert!(provider(&console, &slot).fetch(now()).is_ok());

        let console = Console::default().on("POST", "auth/device/token", 503, "");
        let expired = Slot::holding(&credential(now() - Duration::seconds(1)));
        assert_eq!(
            provider(&console, &expired).fetch(now()).unwrap_err(),
            ProviderError::Network
        );
    }

    /// A slot whose sign-in is removed while the renewal is in flight (signed out in Settings).
    struct RemovedDuringRenewal(Slot, AtomicUsize);

    impl CredentialSlot for RemovedDuringRenewal {
        fn load(&self) -> Result<Option<String>, SlotUnavailable> {
            // Loaded before renewing, then once more before saving.
            if self.1.fetch_add(1, Ordering::SeqCst) >= 1 {
                return Ok(None);
            }
            self.0.load()
        }

        fn save(&self, secret: &str) -> Result<(), SlotUnavailable> {
            self.0.save(secret)
        }
    }

    #[test]
    fn a_renewal_never_brings_back_a_removed_sign_in() {
        let console = Console::default().on("POST", "auth/device/token", 200, TOKEN);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        let provider = OpenCodeConsoleProvider::new(
            console,
            Box::new(RemovedDuringRenewal(slot.clone(), AtomicUsize::new(0))),
            "oc-1",
            "Team",
        );
        assert!(matches!(provider.fetch(now()), Err(ProviderError::NotSignedIn { .. })));
        assert_eq!(slot.saves.load(Ordering::SeqCst), 0, "nothing is written back");
    }

    /// A slot whose writes fail, like a Credential Manager that refuses them.
    #[derive(Clone)]
    struct ReadOnly(Slot, Arc<std::sync::atomic::AtomicBool>);

    impl CredentialSlot for ReadOnly {
        fn load(&self) -> Result<Option<String>, SlotUnavailable> {
            self.0.load()
        }

        fn save(&self, secret: &str) -> Result<(), SlotUnavailable> {
            if self.1.load(Ordering::SeqCst) {
                return Err(SlotUnavailable);
            }
            self.0.save(secret)
        }
    }

    #[test]
    fn a_renewal_that_cant_be_saved_is_kept_said_and_saved_later() {
        let renewed = TOKEN.replace("at-1", "at-2").replace("rt-1", "rt-2");
        let console = Console::default()
            .on("POST", "auth/device/token", 200, &renewed)
            .on("GET", "api/go/status", 200, GO)
            .on("GET", "api/billing/status", 200, BALANCE)
            // The next refresh uses the kept renewal, with no second refresh-token request.
            .on("GET", "api/go/status", 200, GO)
            .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        let refuses = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let provider = |console: &Console| {
            OpenCodeConsoleProvider::new(
                console.clone(),
                Box::new(ReadOnly(slot.clone(), refuses.clone())),
                "oc-unsaved",
                "Team",
            )
        };
        let account = provider(&console).fetch(now()).unwrap().remove(0);
        assert_eq!(account.messages()[0], UNSAVED_MESSAGE);
        assert_eq!(slot.saved().unwrap().refresh_token, "rt-1", "nothing saved yet");

        refuses.store(false, Ordering::SeqCst);
        let account = provider(&console).fetch(now()).unwrap().remove(0);
        assert!(account.messages().is_empty());
        assert_eq!(slot.saved().unwrap().refresh_token, "rt-2", "saved on the next refresh");
        assert_eq!(console.header(4, "Authorization").as_deref(), Some("Bearer at-2"));
        assert_eq!(console.requests(), 5);
    }

    #[test]
    fn a_refused_workspace_isnt_an_expired_sign_in() {
        let console = Console::default()
            .on("GET", "api/go/status", 403, r#"{"_tag":"Forbidden"}"#)
            .on("GET", "api/billing/status", 403, r#"{"_tag":"Forbidden"}"#);
        let slot = Slot::holding(&credential(now() + Duration::hours(1)));
        assert!(matches!(
            provider(&console, &slot).fetch(now()),
            Err(ProviderError::Refused { .. })
        ));
    }

    #[test]
    fn an_omitted_weekly_or_monthly_meter_stays_absent() {
        let without_week = GO.replace(
            r#""week":{"usedMicroCents":"50","limitMicroCents":"100","startsAt":"2026-09-20T00:00:00Z","resetsAt":"2026-09-27T00:00:00Z"},"#,
            "",
        );
        assert_ne!(without_week, GO);
        let GoStatus::Subscribed(metrics, _) = parse_go_status(&without_week, "usr_1", now()).unwrap() else {
            panic!("subscribed");
        };
        let labels: Vec<&str> = metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["5-hour window", "Monthly"]);
        let null_month = GO.replace(
            r#""month":{"usedMicroCents":"75","limitMicroCents":"100"}"#,
            r#""month":null"#,
        );
        assert!(matches!(parse_go_status(&null_month, "usr_1", now()), Ok(GoStatus::Subscribed(m, _)) if m.len() == 2));
        // The 5-hour meter is required, and a weekly one that is there must be well-formed.
        let without_five = GO.replace(r#""fiveHour":"#, r#""other":"#);
        assert_eq!(parse_go_status(&without_five, "usr_1", now()), Err(UnknownShape));
        let broken_week = GO.replace(r#""week":{"#, r#""week":"x","unused":{"#);
        assert_eq!(parse_go_status(&broken_week, "usr_1", now()), Err(UnknownShape));
    }

    /// A slot that reads once, then can't be read: Credential Manager failing mid-renewal.
    struct UnreadableAfterFirst(Slot, AtomicUsize);

    impl CredentialSlot for UnreadableAfterFirst {
        fn load(&self) -> Result<Option<String>, SlotUnavailable> {
            if self.1.fetch_add(1, Ordering::SeqCst) >= 1 {
                return Err(SlotUnavailable);
            }
            self.0.load()
        }

        fn save(&self, secret: &str) -> Result<(), SlotUnavailable> {
            self.0.save(secret)
        }
    }

    #[test]
    fn a_renewal_is_kept_when_the_saved_sign_in_cant_be_read_back() {
        let renewed = TOKEN.replace("at-1", "at-9").replace("rt-1", "rt-9");
        let console = Console::default()
            .on("POST", "auth/device/token", 200, &renewed)
            .on("GET", "api/go/status", 200, GO)
            .on("GET", "api/billing/status", 200, BALANCE);
        let slot = Slot::holding(&credential(now() + Duration::seconds(30)));
        let provider = OpenCodeConsoleProvider::new(
            console.clone(),
            Box::new(UnreadableAfterFirst(slot.clone(), AtomicUsize::new(0))),
            "oc-unreadable",
            "Team",
        );
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.messages()[0], UNSAVED_MESSAGE);
        assert_eq!(console.header(1, "Authorization").as_deref(), Some("Bearer at-9"));
        let kept = UNSAVED
            .lock()
            .unwrap()
            .iter()
            .any(|(id, _, r)| id == "oc-unreadable" && r.refresh_token == "rt-9");
        assert!(kept, "the spent refresh token's replacement is kept");
    }
}
