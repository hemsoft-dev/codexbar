//! Claude subscription limits (5-hour and weekly), read with the sign-in Claude Code keeps in a config folder's
//! `.credentials.json`: Claude Code's own (`~/.claude`), or one CodexBar signed in through Claude Code (#80). On macOS
//! Claude Code keeps the sign-in in the login Keychain instead, one item per folder.
//!
//! This provider never refreshes or rewrites a token itself, because a refresh racing Claude Code can sign it out.
//! When a token expires (or is refused) it asks Claude Code to renew it (`claude_cli::renew`), which takes Claude
//! Code's own refresh lock, then reads the file again. The usage endpoint punishes polling with hour-long 429s, so
//! results are cached and every 429/403 starts a backoff, per sign-in file and across refreshes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Currency, Metric, Money, Provider};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::claude_cli::ClaudeCliError;
use crate::pace::elapsed_pace;
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const SIGN_IN_HINT: &str = "Run `claude` and sign in with your Claude account.";
const EXPIRED_HINT: &str = "Open Claude Code once to renew the sign-in.";
/// The hint for a sign-in CodexBar made (#80).
pub const MANAGED_SIGN_IN_HINT: &str = "Sign in again in Settings > Accounts.";
/// The account of a sign-in whose Claude account can't be read.
const LEGACY_ID: &str = "claude";
/// A sign-in is renewed when its token expires within this long.
const RENEW_AHEAD_MINUTES: i64 = 5;
/// At most one renewal is tried per sign-in file in this long.
const RENEW_BACKOFF: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Fresh results are reused this long; the endpoint does not need 2-minute polling.
const CACHE_TTL_MINUTES: i64 = 10;
/// Minimum wait after a 429, even when Retry-After is shorter.
const RATE_LIMIT_BACKOFF_MINUTES: i64 = 15;
/// A 403 is usually an org policy; asking again soon only re-arms the limiter.
const FORBIDDEN_BACKOFF_HOURS: i64 = 6;

/// The parts of the Claude Code sign-in this provider uses.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    access_token: String,
    expires_at: Option<DateTime<Utc>>,
    subscription: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("subscription", &self.subscription)
            .finish()
    }
}

/// Claude Code's own config folder: `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
pub fn default_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::platform::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

/// Where Claude Code keeps the signed-in account (`oauthAccount`): `.claude.json` in the config folder when one is
/// set, else in the user profile beside `~/.claude`.
pub fn default_profile_path() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir).join(".claude.json"),
        None => crate::platform::home_dir().unwrap_or_default().join(".claude.json"),
    }
}

/// The Claude account a config folder is signed in to: its dashboard account, `claude-` and a short hash of the
/// account and organization, so each identity keeps its own history and a folder signed in to another never inherits
/// them; and its email.
pub fn signed_in_identity(profile: &Path) -> Option<(AccountId, Option<String>)> {
    let text = std::fs::read_to_string(profile).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    let account = json.get("oauthAccount")?;
    let field = |name: &str| {
        account
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let user = field("accountUuid")?;
    let organization = field("organizationUuid").unwrap_or_default();
    let digest = Sha256::digest(format!("{user}\n{organization}").as_bytes());
    let short: String = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect();
    Some((AccountId::new(format!("claude-{short}")), field("emailAddress")))
}

/// `$CLAUDE_CONFIG_DIR/.credentials.json`, or `~/.claude/.credentials.json`.
pub fn default_credentials_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::platform::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
        .join(".credentials.json")
}

/// Whether `config` is Claude Code's own folder used the way Claude Code uses it when no folder is set: `~/.claude`
/// with `CLAUDE_CONFIG_DIR` unset. Claude Code goes by whether a folder is set, not by where it is, so a set
/// `CLAUDE_CONFIG_DIR=~/.claude` is any other folder to it.
pub fn is_default_config_dir(config: &Path) -> bool {
    std::env::var_os("CLAUDE_CONFIG_DIR").is_none_or(|value| value.is_empty())
        && crate::platform::home_dir().is_some_and(|home| config == home.join(".claude"))
}

/// What Claude Code keeps `config`'s sign-in under, as `CLAUDE_SECURESTORAGE_CONFIG_DIR`: empty for its own folder
/// ([`is_default_config_dir`]), the folder itself for any other. Claude Code prefers this selector to the config
/// folder, so the folder Claude Code uses on its own ([`default_config_dir`]) keeps one set for CodexBar, and a
/// folder CodexBar signed in never does.
pub fn secure_storage_selector(config: &Path) -> OsString {
    if config == default_config_dir()
        && let Some(selector) = std::env::var_os("CLAUDE_SECURESTORAGE_CONFIG_DIR")
    {
        return selector;
    }
    if is_default_config_dir(config) {
        OsString::new()
    } else {
        config.as_os_str().to_owned()
    }
}

/// The login Keychain item Claude Code keeps a config folder's sign-in in on macOS: `Claude Code-credentials` for an
/// empty [`secure_storage_selector`], and for any other a suffix of the first 8 hex digits of the SHA-256 of the
/// selector in Unicode NFC, as Claude Code normalizes it.
pub fn keychain_service(config: &Path) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    const SERVICE: &str = "Claude Code-credentials";
    let selector = secure_storage_selector(config);
    if selector.is_empty() {
        return SERVICE.to_owned();
    }
    let path: String = selector.to_string_lossy().nfc().collect();
    let digest = Sha256::digest(path.as_bytes());
    let short: String = digest.iter().take(4).map(|byte| format!("{byte:02x}")).collect();
    format!("{SERVICE}-{short}")
}

/// What a folder's macOS Keychain item says about its sign-in. Elsewhere there is never an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum KeychainItem {
    /// No item, or none could be read: the sign-in file decides, as in Claude Code.
    Absent,
    SignedIn,
    /// An item without a Claude sign-in (only MCP servers' tokens, `mcpOAuth`): signed out, whatever the file says,
    /// as Claude Code goes by the item once there is one.
    NoSignIn,
}

impl KeychainItem {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn of(text: &str) -> Self {
        if parse_credentials(text).is_some() {
            Self::SignedIn
        } else {
            Self::NoSignIn
        }
    }

    fn or_file(self, path: &Path) -> bool {
        match self {
            Self::Absent => credentials_file(path).is_file(),
            Self::SignedIn => true,
            Self::NoSignIn => false,
        }
    }
}

/// Whether the config folder holding the sign-in file `path` is signed in: on macOS its Keychain item when there is
/// one, else the file. May wait for the Keychain, so never on the UI thread; there, use [`seems_signed_in`].
pub fn has_credentials(path: &Path) -> bool {
    path.parent()
        .map_or(KeychainItem::Absent, keychain::holds)
        .or_file(path)
}

/// [`has_credentials`] for the UI thread, which never waits: the Keychain answer is the last one known, and an old
/// or missing one is looked up on another thread. An answer that changes there counts in [`keychain_changes`].
pub fn seems_signed_in(path: &Path) -> bool {
    path.parent()
        .map_or(KeychainItem::Absent, keychain::holds_known)
        .or_file(path)
}

/// Counts Keychain answers that changed in a background lookup. A change means a list built from
/// [`seems_signed_in`] may be out of date, so the dashboard refreshes when this moves.
pub fn keychain_changes() -> u64 {
    keychain::CHANGES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Where Claude Code keeps the sign-in file of the folder holding `path`: in the folder its
/// [`secure_storage_selector`] names (`~/.claude` for an empty one), which is the folder itself unless an inherited
/// selector moved it.
fn credentials_file(path: &Path) -> PathBuf {
    let Some(config) = path.parent() else {
        return path.to_owned();
    };
    let selector = secure_storage_selector(config);
    if selector == config.as_os_str() {
        return path.to_owned();
    }
    let folder = if selector.is_empty() {
        crate::platform::home_dir().map_or_else(|| config.to_owned(), |home| home.join(".claude"))
    } else {
        PathBuf::from(selector)
    };
    folder.join(".credentials.json")
}

/// Records that the folder holding `path` was just signed out, so a Keychain answer from before isn't reused.
pub fn signed_out(path: &Path) {
    if let Some(config) = path.parent() {
        keychain::forget(config);
    }
}

/// Claude Code's sign-ins in the macOS login Keychain. Claude Code reads and writes them with `security`, so reading
/// them with it too never asks the user, where the Security framework would ask for CodexBar.
#[cfg(target_os = "macos")]
mod keychain {
    use std::io::Read as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::KeychainItem;

    /// How long a lookup of whether a folder holds an item is reused.
    const PRESENCE_TTL: Duration = Duration::from_secs(5);
    /// A lookup still running after this long is waiting on a Keychain dialog: it is stopped and counts as no item,
    /// so one stuck lookup never holds up a refresh.
    const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

    pub static CHANGES: AtomicU64 = AtomicU64::new(0);

    /// What is known about one item, and when that was looked up.
    struct Presence {
        service: String,
        known: Option<(Instant, KeychainItem)>,
        looking: bool,
    }

    static PRESENCE: Mutex<Vec<Presence>> = Mutex::new(Vec::new());

    /// Runs `f` on the item's entry, made when there is none. The lock is never held while `security` runs.
    fn with_presence<R>(service: &str, f: impl FnOnce(&mut Presence) -> R) -> R {
        let mut all = PRESENCE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = all.iter_mut().find(|entry| entry.service == service) {
            return f(entry);
        }
        all.push(Presence {
            service: service.to_owned(),
            known: None,
            looking: false,
        });
        f(all.last_mut().expect("just pushed"))
    }

    fn fresh(entry: &Presence) -> Option<KeychainItem> {
        entry
            .known
            .filter(|(at, _)| at.elapsed() < PRESENCE_TTL)
            .map(|(_, held)| held)
    }

    /// The item's account, as Claude Code picks it: `$USER`, else the login name, and a fixed name for one that
    /// isn't a plain name.
    fn account() -> String {
        let name = std::env::var("USER")
            .ok()
            .filter(|user| !user.is_empty())
            .or_else(login_name)
            .unwrap_or_default();
        let plain = !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
        if plain { name } else { "claude-code-user".to_owned() }
    }

    /// The effective user's login name from the user database.
    fn login_name() -> Option<String> {
        let mut buffer = vec![0 as libc::c_char; 4096];
        // SAFETY: `passwd` is plain data that `getpwuid_r` fills in; all-zero is a valid value to start from.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, and `buffer.len()` is the buffer's real size.
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if status != 0 || found.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: on success `pw_name` points to a NUL-terminated string inside `buffer`, which is still alive.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) };
        name.to_str().ok().map(str::to_owned)
    }

    /// Runs `security` with `args` within [`LOOKUP_TIMEOUT`]: its exit code and stdout, or `None` when it couldn't
    /// start or was stopped.
    fn security(args: &[&str]) -> Option<(Option<i32>, Vec<u8>)> {
        let mut child = Command::new("/usr/bin/security")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        // Read while it runs: an item larger than the pipe's buffer would otherwise keep `security` from exiting.
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + LOOKUP_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        // The pipe closes once `security` has exited or been killed, so the reader always finishes.
        let bytes = reader.join().ok()?.ok()?;
        Some((status?.code(), bytes))
    }

    /// Looks the item up with `extra` arguments: its stdout when it was found.
    fn lookup(service: &str, extra: &[&str]) -> Option<Vec<u8>> {
        let account = account();
        let mut args = vec!["find-generic-password", "-a", &account, "-s", service];
        args.extend_from_slice(extra);
        match security(&args)? {
            (Some(0), stdout) => Some(stdout),
            _ => None,
        }
    }

    fn remember(service: &str, item: KeychainItem) {
        with_presence(service, |entry| entry.known = Some((Instant::now(), item)));
    }

    /// Records the item as gone, after Claude Code signed the folder out.
    pub fn forget(config: &Path) {
        remember(&super::keychain_service(config), KeychainItem::Absent);
    }

    /// The item's contents, when it is there.
    fn contents(service: &str) -> Option<String> {
        let stdout = lookup(service, &["-w"])?;
        let text = String::from_utf8(stdout).ok()?;
        let text = text.trim_end();
        (!text.is_empty()).then(|| text.to_owned())
    }

    fn look(service: &str) -> KeychainItem {
        contents(service).map_or(KeychainItem::Absent, |text| KeychainItem::of(&text))
    }

    /// The item's contents. Reading them also answers what the item holds, which replaces an older answer, such as
    /// one from just before a sign-in.
    pub fn read(config: &Path) -> Option<String> {
        let service = super::keychain_service(config);
        let text = contents(&service);
        remember(&service, text.as_deref().map_or(KeychainItem::Absent, KeychainItem::of));
        text
    }

    /// Deletes the item. True when it is gone: deleted, or never there (`security` exits with 44 then).
    pub fn delete(config: &Path) -> bool {
        let service = super::keychain_service(config);
        let account = account();
        let gone = matches!(
            security(&["delete-generic-password", "-a", &account, "-s", &service]),
            Some((Some(0 | 44), _))
        );
        if gone {
            remember(&service, KeychainItem::Absent);
        }
        gone
    }

    /// Looks the item up, reusing an answer from the last [`PRESENCE_TTL`].
    pub fn holds(config: &Path) -> KeychainItem {
        let service = super::keychain_service(config);
        if let Some(item) = with_presence(&service, |entry| fresh(entry)) {
            return item;
        }
        let item = look(&service);
        remember(&service, item);
        item
    }

    /// The last answer for the item, never waiting: a missing or old one is looked up on another thread, which counts
    /// a changed answer in [`CHANGES`] so lists built from the old one are built again.
    pub fn holds_known(config: &Path) -> KeychainItem {
        let service = super::keychain_service(config);
        let (known, start) = with_presence(&service, |entry| {
            let start = fresh(entry).is_none() && !entry.looking;
            entry.looking |= start;
            (entry.known.map(|(_, item)| item), start)
        });
        if start {
            std::thread::spawn(move || {
                let item = look(&service);
                let before = with_presence(&service, |entry| {
                    entry.looking = false;
                    entry.known.replace((Instant::now(), item)).map(|(_, before)| before)
                });
                if before.unwrap_or(KeychainItem::Absent) != item {
                    CHANGES.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        known.unwrap_or(KeychainItem::Absent)
    }
}

/// Claude Code keeps its sign-ins in files everywhere but macOS.
#[cfg(not(target_os = "macos"))]
mod keychain {
    use std::path::Path;
    use std::sync::atomic::AtomicU64;

    use super::KeychainItem;

    pub static CHANGES: AtomicU64 = AtomicU64::new(0);

    pub fn read(_: &Path) -> Option<String> {
        None
    }

    pub fn delete(_: &Path) -> bool {
        true
    }

    pub fn forget(_: &Path) {}

    pub fn holds(_: &Path) -> KeychainItem {
        KeychainItem::Absent
    }

    pub fn holds_known(_: &Path) -> KeychainItem {
        KeychainItem::Absent
    }
}

/// Deletes the macOS Keychain item of the folder holding the sign-in file `path`, for a folder about to be removed:
/// the item is named after the folder, so nothing could find it afterwards. True when it is gone, and always
/// elsewhere.
pub fn delete_keychain_item(path: &Path) -> bool {
    path.parent().is_none_or(keychain::delete)
}

/// The sign-in for the folder holding `path`. On macOS Claude Code keeps it in the login Keychain and may leave an
/// older `.credentials.json` behind, so as in Claude Code an item decides when there is one, even one without a
/// Claude sign-in, and the file only when there is none.
pub fn read_credentials(path: &Path) -> Result<Credentials, ProviderError> {
    let text = match path.parent().and_then(keychain::read) {
        Some(text) => Some(text),
        None => std::fs::read_to_string(credentials_file(path)).ok(),
    };
    text.as_deref()
        .and_then(parse_credentials)
        .ok_or(ProviderError::NotSignedIn { hint: SIGN_IN_HINT })
}

/// The Claude sign-in in Claude Code's credentials JSON (`claudeAiOauth`), when it has an access token.
fn parse_credentials(text: &str) -> Option<Credentials> {
    let json: Value = serde_json::from_str(text).ok()?;
    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())?
        .to_owned();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis);
    let subscription = oauth.get("subscriptionType").and_then(Value::as_str).map(str::to_owned);
    Some(Credentials {
        access_token,
        expires_at,
        subscription,
    })
}

/// Maps `/api/oauth/usage` (#82), an undocumented endpoint, so every field is read defensively (see codexbar-ios
/// `CLAUDE-USAGE.md`):
///
/// - Windows come from the structured `limits[]` first: `session` (5-hour), `weekly_all` (Weekly) and every
///   `weekly_scoped` entry under the model name the server gives (such as Fable). The flat `five_hour`, `seven_day`
///   (or `seven_day_oauth_apps`), `seven_day_opus` and `seven_day_sonnet` fields only fill in limits `limits[]` doesn't
///   report. Other flat fields are internal codenames and are never shown.
/// - Money comes from `spend` first, then `extra_usage`, in minor units of the reported currency. Without a currency
///   nothing is assumed: the amounts are left out and a message says so. A prepaid `spend.balance` is its own metric,
///   never derived from the limit.
///
/// An account with only money (say, Enterprise credits) is valid; only a response with neither is an error.
pub fn parse_usage(
    payload: &str,
    subscription: Option<&str>,
    now: DateTime<Utc>,
) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let mut metrics = windows(&json, now);
    let mut messages = Vec::new();
    let (found, notes) = money(&json);
    metrics.extend(found);
    messages.extend(notes);
    if metrics.is_empty() {
        return Err(ProviderError::Unexpected {
            detail: "no usage windows or credits for this account",
        });
    }
    let mut account = AccountSnapshot::new(AccountId::new("claude"), Provider::Claude, metrics, now);
    for message in messages {
        account = account.with_message(message);
    }
    Ok(match subscription.filter(|s| !s.is_empty()) {
        Some(plan) => account.with_label(crate::codex::plan_name(plan)),
        None => account,
    })
}

/// One limit window as reported: a label, percent used (0..=100) and the reset.
struct Window {
    label: String,
    percent: f64,
    resets_at: DateTime<Utc>,
    length: Duration,
}

impl Window {
    fn metric(self, now: DateTime<Utc>) -> Metric {
        let used = (self.percent / 100.0).clamp(0.0, 1.0);
        Metric::Window {
            label: self.label,
            used,
            resets_at: self.resets_at,
            pace: elapsed_pace(used, self.resets_at - self.length, self.resets_at, now),
        }
    }
}

const SESSION: &str = "5-hour window";
const WEEKLY: &str = "Weekly";

/// The windows in display order: 5-hour, Weekly, then model-scoped weekly limits (structured ones in the server's
/// order, then flat Opus and Sonnet when `limits[]` doesn't have them).
fn windows(json: &Value, now: DateTime<Utc>) -> Vec<Metric> {
    let reset = |value: Option<&Value>| {
        DateTime::parse_from_rfc3339(value?.as_str()?)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    };
    let mut found: Vec<Window> = Vec::new();
    for limit in json.get("limits").and_then(Value::as_array).into_iter().flatten() {
        let label = match limit.get("kind").and_then(Value::as_str) {
            Some("session") => SESSION.to_owned(),
            Some("weekly_all") => WEEKLY.to_owned(),
            Some("weekly_scoped") => {
                let Some(model) = limit
                    .pointer("/scope/model/display_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                else {
                    continue;
                };
                format!("{WEEKLY} {model}")
            }
            // Unknown kinds are skipped, never guessed at.
            _ => continue,
        };
        let (Some(percent), Some(resets_at)) = (
            limit.get("percent").and_then(Value::as_f64),
            reset(limit.get("resets_at")),
        ) else {
            continue;
        };
        if found.iter().any(|window| window.label.eq_ignore_ascii_case(&label)) {
            continue;
        }
        let length = if label == SESSION {
            Duration::hours(5)
        } else {
            Duration::days(7)
        };
        found.push(Window {
            label,
            percent,
            resets_at,
            length,
        });
    }
    // Flat fields fill in what the structured list didn't report.
    let flat: [(&[&str], &str, Duration); 4] = [
        (&["five_hour"], SESSION, Duration::hours(5)),
        (&["seven_day", "seven_day_oauth_apps"], WEEKLY, Duration::days(7)),
        (&["seven_day_opus"], "Weekly Opus", Duration::days(7)),
        (&["seven_day_sonnet"], "Weekly Sonnet", Duration::days(7)),
    ];
    for (keys, label, length) in flat {
        if found.iter().any(|window| window.label.eq_ignore_ascii_case(label)) {
            continue;
        }
        let window = keys.iter().find_map(|key| {
            let value = json.get(*key)?;
            Some((value.get("utilization")?.as_f64()?, reset(value.get("resets_at"))?))
        });
        if let Some((percent, resets_at)) = window {
            found.push(Window {
                label: label.to_owned(),
                percent,
                resets_at,
                length,
            });
        }
    }
    // 5-hour first, then Weekly, then the model-scoped windows in the order found.
    found.sort_by_key(|window| match window.label.as_str() {
        SESSION => 0,
        WEEKLY => 1,
        _ => 2,
    });
    found.into_iter().map(|window| window.metric(now)).collect()
}

/// Money in a provider-owned representation: `{amount_minor, currency, exponent}`. `None` when malformed; an error
/// message when the currency is missing or one CodexBar can't show.
fn amount(value: &Value) -> Option<Result<Money, String>> {
    let minor = value.get("amount_minor")?.as_f64()?;
    let exponent = value
        .get("exponent")
        .and_then(Value::as_u64)
        .map(|places| places as u32);
    Some(to_money(minor, value.get("currency").and_then(Value::as_str), exponent))
}

/// `minor` in units of 10^-`places` of `code`, rescaled to that currency's own minor unit.
fn to_money(minor: f64, code: Option<&str>, places: Option<u32>) -> Result<Money, String> {
    let Some(code) = code.map(str::trim).filter(|code| !code.is_empty()) else {
        return Err("Claude didn't say which currency usage credits are in, so they aren't shown.".to_owned());
    };
    let Some(currency) = Currency::from_code(code) else {
        return Err(format!(
            "Usage credits are billed in {code}, which CodexBar can't show yet."
        ));
    };
    let places = places.unwrap_or(currency.minor_digits());
    let scale = 10f64.powi(currency.minor_digits() as i32 - places as i32);
    Ok(Money::new((minor * scale).round() as i64, currency))
}

/// Usage credits: month-to-date spend against the monthly limit, and the prepaid balance when reported. From `spend`
/// when present, else from `extra_usage`. Nothing while credits are off or not reported. Each amount stands alone: one
/// that can't be read (say, a limit in an unknown currency) is left out with a message, and the others still show.
fn money(json: &Value) -> (Vec<Metric>, Vec<String>) {
    let mut metrics = Vec::new();
    let mut messages: Vec<String> = Vec::new();
    let mut keep = |result: Result<Money, String>| match result {
        Ok(money) => Some(money),
        Err(message) => {
            if !messages.contains(&message) {
                messages.push(message);
            }
            None
        }
    };
    if let Some(spend) = json.get("spend").filter(|spend| spend.is_object()) {
        if !spend.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
            return (metrics, messages);
        }
        if let Some(used) = spend.get("used").and_then(amount).and_then(&mut keep) {
            let limit = spend
                .get("limit")
                .and_then(amount)
                .and_then(&mut keep)
                .filter(|limit| limit.cents() > 0);
            metrics.push(credits(used, limit));
        }
        if let Some(balance) = spend.get("balance").and_then(amount).and_then(&mut keep) {
            metrics.push(Metric::Balance {
                label: "Credit balance".to_owned(),
                remaining: balance,
                burn_per_day: None,
            });
        }
        return (metrics, messages);
    }
    let Some(extra) = json.get("extra_usage").filter(|extra| extra.is_object()) else {
        return (metrics, messages);
    };
    if !extra.get("is_enabled").and_then(Value::as_bool).unwrap_or(false) {
        return (metrics, messages);
    }
    let code = extra.get("currency").and_then(Value::as_str);
    let places = extra
        .get("decimal_places")
        .and_then(Value::as_u64)
        .map(|places| places as u32);
    let reading = |field: &str| {
        extra
            .get(field)
            .and_then(Value::as_f64)
            .map(|value| to_money(value, code, places))
    };
    if let Some(used) = reading("used_credits").and_then(&mut keep) {
        let limit = reading("monthly_limit")
            .and_then(&mut keep)
            .filter(|limit| limit.cents() > 0);
        metrics.push(credits(used, limit));
    }
    (metrics, messages)
}

fn credits(used: Money, limit: Option<Money>) -> Metric {
    Metric::Spend {
        label: "Extra usage".to_owned(),
        spent: Money::new(used.cents().max(0), used.currency()),
        limit,
        // Credits reset monthly, but no verified response says when.
        resets_at: None,
        pace: None,
    }
}

#[derive(Default)]
struct State {
    cached: Option<AccountSnapshot>,
    /// Until when, and for which account: a folder signed in to another account since isn't held back.
    backoff_until: Option<(DateTime<Utc>, AccountId)>,
    renewed_at: Option<Instant>,
    /// The account this sign-in file last fetched for, so a switch between refreshes is noticed.
    last_account: Option<AccountId>,
}

/// Cache, backoff and renewal per sign-in file. Process-wide, because the dashboard builds its providers anew for
/// every refresh: state kept in a provider would be gone by the next one, and with it the 429 backoff.
static STATES: Mutex<Vec<(PathBuf, State)>> = Mutex::new(Vec::new());

fn with_state<R>(path: &Path, f: impl FnOnce(&mut State) -> R) -> R {
    let mut states = STATES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, state)) = states.iter_mut().find(|(known, _)| known == path) {
        return f(state);
    }
    states.push((path.to_owned(), State::default()));
    let (_, state) = states.last_mut().expect("just pushed");
    f(state)
}

/// Asks Claude Code to renew a config folder's sign-in. A seam, so tests never start Claude Code.
pub trait Renewer: Send + Sync {
    fn renew(&self, config: &Path) -> Result<(), ClaudeCliError>;
}

/// Renews through `claude auth status`, which runs Claude Code's own locked refresh.
pub struct ClaudeCliRenewer;

impl Renewer for ClaudeCliRenewer {
    fn renew(&self, config: &Path) -> Result<(), ClaudeCliError> {
        crate::claude_cli::renew(config)
    }
}

pub struct ClaudeProvider<H: HttpClient> {
    http: H,
    credentials_path: PathBuf,
    /// Where the signed-in account is named (`.claude.json`); without it every sign-in is the legacy account.
    profile: Option<PathBuf>,
    account: Option<(String, String)>,
    hint: &'static str,
    renewer: Option<Arc<dyn Renewer>>,
}

impl<H: HttpClient> ClaudeProvider<H> {
    pub fn new(http: H, credentials_path: PathBuf) -> Self {
        Self {
            http,
            credentials_path,
            profile: None,
            account: None,
            hint: EXPIRED_HINT,
            renewer: None,
        }
    }

    /// Names the account from Claude Code's `.claude.json` at `profile`.
    pub fn with_profile(mut self, profile: PathBuf) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Reports one configured account: `id` is the dashboard account it is expected under, `label` its name.
    pub fn with_account(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.account = Some((id.into(), label.into()));
        self
    }

    /// A folder CodexBar signed in: signing in again happens in Settings.
    pub fn managed(mut self) -> Self {
        self.hint = MANAGED_SIGN_IN_HINT;
        self
    }

    /// Asks Claude Code, through `renewer`, to renew a sign-in that expires or is refused.
    pub fn with_renewer(mut self, renewer: Arc<dyn Renewer>) -> Self {
        self.renewer = Some(renewer);
        self
    }

    fn identity(&self) -> AccountId {
        self.profile
            .as_deref()
            .and_then(signed_in_identity)
            .map_or_else(|| AccountId::new(LEGACY_ID), |(id, _)| id)
    }

    fn read(&self) -> Result<Credentials, ProviderError> {
        let not_signed_in = |_| ProviderError::NotSignedIn {
            hint: self.not_signed_in_hint(),
        };
        match read_credentials(&self.credentials_path) {
            Ok(credentials) => Ok(credentials),
            // Claude Code may be replacing the file right now; one more read after a moment sees the finished file.
            Err(_) if credentials_file(&self.credentials_path).is_file() => {
                std::thread::sleep(std::time::Duration::from_millis(150));
                read_credentials(&self.credentials_path).map_err(not_signed_in)
            }
            Err(err) => Err(not_signed_in(err)),
        }
    }

    fn not_signed_in_hint(&self) -> &'static str {
        if self.hint == MANAGED_SIGN_IN_HINT {
            MANAGED_SIGN_IN_HINT
        } else {
            SIGN_IN_HINT
        }
    }

    /// Tries a renewal, at most once per sign-in file in [`RENEW_BACKOFF`]. True when Claude Code ran it.
    fn renew(&self) -> bool {
        let (Some(renewer), Some(config)) = (&self.renewer, self.credentials_path.parent()) else {
            return false;
        };
        let due = with_state(&self.credentials_path, |state| {
            let now = Instant::now();
            if state
                .renewed_at
                .is_some_and(|at| now.duration_since(at) < RENEW_BACKOFF)
            {
                return false;
            }
            state.renewed_at = Some(now);
            true
        });
        due && renewer.renew(config).is_ok()
    }

    fn label_for(&self, plan: Option<&str>) -> Option<String> {
        match (self.account.as_ref().map(|(_, label)| label.trim()), plan) {
            (Some(label), Some(plan)) if !label.is_empty() => Some(format!("{label} · {plan}")),
            (Some(label), None) if !label.is_empty() => Some(label.to_owned()),
            (_, plan) => plan.map(str::to_owned),
        }
    }

    fn request(&self, credentials: &Credentials, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let bearer = format!("Bearer {}", credentials.access_token);
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Accept", "application/json"),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("User-Agent", "claude-cli/2.1.12 (external, cli)"),
        ];
        let response = self.http.get(USAGE_ENDPOINT, &headers)?;
        match response.status {
            200..=299 => parse_usage(&response.body, credentials.subscription.as_deref(), now),
            429 => {
                let retry_after = response
                    .retry_after_secs
                    .map(|secs| Duration::seconds(secs as i64))
                    .unwrap_or_default();
                let until = now + retry_after.max(Duration::minutes(RATE_LIMIT_BACKOFF_MINUTES));
                let id = self.identity();
                with_state(&self.credentials_path, |state| state.backoff_until = Some((until, id)));
                Err(ProviderError::RateLimited { retry_at: until })
            }
            401 => Err(ProviderError::Expired { hint: self.hint }),
            403 => {
                let id = self.identity();
                with_state(&self.credentials_path, |state| {
                    state.backoff_until = Some((now + Duration::hours(FORBIDDEN_BACKOFF_HOURS), id));
                });
                Err(ProviderError::Http { status: 403 })
            }
            status => Err(ProviderError::Http { status }),
        }
    }

    /// The usage with the sign-in as it is now: renewed first when it expires within minutes, and renewed and tried
    /// once more when the endpoint refuses it.
    fn fresh(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let mut credentials = self.read()?;
        let expiring = |credentials: &Credentials| {
            credentials
                .expires_at
                .is_some_and(|at| at <= now + Duration::minutes(RENEW_AHEAD_MINUTES))
        };
        let mut renewed = false;
        if expiring(&credentials) && self.renew() {
            renewed = true;
            credentials = self.read()?;
        }
        // Claude Code reports a folder signed in even when its refresh failed, so the file decides.
        if credentials.expires_at.is_some_and(|at| at <= now) {
            return Err(ProviderError::Expired { hint: self.hint });
        }
        match self.request(&credentials, now) {
            Err(ProviderError::Expired { .. }) if !renewed && self.renew() => {
                let credentials = self.read()?;
                self.request(&credentials, now)
            }
            result => result,
        }
    }
}

impl<H: HttpClient> UsageProvider for ClaudeProvider<H> {
    fn name(&self) -> &'static str {
        "Claude"
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

    /// No sign-in fails the adapter as a whole. Otherwise the account is known from Claude Code's profile, so a
    /// failure (expiry, backoff) is that account's, never another identity's. Fresh results are reused for a while.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        if !has_credentials(&self.credentials_path) {
            return Err(ProviderError::NotSignedIn {
                hint: self.not_signed_in_hint(),
            });
        }
        let id = self.identity();
        let label = self.label_for(None);
        let failed = |error| {
            Ok(vec![AccountOutcome::Failed {
                account: id.clone(),
                label: label.clone(),
                error,
            }])
        };
        let held =
            with_state(&self.credentials_path, |state| {
                if let Some(cached) = state.cached.as_ref().filter(|cached| {
                    cached.id() == &id && now - cached.fetched_at() < Duration::minutes(CACHE_TTL_MINUTES)
                }) {
                    return Some(Ok(cached.clone()));
                }
                state
                    .backoff_until
                    .as_ref()
                    .filter(|(until, held)| now < *until && *held == id)
                    .map(|(until, _)| Err(ProviderError::RateLimited { retry_at: *until }))
            });
        match held {
            Some(Ok(cached)) => return Ok(vec![AccountOutcome::Fresh(cached)]),
            Some(Err(error)) => return failed(error),
            None => {}
        }
        match self.fresh(now) {
            Ok(usage) => {
                let mut account = AccountSnapshot::new(id.clone(), Provider::Claude, usage.metrics().to_vec(), now);
                if let Some(label) = self.label_for(usage.label()) {
                    account = account.with_label(label);
                }
                for message in usage.messages() {
                    account = account.with_message(message.clone());
                }
                let switched = with_state(&self.credentials_path, |state| {
                    let switched = state.last_account.as_ref().is_some_and(|last| *last != id);
                    state.last_account = Some(id.clone());
                    state.cached = Some(account.clone());
                    state.backoff_until = None;
                    switched
                });
                if switched {
                    account = account.with_message(
                        "Signed in to a different Claude account. It is a new account here; the previous one's usage isn't carried over.",
                    );
                }
                Ok(vec![AccountOutcome::Fresh(account)])
            }
            Err(error @ ProviderError::NotSignedIn { .. }) => Err(error),
            Err(error) => failed(error),
        }
    }

    fn account_id(&self) -> Option<&str> {
        self.account.as_ref().map(|(id, _)| id.as_str())
    }

    fn account_label(&self) -> Option<&str> {
        self.account.as_ref().map(|(_, label)| label.as_str())
    }

    /// The folder's signed-in account; a configured one that is signed out still holds the identity it had.
    fn signed_in_account(&self) -> Option<AccountId> {
        let current = self
            .profile
            .as_deref()
            .filter(|_| seems_signed_in(&self.credentials_path))
            .and_then(signed_in_identity)
            .map(|(id, _)| id);
        current.or_else(|| self.account.as_ref().map(|(id, _)| AccountId::new(id.clone())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::HttpResponse;
    use codexbar_core::Severity;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/claude-usage.json");

    struct FakeHttp {
        responses: Vec<HttpResponse>,
        calls: AtomicUsize,
    }

    impl FakeHttp {
        fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl HttpClient for FakeHttp {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.responses[call.min(self.responses.len() - 1)].clone())
        }
    }

    struct CredentialsFile(PathBuf);

    impl CredentialsFile {
        fn new(name: &str, expires_at: DateTime<Utc>) -> Self {
            let path = std::env::temp_dir().join(format!("codexbar-claude-{}-{name}.json", std::process::id()));
            let body = format!(
                r#"{{"claudeAiOauth":{{"accessToken":"secret-token","refreshToken":"r","expiresAt":{},"subscriptionType":"pro"}}}}"#,
                expires_at.timestamp_millis()
            );
            std::fs::write(&path, body).unwrap();
            Self(path)
        }
    }

    impl Drop for CredentialsFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn provider(name: &str, responses: Vec<HttpResponse>) -> (ClaudeProvider<FakeHttp>, CredentialsFile) {
        let file = CredentialsFile::new(name, now() + Duration::hours(8));
        (ClaudeProvider::new(FakeHttp::new(responses), file.0.clone()), file)
    }

    #[test]
    fn structured_limits_and_spend_map_every_window_and_credit_in_its_currency() {
        let payload = include_str!("../tests/fixtures/claude-usage-current.json");
        let account = parse_usage(payload, Some("max"), now()).unwrap();
        let keys: Vec<String> = account.metrics().iter().map(Metric::key).collect();
        assert_eq!(
            keys,
            [
                "5-hour-window",
                "weekly",
                "weekly-fable",
                "extra-usage",
                "credit-balance"
            ],
            "the scoped Fable limit stays distinct from Weekly; unknown kinds are skipped"
        );
        let fable = &account.metrics()[2];
        assert_eq!(fable.used_fraction(), Some(0.74));
        assert!(fable.resets_at().is_some());
        let credits = &account.metrics()[3];
        assert_eq!(credits.used_display(), "S$12.34 of S$60.00");
        assert_eq!(credits.headroom(), Some(Money::new(4766, Currency::Sgd)));
        assert_eq!(
            account.metrics()[4].used_display(),
            "S$100.00 left",
            "a prepaid balance, not derived"
        );
        assert!(account.messages().is_empty());
    }

    fn with(body: &str) -> Result<AccountSnapshot, ProviderError> {
        parse_usage(&format!("{{{body}}}"), None, now())
    }

    const SESSION_FLAT: &str = r#""five_hour":{"utilization":10,"resets_at":"2026-10-07T03:00:00Z"}"#;

    #[test]
    fn flat_fields_fill_in_without_promoting_codenames() {
        let account = with(&format!(
            r#"{SESSION_FLAT},
            "seven_day":{{"utilization":40,"resets_at":"2026-10-10T08:00:00Z"}},
            "seven_day_opus":{{"utilization":12,"resets_at":"2026-10-10T08:00:00Z"}},
            "seven_day_omelette":{{"utilization":90,"resets_at":"2026-10-10T08:00:00Z"}},
            "limits":[{{"kind":"weekly_scoped","percent":5,"resets_at":"2026-10-10T08:00:00Z",
                "scope":{{"model":{{"display_name":"Opus"}}}}}}]"#
        ))
        .unwrap();
        let shown: Vec<(&str, Option<f64>)> = account
            .metrics()
            .iter()
            .map(|metric| (metric.label(), metric.used_fraction()))
            .collect();
        // The structured Opus limit wins over the flat one; the codename never shows.
        assert_eq!(
            shown,
            [
                ("5-hour window", Some(0.1)),
                ("Weekly", Some(0.4)),
                ("Weekly Opus", Some(0.05))
            ]
        );
        // The OAuth-apps window stands in for Weekly when that's all there is.
        let apps = with(r#""seven_day_oauth_apps":{"utilization":20,"resets_at":"2026-10-10T08:00:00Z"}"#).unwrap();
        assert_eq!(apps.metrics()[0].label(), "Weekly");
    }

    #[test]
    fn extra_usage_is_read_when_spend_is_absent() {
        let usd = with(&format!(
            r#"{SESSION_FLAT},"extra_usage":{{"is_enabled":true,"monthly_limit":5000,"used_credits":0.0,
                "utilization":null,"currency":"USD","decimal_places":2}}"#
        ))
        .unwrap();
        assert_eq!(usd.metrics()[1].used_display(), "$0.00 of $50.00");
        let whole = with(&format!(
            r#"{SESSION_FLAT},"extra_usage":{{"is_enabled":true,"monthly_limit":60,"used_credits":12,
                "currency":"CAD","decimal_places":0}}"#
        ))
        .unwrap();
        assert_eq!(whole.metrics()[1].used_display(), "CA$12.00 of CA$60.00");
        let uncapped = with(&format!(
            r#"{SESSION_FLAT},"extra_usage":{{"is_enabled":true,"monthly_limit":null,"used_credits":500,"currency":"EUR"}}"#
        ))
        .unwrap();
        assert_eq!(uncapped.metrics()[1].used_display(), "€5.00 spent");
        // Off or not reported: nothing.
        for extra in [r#"{"is_enabled":false}"#, "null", r#"{"is_enabled":true}"#] {
            let account = with(&format!(r#"{SESSION_FLAT},"extra_usage":{extra}"#)).unwrap();
            assert_eq!(account.metrics().len(), 1, "{extra}");
        }
    }

    #[test]
    fn credits_without_a_known_currency_are_never_shown_as_dollars() {
        let missing = with(&format!(
            r#"{SESSION_FLAT},"extra_usage":{{"is_enabled":true,"monthly_limit":1000,"used_credits":277}}"#
        ))
        .unwrap();
        assert_eq!(missing.metrics().len(), 1, "no amount without a currency");
        assert_eq!(
            missing.messages(),
            ["Claude didn't say which currency usage credits are in, so they aren't shown."]
        );
        let unknown = with(&format!(
            r#"{SESSION_FLAT},"spend":{{"enabled":true,"used":{{"amount_minor":10,"currency":"XAU","exponent":2}}}}"#
        ))
        .unwrap();
        assert_eq!(unknown.metrics().len(), 1);
        assert_eq!(
            unknown.messages(),
            ["Usage credits are billed in XAU, which CodexBar can't show yet."]
        );
    }

    #[test]
    fn a_bad_optional_amount_leaves_the_others() {
        // The limit's currency is unknown: the spend shows without a cap, the balance still shows.
        let account = with(
            r#""spend":{"enabled":true,
                "used":{"amount_minor":2500,"currency":"USD","exponent":2},
                "limit":{"amount_minor":10000,"currency":"XAU","exponent":2},
                "balance":{"amount_minor":4000,"currency":"USD","exponent":2}}"#,
        )
        .unwrap();
        let shown: Vec<String> = account.metrics().iter().map(Metric::used_display).collect();
        assert_eq!(shown, ["$25.00 spent", "$40.00 left"]);
        assert_eq!(
            account.messages(),
            ["Usage credits are billed in XAU, which CodexBar can't show yet."]
        );
    }

    #[test]
    fn an_account_with_only_credits_is_shown() {
        // Enterprise and credit-based accounts can report money without subscription windows.
        let account = with(
            r#""five_hour":null,"seven_day":null,"spend":{"enabled":true,
                "used":{"amount_minor":2500,"currency":"USD","exponent":2},
                "limit":{"amount_minor":10000,"currency":"USD","exponent":2}}"#,
        )
        .unwrap();
        assert_eq!(account.metrics().len(), 1);
        assert_eq!(account.primary().unwrap().used_display(), "$25.00 of $100.00");
        assert!(
            with(r#""five_hour":null,"spend":{"enabled":false}"#).is_err(),
            "nothing at all is still an error"
        );
    }

    #[test]
    fn credits_near_their_limit_raise_the_account_status() {
        let account = with(&format!(
            r#"{SESSION_FLAT},"spend":{{"enabled":true,"used":{{"amount_minor":5760,"currency":"SGD","exponent":2}},
                "limit":{{"amount_minor":6000,"currency":"SGD","exponent":2}}}}"#
        ))
        .unwrap();
        assert_eq!(account.assess(now()).severity(), Severity::LimitSoon);
    }

    #[test]
    fn incomplete_windows_are_skipped_not_fatal() {
        let account = with(
            r#""five_hour":{"utilization":10,"resets_at":null},
            "seven_day":{"utilization":40,"resets_at":"2026-10-10T08:00:00Z"},
            "limits":[{"kind":"session","percent":0,"resets_at":null},
                {"kind":"weekly_scoped","percent":5,"resets_at":"2026-10-10T08:00:00Z","scope":{"model":{"display_name":" "}}}]"#,
        )
        .unwrap();
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, ["Weekly"]);
    }

    #[test]
    fn parse_usage_fixture_maps_windows_and_plan() {
        let account = parse_usage(FIXTURE, Some("pro"), now()).unwrap();
        assert_eq!(account.display_name(), "Claude · Pro");
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, ["5-hour window", "Weekly"]);
        assert_eq!(account.primary().unwrap().used_fraction(), Some(0.64));
        // Weekly: 82% after 5.5 days runs out about six hours before Thursday's reset.
        assert_eq!(account.assess(now()).severity(), Severity::AtRisk);
    }

    #[test]
    fn parse_usage_without_windows_is_unexpected() {
        assert!(matches!(
            parse_usage(r#"{"five_hour":null,"seven_day":null}"#, None, now()),
            Err(ProviderError::Unexpected { .. })
        ));
    }

    #[test]
    fn keychain_items_are_named_as_claude_code_names_them() {
        // Claude Code: `Claude Code-credentials`, plus `-` and sha256(folder)[..8] for a folder that isn't its own.
        assert_eq!(
            keychain_service(Path::new("/Users/home/.codexbar/claude/abc")),
            "Claude Code-credentials-88a2166c"
        );
        // Claude Code keeps MCP servers' tokens beside the sign-in; those alone are no sign-in.
        assert!(parse_credentials(r#"{"mcpOAuth":{"server":{"accessToken":"x"}}}"#).is_none());
        assert!(parse_credentials(r#"{"claudeAiOauth":{"accessToken":" "}}"#).is_none());
        assert!(parse_credentials(r#"{"claudeAiOauth":{"accessToken":"x"},"mcpOAuth":{}}"#).is_some());
        // An item without a Claude sign-in is signed out even beside a sign-in file; without an item the file decides.
        assert_eq!(KeychainItem::of(r#"{"mcpOAuth":{}}"#), KeychainItem::NoSignIn);
        assert!(!KeychainItem::NoSignIn.or_file(Path::new("Cargo.toml")));
        assert!(KeychainItem::SignedIn.or_file(Path::new("missing/.credentials.json")));
        // A folder CodexBar signed in keeps its sign-in file in itself.
        let file = Path::new("/Users/home/.codexbar/claude/abc/.credentials.json");
        assert_eq!(credentials_file(file), file);
        // Claude Code hashes the path in NFC, so a decomposed "é" names the same item as a composed one.
        assert_eq!(
            keychain_service(Path::new("/Users/home/Caf\u{65}\u{301}/claude")),
            keychain_service(Path::new("/Users/home/Caf\u{e9}/claude"))
        );
        // Its own folder has the plain name, but only while no folder is set: a set one is hashed even when it is
        // `~/.claude`.
        if ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"]
            .iter()
            .all(|name| std::env::var_os(name).is_none())
        {
            let home = crate::platform::home_dir().expect("a home folder");
            assert_eq!(keychain_service(&home.join(".claude")), "Claude Code-credentials");
        }
    }

    #[test]
    fn fetch_caches_fresh_results() {
        let (provider, _file) = provider("cache", vec![HttpResponse::new(200, FIXTURE)]);
        provider.fetch(now()).unwrap();
        provider.fetch(now() + Duration::minutes(5)).unwrap();
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 1);
        provider.fetch(now() + Duration::minutes(11)).unwrap();
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fetch_rate_limited_backs_off_for_retry_after_at_least_fifteen_minutes() {
        let limited = HttpResponse {
            retry_after_secs: Some(3600),
            ..HttpResponse::new(429, "")
        };
        let (provider, _file) = provider("429", vec![limited, HttpResponse::new(200, FIXTURE)]);
        let err = provider.fetch(now()).unwrap_err();
        assert_eq!(
            err,
            ProviderError::RateLimited {
                retry_at: now() + Duration::hours(1)
            }
        );
        // Within the backoff no request is made at all.
        assert!(provider.fetch(now() + Duration::minutes(30)).is_err());
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 1);
        assert!(provider.fetch(now() + Duration::minutes(61)).is_ok());
    }

    #[test]
    fn fetch_rate_limited_short_retry_after_still_waits_fifteen_minutes() {
        let limited = HttpResponse {
            retry_after_secs: Some(30),
            ..HttpResponse::new(429, "")
        };
        let (provider, _file) = provider("429-short", vec![limited]);
        let err = provider.fetch(now()).unwrap_err();
        assert_eq!(
            err,
            ProviderError::RateLimited {
                retry_at: now() + Duration::minutes(15)
            }
        );
    }

    #[test]
    fn fetch_forbidden_backs_off_six_hours() {
        let (provider, _file) = provider("403", vec![HttpResponse::new(403, "")]);
        assert_eq!(provider.fetch(now()), Err(ProviderError::Http { status: 403 }));
        assert!(matches!(
            provider.fetch(now() + Duration::hours(5)),
            Err(ProviderError::RateLimited { .. })
        ));
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fetch_expired_token_does_not_call_or_refresh() {
        let file = CredentialsFile::new("expired", now() - Duration::minutes(1));
        let provider = ClaudeProvider::new(FakeHttp::new(vec![HttpResponse::new(200, FIXTURE)]), file.0.clone());
        assert_eq!(
            provider.fetch(now()),
            Err(ProviderError::Expired { hint: EXPIRED_HINT })
        );
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 0);
        // The credentials file is untouched.
        assert!(
            std::fs::read_to_string(&file.0)
                .unwrap()
                .contains("\"refreshToken\":\"r\"")
        );
    }

    /// A Claude Code config folder for one test: `.credentials.json` and `.claude.json`, removed at the end.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("codexbar-claude-dir-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn sign_in(&self, token: &str, expires_at: DateTime<Utc>, account: &str) {
            std::fs::write(
                self.0.join(".credentials.json"),
                format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"r","expiresAt":{},"subscriptionType":"max"}}}}"#,
                    expires_at.timestamp_millis()
                ),
            )
            .unwrap();
            std::fs::write(
                self.0.join(".claude.json"),
                format!(
                    r#"{{"oauthAccount":{{"accountUuid":"{account}","organizationUuid":"org-1","emailAddress":"{account}@example.com"}}}}"#
                ),
            )
            .unwrap();
        }

        fn provider(&self, responses: Vec<HttpResponse>) -> ClaudeProvider<FakeHttp> {
            ClaudeProvider::new(FakeHttp::new(responses), self.0.join(".credentials.json"))
                .with_profile(self.0.join(".claude.json"))
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Stands in for Claude Code: a renewal writes `renewed` (if any) as the new token, and counts the calls.
    struct FakeRenewer {
        folder: PathBuf,
        renewed: Option<String>,
        calls: AtomicUsize,
    }

    impl Renewer for FakeRenewer {
        fn renew(&self, config: &Path) -> Result<(), ClaudeCliError> {
            assert_eq!(config, self.folder.as_path());
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(token) = &self.renewed {
                let path = config.join(".credentials.json");
                let text = std::fs::read_to_string(&path).unwrap();
                let renewed = text.replace(
                    &text[text.find("\"accessToken\":\"").unwrap()..text.find("\",\"refreshToken\"").unwrap()],
                    &format!("\"accessToken\":\"{token}"),
                );
                let later = (now() + Duration::hours(8)).timestamp_millis().to_string();
                let start = renewed.find("\"expiresAt\":").unwrap() + "\"expiresAt\":".len();
                let end = start + renewed[start..].find(',').unwrap();
                std::fs::write(&path, format!("{}{later}{}", &renewed[..start], &renewed[end..])).unwrap();
            }
            // Like Claude Code, it reports success even when the refresh didn't change anything.
            Ok(())
        }
    }

    fn renewer(folder: &Folder, renewed: Option<&str>) -> Arc<FakeRenewer> {
        Arc::new(FakeRenewer {
            folder: folder.0.clone(),
            renewed: renewed.map(str::to_owned),
            calls: AtomicUsize::new(0),
        })
    }

    #[test]
    fn the_cache_and_backoff_survive_a_new_provider() {
        // The dashboard builds its providers anew for every refresh.
        let folder = Folder::new("state");
        folder.sign_in("tok", now() + Duration::hours(8), "user-1");
        let first = folder.provider(vec![HttpResponse::new(200, FIXTURE)]);
        first.fetch(now()).unwrap();
        let again = folder.provider(vec![HttpResponse::new(200, FIXTURE)]);
        again.fetch(now() + Duration::minutes(5)).unwrap();
        assert_eq!(
            again.http.calls.load(Ordering::SeqCst),
            0,
            "the cached result is reused"
        );

        let limited = Folder::new("state-429");
        limited.sign_in("tok", now() + Duration::hours(8), "user-1");
        let mut response = HttpResponse::new(429, "");
        response.retry_after_secs = Some(3600);
        assert!(limited.provider(vec![response]).fetch(now()).is_err());
        let next = limited.provider(vec![HttpResponse::new(200, FIXTURE)]);
        assert!(matches!(
            next.fetch(now() + Duration::minutes(30)),
            Err(ProviderError::RateLimited { .. })
        ));
        assert_eq!(
            next.http.calls.load(Ordering::SeqCst),
            0,
            "the backoff holds across refreshes"
        );
    }

    #[test]
    fn each_claude_identity_is_its_own_account() {
        let folder = Folder::new("identity");
        folder.sign_in("tok", now() + Duration::hours(8), "user-1");
        let (first, email) = signed_in_identity(&folder.0.join(".claude.json")).unwrap();
        assert!(
            first.as_str().starts_with("claude-") && first.as_str().len() == 19,
            "{first:?}"
        );
        assert_eq!(email.as_deref(), Some("user-1@example.com"));
        folder.sign_in("tok", now() + Duration::hours(8), "user-2");
        let (second, _) = signed_in_identity(&folder.0.join(".claude.json")).unwrap();
        assert_ne!(first, second);
        assert!(signed_in_identity(&folder.0.join("missing.json")).is_none());
        // Without a profile the account keeps the legacy id.
        let (provider, _file) = provider("legacy-id", vec![HttpResponse::new(200, FIXTURE)]);
        assert_eq!(provider.fetch(now()).unwrap()[0].id().as_str(), LEGACY_ID);
    }

    #[test]
    fn a_sign_in_about_to_expire_is_renewed_by_claude_code_first() {
        let folder = Folder::new("renew");
        folder.sign_in("old", now() + Duration::minutes(2), "user-1");
        let renewer = renewer(&folder, Some("new"));
        let provider = folder
            .provider(vec![HttpResponse::new(200, FIXTURE)])
            .with_renewer(renewer.clone());
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(renewer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            account.id(),
            &signed_in_identity(&folder.0.join(".claude.json")).unwrap().0
        );
    }

    #[test]
    fn a_renewal_that_changed_nothing_is_still_an_expiry() {
        // Claude Code says signed in even when its refresh failed; the file decides.
        let folder = Folder::new("renew-failed");
        folder.sign_in("old", now() - Duration::minutes(1), "user-1");
        let renewer = renewer(&folder, None);
        let provider = folder
            .provider(vec![HttpResponse::new(200, FIXTURE)])
            .managed()
            .with_account("record-1", "Work")
            .with_renewer(renewer.clone());
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { error, label, .. } = &outcomes[0] else {
            panic!("expected an expiry")
        };
        assert_eq!(
            error,
            &ProviderError::Expired {
                hint: MANAGED_SIGN_IN_HINT
            }
        );
        assert_eq!(label.as_deref(), Some("Work"));
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 0);
        // The next refresh doesn't ask Claude Code again right away.
        provider.fetch_outcomes(now() + Duration::minutes(1)).unwrap();
        assert_eq!(renewer.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_sign_in_is_renewed_and_tried_once_more() {
        let folder = Folder::new("renew-401");
        folder.sign_in("revoked", now() + Duration::hours(8), "user-1");
        let renewer = renewer(&folder, Some("new"));
        let provider = folder
            .provider(vec![HttpResponse::new(401, ""), HttpResponse::new(200, FIXTURE)])
            .with_renewer(renewer.clone());
        assert_eq!(provider.fetch(now()).unwrap().len(), 1);
        assert_eq!(renewer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.http.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_folder_signed_in_to_another_account_starts_a_separate_one() {
        let folder = Folder::new("switch");
        folder.sign_in("tok", now() + Duration::hours(8), "user-1");
        let first = folder
            .provider(vec![HttpResponse::new(200, FIXTURE)])
            .fetch(now())
            .unwrap()
            .remove(0);
        assert!(!first.messages().iter().any(|m| m.contains("different Claude account")));
        folder.sign_in("tok", now() + Duration::hours(8), "user-2");
        // A fresh fetch (the first one is still cached, but for the other identity).
        let second = folder
            .provider(vec![HttpResponse::new(200, FIXTURE)])
            .fetch(now())
            .unwrap()
            .remove(0);
        assert_ne!(first.id(), second.id());
        assert!(second.messages().iter().any(|m| m.contains("different Claude account")));
    }

    #[test]
    fn a_backoff_holds_only_the_account_that_earned_it() {
        let folder = Folder::new("backoff-identity");
        folder.sign_in("tok", now() + Duration::hours(8), "user-a");
        assert!(folder.provider(vec![HttpResponse::new(403, "")]).fetch(now()).is_err());
        // Signed in to another account: its first fetch isn't held back by the first account's 403.
        folder.sign_in("tok", now() + Duration::hours(8), "user-b");
        let other = folder.provider(vec![HttpResponse::new(200, FIXTURE)]);
        assert_eq!(other.fetch(now() + Duration::minutes(1)).unwrap().len(), 1);
        assert_eq!(other.http.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_configured_account_that_is_signed_out_keeps_its_identity_and_label() {
        let folder = Folder::new("signed-out");
        let provider = folder
            .provider(vec![HttpResponse::new(200, FIXTURE)])
            .managed()
            .with_account("claude-aaaaaaaaaaaa", "Work");
        assert_eq!(
            provider.signed_in_account().map(|id| id.as_str().to_owned()).as_deref(),
            Some("claude-aaaaaaaaaaaa")
        );
        assert_eq!(
            provider.fetch_outcomes(now()).err(),
            Some(ProviderError::NotSignedIn {
                hint: MANAGED_SIGN_IN_HINT
            })
        );
        folder.sign_in("tok", now() + Duration::hours(8), "user-1");
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.label(), Some("Work · Max"));
    }

    #[test]
    fn credentials_debug_redacts_token() {
        let file = CredentialsFile::new("debug", now());
        let debug = format!("{:?}", read_credentials(&file.0).unwrap());
        assert!(!debug.contains("secret-token"));
    }
}
