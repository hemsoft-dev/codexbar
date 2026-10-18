//! GitHub Copilot premium-request quotas for every account signed in to the GitHub CLI, and for accounts CodexBar
//! signed in itself (#79). An account on a Copilot Enterprise seat can also show its organization's AI credits this
//! month and its own share, from the enterprise billing API (configured per account).

use std::time::Duration as StdDuration;

use chrono::{DateTime, Datelike as _, Months, NaiveDate, TimeZone as _, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider};
use serde_json::Value;

use crate::AccountOutcome;
use crate::pace::elapsed_pace;
use crate::{CommandError, CommandRunner, HttpClient, ProviderError, UsageProvider};

const USER_ENDPOINT: &str = "https://api.github.com/copilot_internal/user";
const REST_API: &str = "https://api.github.com";
const REST_API_VERSION: &str = "2026-03-10";
const USER_AGENT: &str = "CodexBar";
/// AI credits per seat each month; June to August 2026 had a promotional allowance.
const CREDITS_PER_SEAT: u64 = 3900;
const PROMOTIONAL_CREDITS_PER_SEAT: u64 = 7000;
/// The hint for an account CodexBar signed in.
pub const MANAGED_LOGIN: &str = "Sign in again in Settings > Accounts.";
const HOST: &str = "github.com";
const GH_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const GH_MISSING: &str = "Install the GitHub CLI and run `gh auth login`.";
const GH_LOGIN: &str = "Run `gh auth login` for this account.";

/// Usernames from `gh auth status` output ("✓ Logged in to github.com account HemSoft (keyring)").
pub fn parse_gh_accounts(status: &str) -> Vec<String> {
    let mut accounts = Vec::new();
    for line in status.lines() {
        let line = line.trim();
        if !line.to_lowercase().contains("logged in to github.com") {
            continue;
        }
        let name = ["account ", " as "].iter().find_map(|marker| {
            let start = line.to_lowercase().find(marker)? + marker.len();
            line[start..].split_whitespace().next()
        });
        if let Some(name) = name.filter(|name| !name.is_empty())
            && !accounts.iter().any(|known: &String| known.eq_ignore_ascii_case(name))
        {
            accounts.push(name.to_owned());
        }
    }
    accounts
}

/// Maps a `copilot_internal/user` response to one account. Unlimited and absent quotas are not limits, so they
/// produce no metric rather than a false 0%.
/// A Copilot account's stable id: its GitHub username, lowercased.
pub fn account_id(username: &str) -> AccountId {
    AccountId::new(format!("copilot-{}", username.to_lowercase()))
}

pub fn parse_user(payload: &str, username: &str, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
    let json: Value = serde_json::from_str(payload).map_err(|_| ProviderError::Unexpected { detail: "not JSON" })?;
    let resets_at = reset_date(&json);
    let mut metrics = Vec::new();
    if let Some(resets_at) = resets_at {
        for (key, label) in QUOTAS {
            if let Some(metric) = json
                .pointer(&format!("/quota_snapshots/{key}"))
                .and_then(|quota| quota_metric(quota, label, resets_at, now))
            {
                metrics.push(metric);
            }
        }
    }

    Ok(AccountSnapshot::new(account_id(username), Provider::Copilot, metrics, now).with_label(username))
}

/// Quotas in display order. Paid plans meter premium requests; Copilot Free meters chat and completions.
const QUOTAS: [(&str, &str); 3] = [
    ("premium_interactions", "Premium requests"),
    ("chat", "Chat messages"),
    ("completions", "Code completions"),
];

/// A monthly quota with a real ceiling. Unlimited or zero entitlements are not limits.
fn quota_metric(quota: &Value, label: &str, resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Metric> {
    let entitlement = quota.get("entitlement").and_then(Value::as_u64).filter(|e| *e > 0)?;
    if quota.get("unlimited").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let remaining = quota.get("remaining").and_then(Value::as_i64).unwrap_or(0).max(0) as u64;
    let used = entitlement.saturating_sub(remaining);
    let start = resets_at.checked_sub_months(Months::new(1)).unwrap_or(resets_at);
    let pace = elapsed_pace(used as f64 / entitlement as f64, start, resets_at, now);
    Some(Metric::Quota {
        label: label.into(),
        used,
        limit: entitlement,
        resets_at,
        pace,
    })
}

/// `quota_reset_date_utc` (RFC 3339), falling back to the date-only `quota_reset_date` at midnight UTC.
fn reset_date(json: &Value) -> Option<DateTime<Utc>> {
    if let Some(stamp) = json.get("quota_reset_date_utc").and_then(Value::as_str)
        && let Ok(at) = DateTime::parse_from_rfc3339(stamp)
    {
        return Some(at.with_timezone(&Utc));
    }
    let date = json.get("quota_reset_date").and_then(Value::as_str)?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc())
}

/// Where an account on a Copilot Enterprise seat reads its organization's AI credits: the enterprise and organization
/// slugs, and optionally the pool size (otherwise seats times the monthly allowance per seat).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgBilling {
    pub enterprise: String,
    pub organization: String,
    pub pool_total: Option<u64>,
}

/// A Copilot account CodexBar signed in itself, with its own token. `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq)]
struct Login {
    username: String,
    token: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// The seat details `copilot_internal/user` reports beside the quotas.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Seat {
    plan: Option<String>,
    organizations: Vec<String>,
}

fn parse_seat(payload: &str) -> Seat {
    let Ok(json) = serde_json::from_str::<Value>(payload) else {
        return Seat::default();
    };
    Seat {
        plan: json.get("copilot_plan").and_then(Value::as_str).map(str::to_owned),
        organizations: json
            .get("organization_login_list")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    }
}

/// The organization's dashboard account. Users are `copilot-<user>`; organizations use another prefix, so a user
/// named like `org-acme` never shares an id with the organization `acme`.
pub fn org_account_id(organization: &str) -> AccountId {
    AccountId::new(format!("copilotorg-{}", organization.to_lowercase()))
}

/// AI credits each seat gets in this month.
pub fn credits_per_seat(year: i32, month: u32) -> u64 {
    if year == 2026 && (6..=8).contains(&month) {
        PROMOTIONAL_CREDITS_PER_SEAT
    } else {
        CREDITS_PER_SEAT
    }
}

/// The calendar month `now` is in, in UTC, as the billing API counts it.
fn billing_month(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now);
    let end = start.checked_add_months(Months::new(1)).unwrap_or(start);
    (start, end)
}

fn credits_metric(label: &str, used: f64, limit: u64, now: DateTime<Utc>) -> Metric {
    let (start, end) = billing_month(now);
    let used = used.max(0.0).round() as u64;
    Metric::Quota {
        label: label.into(),
        used,
        limit,
        resets_at: end,
        pace: elapsed_pace(used as f64 / limit as f64, start, end, now),
    }
}

pub struct CopilotProvider<H: HttpClient, C: CommandRunner> {
    http: H,
    commands: C,
    /// When set, only these gh usernames are fetched (case-insensitive); otherwise every signed-in account.
    only: Option<Vec<String>>,
    /// Whether accounts are found through the GitHub CLI. Off when every Copilot account is one CodexBar signed in.
    use_cli: bool,
    logins: Vec<Login>,
    /// Accounts CodexBar signed in whose saved token couldn't be read.
    unreadable: Vec<String>,
    /// Org billing per username (lowercase).
    billing: Vec<(String, OrgBilling)>,
}

impl<H: HttpClient, C: CommandRunner> CopilotProvider<H, C> {
    pub fn new(http: H, commands: C) -> Self {
        Self {
            http,
            commands,
            only: None,
            use_cli: true,
            logins: Vec::new(),
            unreadable: Vec::new(),
            billing: Vec::new(),
        }
    }

    pub fn only_accounts(mut self, usernames: Vec<String>) -> Self {
        self.only = Some(usernames);
        self
    }

    /// Fetches only accounts CodexBar signed in, never the GitHub CLI's.
    pub fn without_cli(mut self) -> Self {
        self.use_cli = false;
        self
    }

    /// An account CodexBar signed in, fetched with its own token instead of the GitHub CLI's.
    pub fn with_login(mut self, username: impl Into<String>, token: impl Into<String>) -> Self {
        self.logins.push(Login {
            username: username.into(),
            token: token.into(),
        });
        self
    }

    /// An account CodexBar signed in whose saved token couldn't be read: it is reported as failing, not fetched.
    pub fn with_unreadable_login(mut self, username: impl Into<String>) -> Self {
        self.unreadable.push(username.into());
        self
    }

    /// Reads `username`'s organization AI credits from the enterprise billing API.
    pub fn with_billing(mut self, username: &str, billing: OrgBilling) -> Self {
        self.billing.push((username.to_lowercase(), billing));
        self
    }

    fn login(&self, username: &str) -> Option<&Login> {
        self.logins
            .iter()
            .find(|login| login.username.eq_ignore_ascii_case(username))
    }

    fn gh(&self, args: &[&str]) -> Result<String, ProviderError> {
        match self.commands.run("gh", args, GH_TIMEOUT) {
            Ok(output) if output.success => Ok(format!("{}\n{}", output.stdout, output.stderr)),
            Ok(_) => Err(ProviderError::NotSignedIn { hint: GH_LOGIN }),
            Err(CommandError::NotFound) => Err(ProviderError::NotSignedIn { hint: GH_MISSING }),
            Err(CommandError::TimedOut | CommandError::Failed) => Err(ProviderError::Network),
        }
    }

    fn token(&self, username: &str) -> Result<String, ProviderError> {
        if let Some(login) = self.login(username) {
            return Ok(login.token.clone());
        }
        let output = self.commands.run(
            "gh",
            &["auth", "token", "--user", username, "--hostname", HOST],
            GH_TIMEOUT,
        );
        match output {
            Ok(output) if output.success => {
                let token = output.stdout.trim();
                if token.is_empty() {
                    Err(ProviderError::NotSignedIn { hint: GH_LOGIN })
                } else {
                    Ok(token.to_owned())
                }
            }
            Ok(_) => Err(ProviderError::NotSignedIn { hint: GH_LOGIN }),
            Err(CommandError::NotFound) => Err(ProviderError::NotSignedIn { hint: GH_MISSING }),
            Err(_) => Err(ProviderError::Network),
        }
    }

    fn fetch_account(
        &self,
        username: &str,
        now: DateTime<Utc>,
    ) -> Result<(AccountSnapshot, Seat, String), ProviderError> {
        let token = self.token(username)?;
        let expired = if self.login(username).is_some() {
            MANAGED_LOGIN
        } else {
            GH_LOGIN
        };
        let authorization = format!("token {token}");
        let headers = [
            ("Authorization", authorization.as_str()),
            ("Accept", "application/json"),
            ("Editor-Version", "vscode/1.96.2"),
            ("Editor-Plugin-Version", "copilot-chat/0.26.7"),
            ("X-Github-Api-Version", "2025-04-01"),
        ];
        let response = self.http.get(USER_ENDPOINT, &headers)?;
        match response.status {
            200..=299 => {
                parse_user(&response.body, username, now).map(|account| (account, parse_seat(&response.body), token))
            }
            401 | 403 => Err(ProviderError::Expired { hint: expired }),
            // 404: this account has no Copilot seat.
            404 => Err(ProviderError::Unexpected {
                detail: "no Copilot subscription on this account",
            }),
            status => Err(ProviderError::Http { status }),
        }
    }

    /// A GitHub REST API GET with `token`; the body on success.
    fn rest(&self, path: &str, token: &str) -> Result<String, ProviderError> {
        let url = format!("{REST_API}{path}");
        let bearer = format!("Bearer {token}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", REST_API_VERSION),
            ("User-Agent", USER_AGENT),
        ];
        let response = self.http.get(&url, &headers)?;
        match response.status {
            200..=299 => Ok(response.body),
            status => Err(ProviderError::Http { status }),
        }
    }

    /// AI credits this month from the enterprise AI-credit report, filtered by `filter` (`organization=…` or
    /// `user=…`), on `day` or for the whole month.
    fn ai_credits(
        &self,
        billing: &OrgBilling,
        filter: &str,
        day: Option<u32>,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<f64>, ProviderError> {
        let day = day.map(|day| format!("&day={day}")).unwrap_or_default();
        let path = format!(
            "/enterprises/{}/settings/billing/ai_credit/usage?year={}&month={}{day}&{filter}",
            encode(&billing.enterprise),
            now.year(),
            now.month(),
        );
        let body = self.rest(&path, token)?;
        let json: Value = serde_json::from_str(&body).map_err(|_| ProviderError::Unexpected {
            detail: "unreadable AI-credit report",
        })?;
        let items = json
            .get("usageItems")
            .and_then(Value::as_array)
            .ok_or(ProviderError::Unexpected {
                detail: "unreadable AI-credit report",
            })?;
        if items.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            items
                .iter()
                .filter_map(|item| item.get("grossQuantity").and_then(Value::as_f64))
                .sum(),
        ))
    }

    /// The organization's AI credits this month.
    fn org_consumed(&self, billing: &OrgBilling, token: &str, now: DateTime<Utc>) -> Result<f64, ProviderError> {
        let filter = format!("organization={}", encode(&billing.organization));
        Ok(self.ai_credits(billing, &filter, None, token, now)?.unwrap_or(0.0))
    }

    /// The organization's monthly pool: the configured total, else seats times the allowance per seat.
    fn pool(&self, billing: &OrgBilling, token: &str, now: DateTime<Utc>) -> Option<u64> {
        if let Some(total) = billing.pool_total.filter(|total| *total > 0) {
            return Some(total);
        }
        let body = self
            .rest(
                &format!("/orgs/{}/copilot/billing", encode(&billing.organization)),
                token,
            )
            .ok()?;
        let seats = serde_json::from_str::<Value>(&body)
            .ok()?
            .pointer("/seat_breakdown/total")?
            .as_u64()
            .filter(|seats| *seats > 0)?;
        Some(seats * credits_per_seat(now.year(), now.month()))
    }

    /// The user's AI credits this month: the month's report, or, when that comes back empty (as month-wide user
    /// queries did for premium requests), the sum of each day so far. A day that can't be read makes the total
    /// unknown rather than too low.
    fn user_consumed(
        &self,
        billing: &OrgBilling,
        username: &str,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<f64, ProviderError> {
        let filter = format!("user={}", encode(username));
        if let Some(month) = self.ai_credits(billing, &filter, None, token, now)? {
            return Ok(month);
        }
        let mut total = 0.0;
        for day in 1..=now.day() {
            total += self.ai_credits(billing, &filter, Some(day), token, now)?.unwrap_or(0.0);
        }
        Ok(total)
    }

    /// The organization's account, from one user's token.
    fn org_outcome(
        &self,
        billing: &OrgBilling,
        consumed: &Result<f64, ProviderError>,
        token: &str,
        now: DateTime<Utc>,
    ) -> AccountOutcome {
        let id = org_account_id(&billing.organization);
        match consumed {
            Ok(consumed) => {
                let org = match self.pool(billing, token, now) {
                    Some(pool) => AccountSnapshot::new(
                        id,
                        Provider::Copilot,
                        vec![credits_metric("AI credits", *consumed, pool, now)],
                        now,
                    ),
                    None => AccountSnapshot::new(id, Provider::Copilot, Vec::new(), now).with_message(format!(
                        "{consumed:.0} AI credits used this month. Set the pool total in Settings to see how much is left."
                    )),
                };
                AccountOutcome::Fresh(org.with_label(&billing.organization))
            }
            Err(error) => AccountOutcome::Failed {
                account: id,
                label: Some(billing.organization.clone()),
                error: error.clone(),
            },
        }
    }

    /// Adds org billing to a fetched account on an Enterprise seat in the configured organization: the user's AI
    /// credits as the first metric and the share of the organization's as a note. The organization's own account is
    /// kept in `orgs`, one per organization; a later user whose token can read it replaces an earlier failure.
    fn add_billing(
        &self,
        account: AccountSnapshot,
        seat: &Seat,
        token: &str,
        now: DateTime<Utc>,
        orgs: &mut Vec<(String, AccountOutcome)>,
    ) -> AccountSnapshot {
        let username = account.label().unwrap_or_default().to_owned();
        let Some((_, billing)) = self
            .billing
            .iter()
            .find(|(user, _)| user.eq_ignore_ascii_case(&username))
        else {
            return account;
        };
        let eligible = seat
            .plan
            .as_deref()
            .is_some_and(|plan| plan.eq_ignore_ascii_case("enterprise"))
            && seat
                .organizations
                .iter()
                .any(|org| org.eq_ignore_ascii_case(&billing.organization));
        if !eligible {
            return account.with_message(format!(
                "Org billing is set for {}, but this account's Copilot seat isn't an Enterprise seat in it.",
                billing.organization
            ));
        }
        let org_consumed = self.org_consumed(billing, token, now);
        match orgs
            .iter_mut()
            .find(|(org, _)| org.eq_ignore_ascii_case(&billing.organization))
        {
            Some((_, outcome)) => {
                if matches!(outcome, AccountOutcome::Failed { .. }) && org_consumed.is_ok() {
                    *outcome = self.org_outcome(billing, &org_consumed, token, now);
                }
            }
            None => orgs.push((
                billing.organization.clone(),
                self.org_outcome(billing, &org_consumed, token, now),
            )),
        }
        let Ok(consumed) = self.user_consumed(billing, &username, token, now) else {
            return account.with_message(
                "Enterprise billing couldn't be read for this account. Its token needs access to enterprise billing.",
            );
        };
        // AI credits are measured against the seat's monthly AI-credit allowance; the premium-request entitlement
        // counts requests, a different unit.
        let allowance = credits_per_seat(now.year(), now.month());
        let mut metrics = vec![credits_metric("AI credits", consumed, allowance, now)];
        metrics.extend(account.metrics().iter().cloned());
        let mut billed =
            AccountSnapshot::new(account.id().clone(), Provider::Copilot, metrics, now).with_label(username);
        for message in account.messages() {
            billed = billed.with_message(message.clone());
        }
        if let Some(org) = org_consumed.ok().filter(|org| *org > 0.0) {
            billed = billed.with_message(format!(
                "{:.0}% of {}'s AI credits this month.",
                (consumed / org * 100.0).clamp(0.0, 100.0),
                billing.organization
            ));
        }
        billed
    }

    /// The GitHub CLI's accounts. `--json hosts` lists every account, signed in or not, and exits 0 even when one of
    /// them has a problem (the plain status exits 1 then); older GitHub CLIs without it get the plain status.
    fn cli_accounts(&self) -> Result<Vec<String>, ProviderError> {
        match self.gh(&["auth", "status", "--hostname", HOST, "--json", "hosts"]) {
            Ok(output) => {
                if let Some(accounts) = parse_gh_accounts_json(&output) {
                    return Ok(accounts);
                }
            }
            Err(err @ ProviderError::NotSignedIn { hint: GH_MISSING }) => return Err(err),
            Err(_) => {}
        }
        self.gh(&["auth", "status", "--hostname", HOST])
            .map(|status| parse_gh_accounts(&status))
    }
}

/// Usernames on github.com from `gh auth status --json hosts`, or None if the output isn't that JSON.
pub fn parse_gh_accounts_json(output: &str) -> Option<Vec<String>> {
    let json: Value = serde_json::Deserializer::from_str(output.trim_start())
        .into_iter::<Value>()
        .next()?
        .ok()?;
    let entries = json.get("hosts")?.get(HOST)?.as_array()?;
    let mut accounts: Vec<String> = Vec::new();
    for login in entries
        .iter()
        .filter_map(|entry| entry.get("login").and_then(Value::as_str))
    {
        if !login.is_empty() && !accounts.iter().any(|known| known.eq_ignore_ascii_case(login)) {
            accounts.push(login.to_owned());
        }
    }
    Some(accounts)
}

/// Percent-encodes a path or query value.
fn encode(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

impl<H: HttpClient, C: CommandRunner> UsageProvider for CopilotProvider<H, C> {
    fn name(&self) -> &'static str {
        "Copilot"
    }

    /// Every signed-in account. One failing account doesn't hide the others; the call fails only if all do.
    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        let mut first_error = None;
        let mut snapshots = Vec::new();
        for outcome in self.fetch_outcomes(now)? {
            match outcome {
                AccountOutcome::Fresh(snapshot) => snapshots.push(snapshot),
                AccountOutcome::Failed { error, .. } => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match (snapshots.is_empty(), first_error) {
            (true, Some(err)) => Err(err),
            _ => Ok(snapshots),
        }
    }

    /// One outcome per signed-in user, so one user's failure keeps that user's last good usage on the dashboard.
    /// Accounts CodexBar signed in come with their own token; the GitHub CLI's are found with `gh auth status`.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        let mut accounts = Vec::new();
        let mut cli_failures = Vec::new();
        if self.use_cli {
            match self.cli_accounts() {
                Ok(found) => accounts = found,
                // Without the GitHub CLI, CodexBar's own accounts still fetch, and the configured GitHub CLI
                // accounts report the failure (keeping their last usage). With every gh account wanted, which
                // ones they are is unknown, so the provider fails as a whole and nothing is dropped.
                // No GitHub CLI installed: it has no accounts to lose.
                Err(ProviderError::NotSignedIn { hint: GH_MISSING })
                    if !self.logins.is_empty() || !self.unreadable.is_empty() => {}
                Err(err) => match &self.only {
                    Some(only) if !self.logins.is_empty() || !self.unreadable.is_empty() => {
                        cli_failures = only
                            .iter()
                            .filter(|user| self.login(user).is_none())
                            .map(|user| AccountOutcome::Failed {
                                account: account_id(user),
                                label: Some(user.clone()),
                                error: err.clone(),
                            })
                            .collect();
                    }
                    _ => return Err(err),
                },
            }
            if let Some(only) = &self.only {
                accounts.retain(|name| only.iter().any(|wanted| wanted.eq_ignore_ascii_case(name)));
            }
        }
        for login in &self.logins {
            if !accounts.iter().any(|known| known.eq_ignore_ascii_case(&login.username)) {
                accounts.push(login.username.clone());
            }
        }
        let mut outcomes = cli_failures;
        // An account whose saved token couldn't be read is reported, not taken for signed out.
        for username in &self.unreadable {
            accounts.retain(|known| !known.eq_ignore_ascii_case(username));
            outcomes.push(AccountOutcome::Failed {
                account: account_id(username),
                label: Some(username.clone()),
                error: ProviderError::Unexpected {
                    detail: concat!(
                        "its saved token couldn't be read from ",
                        codexbar_core::secret_store_name!()
                    ),
                },
            });
        }
        if accounts.is_empty() && outcomes.is_empty() {
            return Err(ProviderError::NotSignedIn { hint: GH_MISSING });
        }
        let mut orgs = Vec::new();
        for username in &accounts {
            match self.fetch_account(username, now) {
                Ok((snapshot, seat, token)) => {
                    let snapshot = self.add_billing(snapshot, &seat, &token, now, &mut orgs);
                    outcomes.push(AccountOutcome::Fresh(snapshot));
                }
                Err(error) => outcomes.push(AccountOutcome::Failed {
                    account: account_id(username),
                    label: Some(username.clone()),
                    error,
                }),
            }
        }
        outcomes.extend(orgs.into_iter().map(|(_, outcome)| outcome));
        Ok(outcomes)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::{CommandOutput, HttpResponse};
    use codexbar_core::Severity;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    const STATUS: &str = "github.com\n  \u{2713} Logged in to github.com account HemSoft (keyring)\n  - Active account: true\n  - Token: gho_************\n\n  \u{2713} Logged in to github.com account fhemmerrelias (keyring)\n  - Active account: false\n";
    const USER: &str = include_str!("../tests/fixtures/copilot-user.json");

    struct FakeGh {
        tokens: HashMap<&'static str, &'static str>,
    }

    impl CommandRunner for FakeGh {
        fn run(&self, _: &str, args: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
            let ok = |stdout: &str| {
                Ok(CommandOutput {
                    success: true,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
            };
            match args {
                ["auth", "status", ..] => ok(STATUS),
                ["auth", "token", "--user", user, ..] => match self.tokens.get(user) {
                    Some(token) => ok(&format!("{token}\n")),
                    None => Ok(CommandOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "no token".into(),
                    }),
                },
                _ => Err(CommandError::Failed),
            }
        }
    }

    struct FakeHttp {
        by_token: HashMap<String, HttpResponse>,
        seen: Mutex<Vec<String>>,
    }

    impl HttpClient for FakeHttp {
        fn get(&self, _: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            let auth = headers
                .iter()
                .find(|(name, _)| *name == "Authorization")
                .map(|(_, v)| v.to_string())
                .unwrap();
            self.seen.lock().unwrap().push(auth.clone());
            self.by_token.get(&auth).cloned().ok_or(ProviderError::Network)
        }
    }

    fn ok(body: &str) -> HttpResponse {
        HttpResponse::new(200, body)
    }

    #[test]
    fn parse_gh_accounts_reads_every_logged_in_account_once() {
        let doubled = format!("{STATUS}{STATUS}");
        assert_eq!(parse_gh_accounts(&doubled), ["HemSoft", "fhemmerrelias"]);
        assert_eq!(
            parse_gh_accounts("  ✓ Logged in to github.com as octocat (oauth_token)"),
            ["octocat"]
        );
        assert!(parse_gh_accounts("You are not logged into any GitHub hosts.").is_empty());
    }

    #[test]
    fn parse_user_premium_quota_maps_used_limit_and_reset() {
        let account = parse_user(USER, "HemSoft", now()).unwrap();
        assert_eq!(account.display_name(), "Copilot · HemSoft");
        let Some(Metric::Quota {
            used, limit, resets_at, ..
        }) = account.primary()
        else {
            panic!("quota")
        };
        assert_eq!((*used, *limit), (1284, 1500));
        assert_eq!(*resets_at, "2026-11-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
        // 86% used six days into the month projects exhaustion well before Nov 1.
        assert_eq!(account.assess(now()).severity(), Severity::AtRisk);
    }

    #[test]
    fn parse_user_unlimited_quota_has_no_metric() {
        let payload = r#"{"quota_reset_date":"2026-11-01","quota_snapshots":{"premium_interactions":{"entitlement":0,"remaining":0,"unlimited":true}}}"#;
        assert!(parse_user(payload, "x", now()).unwrap().metrics().is_empty());
    }

    #[test]
    fn parse_user_free_plan_reports_chat_and_completions() {
        let payload = r#"{"copilot_plan":"individual","quota_reset_date":"2026-11-01","quota_snapshots":{
            "chat":{"entitlement":200,"remaining":199,"unlimited":false},
            "completions":{"entitlement":2000,"remaining":2000,"unlimited":false},
            "premium_interactions":{"entitlement":0,"remaining":0,"unlimited":false}}}"#;
        let account = parse_user(payload, "HemSoft", now()).unwrap();
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(labels, ["Chat messages", "Code completions"]);
        assert_eq!(account.primary().unwrap().used_display(), "1");
    }

    #[test]
    fn parse_user_date_only_reset_falls_back_to_midnight_utc() {
        let payload = r#"{"quota_reset_date":"2026-11-01","quota_snapshots":{"premium_interactions":{"entitlement":300,"remaining":290}}}"#;
        let account = parse_user(payload, "x", now()).unwrap();
        assert_eq!(
            account.primary().unwrap().resets_at(),
            Some("2026-11-01T00:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn fetch_all_accounts_uses_each_accounts_token() {
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a"), ("fhemmerrelias", "tok-b")]),
        };
        let http = FakeHttp {
            by_token: HashMap::from([("token tok-a".into(), ok(USER)), ("token tok-b".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, gh);
        let accounts = provider.fetch(now()).unwrap();
        let ids: Vec<&str> = accounts.iter().map(|a| a.id().as_str()).collect();
        assert_eq!(ids, ["copilot-hemsoft", "copilot-fhemmerrelias"]);
        assert_eq!(*provider.http.seen.lock().unwrap(), ["token tok-a", "token tok-b"]);
    }

    #[test]
    fn fetch_only_selected_accounts() {
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a"), ("fhemmerrelias", "tok-b")]),
        };
        let http = FakeHttp {
            by_token: HashMap::from([("token tok-a".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, gh).only_accounts(vec!["hemsoft".into()]);
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(*provider.http.seen.lock().unwrap(), ["token tok-a"]);
    }

    #[test]
    fn fetch_one_account_failing_keeps_the_other() {
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a")]),
        };
        let http = FakeHttp {
            by_token: HashMap::from([("token tok-a".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let accounts = CopilotProvider::new(http, gh).fetch(now()).unwrap();
        assert_eq!(accounts.len(), 1);
    }

    #[test]
    fn fetch_gh_missing_returns_install_hint() {
        struct NoGh;
        impl CommandRunner for NoGh {
            fn run(&self, _: &str, _: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
                Err(CommandError::NotFound)
            }
        }
        let http = FakeHttp {
            by_token: HashMap::new(),
            seen: Mutex::default(),
        };
        let err = CopilotProvider::new(http, NoGh).fetch(now()).unwrap_err();
        assert_eq!(err, ProviderError::NotSignedIn { hint: GH_MISSING });
    }

    /// Answers by URL: the first route whose fragment the URL contains, else 404. Records every URL and token.
    struct Routes {
        routes: Vec<(&'static str, HttpResponse)>,
        seen: Mutex<Vec<(String, String)>>,
    }

    impl Routes {
        fn new(routes: Vec<(&'static str, HttpResponse)>) -> Self {
            Self {
                routes,
                seen: Mutex::default(),
            }
        }

        fn urls(&self, fragment: &str) -> usize {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|(url, _)| url.contains(fragment))
                .count()
        }
    }

    impl HttpClient for Routes {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            let auth = headers
                .iter()
                .find(|(name, _)| *name == "Authorization")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
            self.seen.lock().unwrap().push((url.to_owned(), auth.clone()));
            // A route is a URL fragment, or "fragment|Authorization header" to answer one token only.
            let matches = |route: &str| match route.split_once('|') {
                Some((fragment, token)) => url.contains(fragment) && auth == token,
                None => url.contains(route),
            };
            Ok(self
                .routes
                .iter()
                .find(|(route, _)| matches(route))
                .map_or_else(|| HttpResponse::new(404, ""), |(_, response)| response.clone()))
        }
    }

    struct NoGh;

    impl CommandRunner for NoGh {
        fn run(&self, _: &str, _: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
            Err(CommandError::NotFound)
        }
    }

    const ENTERPRISE_USER: &str = r#"{"login":"dev","copilot_plan":"enterprise","organization_login_list":["Acme-Eng","other"],
        "quota_reset_date_utc":"2026-11-01T00:00:00.000Z","quota_snapshots":{
        "premium_interactions":{"entitlement":1000,"remaining":400,"unlimited":false}}}"#;

    fn billing(pool_total: Option<u64>) -> OrgBilling {
        OrgBilling {
            enterprise: "acme".to_owned(),
            organization: "acme-eng".to_owned(),
            pool_total,
        }
    }

    fn fresh(outcomes: &[AccountOutcome]) -> Vec<&AccountSnapshot> {
        outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                AccountOutcome::Fresh(account) => Some(account),
                AccountOutcome::Failed { .. } => None,
            })
            .collect()
    }

    #[test]
    fn an_account_codexbar_signed_in_uses_its_own_token_without_the_cli() {
        let http = FakeHttp {
            by_token: HashMap::from([("token gho_own".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("HemSoft", "gho_own");
        let accounts = provider.fetch(now()).unwrap();
        assert_eq!(accounts[0].id().as_str(), "copilot-hemsoft");
        assert!(
            *provider.http.seen.lock().unwrap() == ["token gho_own"],
            "only the stored token is used"
        );
    }

    #[test]
    fn signed_in_accounts_join_the_cli_accounts_once_and_keep_their_token() {
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a"), ("fhemmerrelias", "tok-b")]),
        };
        let http = FakeHttp {
            by_token: HashMap::from([
                ("token tok-a".into(), ok(USER)),
                ("token gho_own".into(), ok(USER)),
                ("token gho_new".into(), ok(USER)),
            ]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, gh)
            .with_login("FHEMMERRELIAS", "gho_own")
            .with_login("newcomer", "gho_new");
        let ids: Vec<String> = provider
            .fetch(now())
            .unwrap()
            .iter()
            .map(|a| a.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["copilot-hemsoft", "copilot-fhemmerrelias", "copilot-newcomer"]);
        let seen = provider.http.seen.lock().unwrap().clone();
        assert!(
            seen == ["token tok-a", "token gho_own", "token gho_new"],
            "CodexBar's token wins for its account"
        );
    }

    #[test]
    fn signed_in_accounts_still_fetch_without_the_github_cli() {
        let http = FakeHttp {
            by_token: HashMap::from([("token gho_own".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let accounts = CopilotProvider::new(http, NoGh)
            .with_login("HemSoft", "gho_own")
            .fetch(now())
            .unwrap();
        assert_eq!(accounts.len(), 1);
    }

    #[test]
    fn a_refused_signed_in_account_says_to_sign_in_from_settings() {
        let http = FakeHttp {
            by_token: HashMap::from([("token gho_own".into(), HttpResponse::new(401, ""))]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("HemSoft", "gho_own");
        assert_eq!(
            provider.fetch(now()),
            Err(ProviderError::Expired { hint: MANAGED_LOGIN })
        );
    }

    fn credits(total: f64) -> HttpResponse {
        ok(&format!(
            r#"{{"usageItems":[{{"grossQuantity":{total},"unitType":"ai-credits"}}]}}"#
        ))
    }

    fn day_usage(credits: f64) -> HttpResponse {
        ok(&format!(
            r#"{{"usageItems":[{{"grossQuantity":{credits}}},{{"grossQuantity":0.5}}]}}"#
        ))
    }

    #[test]
    fn an_enterprise_seat_shows_its_ai_credits_and_its_organization() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            (
                "ai_credit/usage?year=2026&month=10&organization=acme-eng",
                credits(7000.0),
            ),
            (
                "/orgs/acme-eng/copilot/billing",
                ok(r#"{"seat_breakdown":{"total":4}}"#),
            ),
            ("ai_credit/usage?year=2026&month=10&user=dev", credits(700.0)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("DEV", billing(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let fresh = fresh(&outcomes);
        let ids: Vec<&str> = fresh.iter().map(|a| a.id().as_str()).collect();
        assert_eq!(ids, ["copilot-dev", "copilotorg-acme-eng"]);
        // The user: 700 credits of the 3,900-credit allowance per seat, then the premium quota, and the share of
        // the org.
        let user = fresh[0];
        let Some(Metric::Quota { label, used, limit, .. }) = user.primary() else {
            panic!("credits")
        };
        assert_eq!((label.as_str(), *used, *limit), ("AI credits", 700, 3900));
        assert_eq!(user.metrics().len(), 2);
        assert!(
            user.messages()
                .iter()
                .any(|m| m == "10% of acme-eng's AI credits this month."),
            "{:?}",
            user.messages()
        );
        // The organization: 7,000 of 4 seats x 3,900 credits.
        let Some(Metric::Quota { used, limit, .. }) = fresh[1].primary() else {
            panic!("pool")
        };
        assert_eq!((*used, *limit), (7000, 15_600));
        assert_eq!(fresh[1].label(), Some("acme-eng"));
        assert_eq!(provider.http.urls("day="), 0, "the month's report is enough");
        let seen = provider.http.seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .filter(|(url, _)| url.contains("api.github.com/enterprises"))
                .all(|(_, auth)| auth == "Bearer gho_own"),
            "billing uses the account's own token"
        );
    }

    #[test]
    fn a_configured_pool_total_replaces_the_seat_count() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng", credits(500.0)),
            ("user=dev", credits(10.0)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(Some(2000)));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = &outcomes[1] else {
            panic!("org")
        };
        let Some(Metric::Quota { used, limit, .. }) = org.primary() else {
            panic!("pool")
        };
        assert_eq!((*used, *limit), (500, 2000));
        assert_eq!(provider.http.urls("/copilot/billing"), 0);
    }

    #[test]
    fn org_billing_without_a_pool_shows_the_credits_used() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng", credits(1234.0)),
            ("/copilot/billing", HttpResponse::new(403, "")),
            ("user=dev", credits(10.0)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = &outcomes[1] else {
            panic!("org")
        };
        assert!(org.metrics().is_empty());
        assert!(
            org.messages()
                .iter()
                .any(|m| m.starts_with("1234 AI credits used this month."))
        );
    }

    #[test]
    fn org_billing_failures_are_reported_and_keep_the_seat() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("ai_credit/usage", HttpResponse::new(403, "")),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert_eq!(user.primary().map(Metric::label), Some("Premium requests"));
        assert!(user.messages().iter().any(|m| m.contains("couldn't be read")));
        let AccountOutcome::Failed { account, error, .. } = &outcomes[1] else {
            panic!("org failure")
        };
        assert_eq!(account.as_str(), "copilotorg-acme-eng");
        assert_eq!(error, &ProviderError::Http { status: 403 });
    }

    #[test]
    fn an_empty_month_report_for_a_user_is_summed_day_by_day() {
        // Seven days into October; the month-wide user report comes back empty.
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng", credits(7000.0)),
            ("day=", day_usage(99.5)),
            ("user=dev", ok(r#"{"usageItems":[]}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(Some(10_000)));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        let Some(Metric::Quota { used, .. }) = user.primary() else {
            panic!("credits")
        };
        assert_eq!(*used, 700);
        assert_eq!(provider.http.urls("day="), 7);
    }

    #[test]
    fn a_day_that_cant_be_read_leaves_the_month_unknown_not_low() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng", credits(7000.0)),
            ("day=3&", HttpResponse::new(500, "")),
            ("day=", day_usage(99.5)),
            ("user=dev", ok(r#"{"usageItems":[]}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(Some(10_000)));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert_eq!(user.primary().map(Metric::label), Some("Premium requests"));
        assert!(user.messages().iter().any(|m| m.contains("couldn't be read")));
    }

    #[test]
    fn another_member_can_read_the_organization_when_the_first_cannot() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng|Bearer gho_dev", HttpResponse::new(403, "")),
            ("organization=acme-eng|Bearer gho_admin", credits(4000.0)),
            ("ai_credit/usage", credits(10.0)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_dev")
            .with_login("admin", "gho_admin")
            .with_billing("dev", billing(Some(10_000)))
            .with_billing("admin", billing(Some(10_000)));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert_eq!(outcomes.len(), 3, "two users and one organization");
        let AccountOutcome::Fresh(org) = &outcomes[2] else {
            panic!("the second member's token read the organization")
        };
        assert_eq!(org.id().as_str(), "copilotorg-acme-eng");
        let Some(Metric::Quota { used, .. }) = org.primary() else {
            panic!("credits")
        };
        assert_eq!(*used, 4000);
    }

    #[test]
    fn cli_accounts_come_from_the_json_status_even_when_one_has_a_problem() {
        let json = r#"{"hosts":{"github.com":[
            {"active":true,"host":"github.com","login":"HemSoft","state":"success"},
            {"active":false,"host":"github.com","login":"expired-user","state":"error"},
            {"active":false,"host":"github.com","login":"hemsoft","state":"success"}]}}"#;
        let expected = Some(vec!["HemSoft".to_owned(), "expired-user".to_owned()]);
        assert_eq!(parse_gh_accounts_json(json), expected);
        assert_eq!(parse_gh_accounts_json(&format!("{json}\n")), expected);
        assert_eq!(parse_gh_accounts_json(STATUS), None, "plain status output isn't JSON");
        assert_eq!(parse_gh_accounts_json(r#"{"hosts":{}}"#), None);
    }

    #[test]
    fn a_github_cli_failure_keeps_its_configured_accounts_as_failures() {
        struct BrokenGh;
        impl CommandRunner for BrokenGh {
            fn run(&self, _: &str, _: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
                Err(CommandError::TimedOut)
            }
        }
        let http = FakeHttp {
            by_token: HashMap::from([("token gho_own".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        // gh times out: the named gh account fails (keeping its last usage) and CodexBar's own still fetches.
        let provider = CopilotProvider::new(http, BrokenGh)
            .only_accounts(vec!["cli-user".into()])
            .with_login("HemSoft", "gho_own");
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let failed: Vec<&str> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                AccountOutcome::Failed { account, .. } => Some(account.as_str()),
                AccountOutcome::Fresh(_) => None,
            })
            .collect();
        assert_eq!(failed, ["copilot-cli-user"]);
        assert_eq!(fresh(&outcomes).len(), 1);
        // With every gh account wanted, they can't be named: the provider fails as a whole, so nothing is dropped.
        let http = FakeHttp {
            by_token: HashMap::from([("token gho_own".into(), ok(USER))]),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, BrokenGh).with_login("HemSoft", "gho_own");
        assert_eq!(provider.fetch_outcomes(now()).err(), Some(ProviderError::Network));
    }

    #[test]
    fn organization_ids_never_collide_with_user_ids() {
        assert_ne!(org_account_id("acme").as_str(), account_id("org-acme").as_str());
        assert_eq!(org_account_id("Acme-Eng").as_str(), "copilotorg-acme-eng");
    }

    #[test]
    fn an_unreadable_saved_token_is_reported_not_taken_for_signed_out() {
        let http = FakeHttp {
            by_token: HashMap::new(),
            seen: Mutex::default(),
        };
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_unreadable_login("HemSoft");
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { account, error, .. } = &outcomes[0] else {
            panic!("failure")
        };
        assert_eq!(account.as_str(), "copilot-hemsoft");
        assert!(
            matches!(error, ProviderError::Unexpected { detail } if detail.contains(codexbar_core::secret_store_name!()))
        );
        assert!(provider.http.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn org_billing_needs_an_enterprise_seat_in_that_organization() {
        let http = Routes::new(vec![("copilot_internal/user", ok(USER))]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("HemSoft", "gho_own")
            .with_billing("hemsoft", billing(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert_eq!(outcomes.len(), 1);
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert!(user.messages().iter().any(|m| m.contains("isn't an Enterprise seat")));
        assert_eq!(provider.http.urls("/enterprises/"), 0);
    }

    #[test]
    fn the_allowance_per_seat_follows_the_month() {
        assert_eq!(credits_per_seat(2026, 5), 3900);
        assert_eq!(credits_per_seat(2026, 6), 7000);
        assert_eq!(credits_per_seat(2026, 8), 7000);
        assert_eq!(credits_per_seat(2026, 9), 3900);
        assert_eq!(credits_per_seat(2027, 7), 3900);
    }

    #[test]
    fn fetch_unauthorized_for_every_account_returns_expired() {
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a"), ("fhemmerrelias", "tok-b")]),
        };
        let denied = HttpResponse::new(401, String::new());
        let http = FakeHttp {
            by_token: HashMap::from([("token tok-a".into(), denied.clone()), ("token tok-b".into(), denied)]),
            seen: Mutex::default(),
        };
        assert!(matches!(
            CopilotProvider::new(http, gh).fetch(now()),
            Err(ProviderError::Expired { .. })
        ));
    }
}
