//! GitHub Copilot premium-request quotas for every account signed in to the GitHub CLI, and for accounts CodexBar
//! signed in itself (#79). An account on a Copilot Enterprise seat can also show its organization's AI credits this
//! month and its own share, from the enterprise billing API (configured per account). An organization billed on its
//! own (Copilot Business, no enterprise) is read from the organization's billing API instead, by an owner or billing
//! manager with or without a seat there (#233).

use std::sync::Mutex;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Datelike as _, Months, NaiveDate, TimeZone as _, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Metric, Provider};
use serde_json::Value;

use crate::AccountOutcome;
use crate::github_allowances::{Budget, parse_budgets};
use crate::pace::elapsed_pace;
use crate::{CommandError, CommandRunner, HttpClient, ProviderError, UsageProvider};

const USER_ENDPOINT: &str = "https://api.github.com/copilot_internal/user";
const REST_API: &str = "https://api.github.com";
const REST_API_VERSION: &str = "2026-03-10";
const USER_AGENT: &str = "CodexBar";
/// AI credits per seat each month by plan; June to August 2026 had a promotional allowance.
const CREDITS_PER_SEAT: u64 = 3900;
const PROMOTIONAL_CREDITS_PER_SEAT: u64 = 7000;
const BUSINESS_CREDITS_PER_SEAT: u64 = 1900;
const PROMOTIONAL_BUSINESS_CREDITS_PER_SEAT: u64 = 3000;
/// How long to wait when GitHub limits the rate without saying for how long.
const RATE_LIMIT_WAIT: i64 = 60;
const NO_SEAT: &str = "no Copilot subscription on this account";
const ORG_NO_ACCESS: &str = "This account can't read the organization's Copilot billing. It needs to be an owner or \
                             billing manager of the organization.";
/// The most budget pages read: 10,000 budgets.
const MAX_BUDGET_PAGES: u32 = 100;
const BUDGETS_UNREAD: &str = "Its Copilot budgets couldn't be read.";
const ORG_NOT_FOUND: &str = "GitHub has no such organization, or this account can't see its billing. Check the \
                             organization's name in Settings > Accounts, and that this account is an owner or billing \
                             manager there.";
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

    let mut account = AccountSnapshot::new(account_id(username), Provider::Copilot, metrics, now).with_label(username);
    if let Some(plan) = json
        .get("copilot_plan")
        .and_then(Value::as_str)
        .filter(|plan| !plan.trim().is_empty())
    {
        account = account.with_plan(crate::codex::plan_name(plan));
    }
    Ok(account)
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
        length: (start < resets_at).then(|| resets_at - start),
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

/// Where an account reads its organization's AI credits: the enterprise and organization slugs, and optionally the pool
/// size (otherwise seats times the monthly allowance per seat). Without an enterprise the organization is billed on its
/// own (Copilot Business) and read from its own billing API (#233).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgBilling {
    pub enterprise: Option<String>,
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

/// When GitHub limited the rate of a token on organization billing, by a hash of the token (never the token itself):
/// no organization billing request is made with it before then, as GitHub asks. The provider is rebuilt on every
/// refresh, so this outlives it, as Claude's backoff does.
static BACKOFF: Mutex<Vec<(u64, DateTime<Utc>)>> = Mutex::new(Vec::new());

fn token_key(token: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// The retry time still ahead for `key`, if GitHub limited its rate.
fn backed_off(key: u64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut backoff = BACKOFF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    backoff.retain(|(_, until)| *until > now);
    backoff.iter().find(|(known, _)| *known == key).map(|(_, until)| *until)
}

/// Remembers `retry_at` for `key` and returns the rate-limit error that says it.
fn back_off(key: u64, retry_at: DateTime<Utc>) -> ProviderError {
    let mut backoff = BACKOFF.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    backoff.retain(|(known, _)| *known != key);
    backoff.push((key, retry_at));
    ProviderError::RateLimited { retry_at }
}

/// When GitHub said to try again, if `response` says its rate is limited: a 429, or a 403 with a retry time or a
/// rate-limit message. GitHub's whole wait: its Retry-After (a year at most, so the time stays representable), else
/// the reset of a used-up primary limit, else a minute.
fn rate_limited_until(response: &crate::HttpResponse, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let reset = response.rate_limit_reset.filter(|reset| *reset > now);
    let limited = match response.status {
        429 => true,
        403 => {
            response.retry_after_secs.is_some()
                || reset.is_some()
                || response.body.to_lowercase().contains("rate limit")
        }
        _ => false,
    };
    limited.then(|| match (response.retry_after_secs, reset) {
        (Some(secs), _) => now + chrono::Duration::seconds(secs.min(365 * 24 * 3600) as i64),
        (None, Some(reset)) => reset,
        (None, None) => now + chrono::Duration::seconds(RATE_LIMIT_WAIT),
    })
}

/// The organization's dashboard account. Users are `copilot-<user>`; organizations use another prefix, so a user
/// named like `org-acme` never shares an id with the organization `acme`.
pub fn org_account_id(organization: &str) -> AccountId {
    AccountId::new(format!("copilotorg-{}", organization.to_lowercase()))
}

/// AI credits each Copilot Enterprise seat gets in this month.
pub fn credits_per_seat(year: i32, month: u32) -> u64 {
    if promotional(year, month) {
        PROMOTIONAL_CREDITS_PER_SEAT
    } else {
        CREDITS_PER_SEAT
    }
}

/// AI credits each Copilot Business seat gets in this month.
pub fn business_credits_per_seat(year: i32, month: u32) -> u64 {
    if promotional(year, month) {
        PROMOTIONAL_BUSINESS_CREDITS_PER_SEAT
    } else {
        BUSINESS_CREDITS_PER_SEAT
    }
}

/// The first three months of usage-based billing, June to August 2026, came with more AI credits per seat.
fn promotional(year: i32, month: u32) -> bool {
    year == 2026 && (6..=8).contains(&month)
}

/// The allowance per seat for a Copilot plan as GitHub names it (`business`, `enterprise`).
fn plan_credits_per_seat(plan: &str, now: DateTime<Utc>) -> u64 {
    if plan.eq_ignore_ascii_case("business") {
        business_credits_per_seat(now.year(), now.month())
    } else {
        credits_per_seat(now.year(), now.month())
    }
}

/// What an AI-credit report counts: the credits used and what they cost past the included pool, in dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Credits {
    used: f64,
    net: f64,
}

/// Whether `budget` caps the organization's Copilot AI credits: an organization budget on all AI credits
/// (`BundlePricing`), on the Copilot product, or on a Copilot SKU.
fn caps_copilot(budget: &Budget, organization: &str) -> bool {
    let own = budget.scope.eq_ignore_ascii_case("organization")
        && (budget.entity.is_empty() || budget.entity.eq_ignore_ascii_case(organization));
    let targets = |wanted: &dyn Fn(&str) -> bool| budget.targets.iter().any(|t| wanted(&t.to_lowercase()));
    let copilot = match budget.kind.as_str() {
        kind if kind.eq_ignore_ascii_case("BundlePricing") => {
            budget.targets.is_empty() || targets(&|t| t == "ai_credits" || t.starts_with("copilot"))
        }
        kind if kind.eq_ignore_ascii_case("ProductPricing") => targets(&|t| t == "copilot"),
        // GitHub's Copilot AI-credit SKUs: `copilot_ai_credit` and the cloud agent's `coding_agent_ai_credit`. Its
        // license SKUs (`copilot_for_business`, `copilot_enterprise`, `copilot_standalone`) are budgeted in seats,
        // not dollars, and aren't AI credits, so they stay off this card.
        kind if kind.eq_ignore_ascii_case("SkuPricing") => {
            targets(&|t| t == "coding_agent_ai_credit" || (t.starts_with("copilot") && t.contains("ai_credit")))
        }
        _ => false,
    };
    own && copilot
}

/// Which usage a Copilot budget caps, so it is measured against that usage and no other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BudgetCovers {
    /// Every AI-credit SKU, whatever the product (`BundlePricing`).
    AllAiCredits,
    /// The Copilot product (`ProductPricing`), as the Copilot report counts it.
    Copilot,
    /// One Copilot SKU (`SkuPricing`), which the reports don't name the same way: shown, never measured.
    Sku,
}

fn budget_covers(budget: &Budget) -> BudgetCovers {
    if budget.kind.eq_ignore_ascii_case("BundlePricing") {
        BudgetCovers::AllAiCredits
    } else if budget.kind.eq_ignore_ascii_case("ProductPricing") {
        BudgetCovers::Copilot
    } else {
        BudgetCovers::Sku
    }
}

/// What the tokens configured for one organization have read of it: its credits, its pool and its Copilot budgets,
/// each from whichever token could read it.
struct OrgRead {
    billing: OrgBilling,
    /// The month's credits, or the error the tokens met when none could read them.
    credits: Result<Credits, ProviderError>,
    pool: Option<u64>,
    plan: Option<String>,
    /// The Copilot budgets; `None` until a token reads them. An organization read through its enterprise has none.
    budgets: Option<Vec<Budget>>,
    /// Every product's net AI-credit spend, which a budget on every AI credit is measured against; `None` until a
    /// token reads it, kept apart so a later token can fill it in.
    all_net: Option<f64>,
}

impl OrgRead {
    /// The organization's account. An organization billed on its own also shows its Copilot budgets; ones that
    /// couldn't be read never hide the credits.
    fn outcome(&self, now: DateTime<Utc>) -> AccountOutcome {
        let id = org_account_id(&self.billing.organization);
        let consumed = match &self.credits {
            Ok(consumed) => consumed,
            Err(error) => {
                return AccountOutcome::Failed {
                    account: id,
                    label: Some(self.billing.organization.clone()),
                    error: error.clone(),
                };
            }
        };
        let mut org = match self.pool {
            Some(pool) => AccountSnapshot::new(
                id,
                Provider::Copilot,
                vec![credits_metric("AI credits", consumed.used, pool, now)],
                now,
            ),
            None => AccountSnapshot::new(id, Provider::Copilot, Vec::new(), now).with_message(format!(
                "{:.0} AI credits used this month. Set the pool total in Settings to see how much is left.",
                consumed.used
            )),
        };
        if self.billing.enterprise.is_none() {
            match &self.budgets {
                Some(budgets) => {
                    for budget in budgets {
                        let spent = match budget_covers(budget) {
                            BudgetCovers::AllAiCredits => self.all_net,
                            BudgetCovers::Copilot => Some(consumed.net),
                            BudgetCovers::Sku => None,
                        };
                        org = org.with_message(budget_line(budget, spent));
                    }
                }
                None => org = org.with_message(BUDGETS_UNREAD),
            }
        }
        if let Some(plan) = &self.plan {
            org = org.with_plan(plan);
        }
        AccountOutcome::Fresh(org.with_label(&self.billing.organization))
    }
}

/// One line per Copilot budget: what it caps, how much of that this month's usage has spent when it can be measured,
/// and whether it stops usage.
fn budget_line(budget: &Budget, spent: Option<f64>) -> String {
    let dollars = |amount: f64| format!("${amount:.2}");
    let name = match budget_covers(budget) {
        BudgetCovers::AllAiCredits => "AI credits budget".to_owned(),
        BudgetCovers::Copilot => "Copilot budget".to_owned(),
        BudgetCovers::Sku => format!("Budget on {}", budget.targets.join(", ")),
    };
    // GitHub: "Any budget set to $0 USD stops usage immediately for the users it applies to."
    let zero_stop = budget.stops_usage && budget.amount <= 0.0;
    let Some(spent) = spent else {
        let state = if zero_stop {
            "usage is stopped: a $0 budget stops it at once"
        } else if budget.stops_usage {
            "stops usage at the limit"
        } else {
            "doesn't stop usage"
        };
        return format!(
            "{name}: {}, {state}. Its spend isn't measured here.",
            dollars(budget.amount)
        );
    };
    // A budget counts spend only from when it was created, which GitHub doesn't say, so the month's spend can't tell
    // whether one is reached: its own setting is shown. Only a $0 budget is known to stop usage, at once.
    let state = match (budget.stops_usage, zero_stop) {
        (true, true) => "usage is stopped: a $0 budget stops it at once",
        (true, false) => "stops usage at the limit",
        (false, _) => "doesn't stop usage",
    };
    let earlier = if budget.amount > 0.0 && spent >= budget.amount {
        " Spend from before the budget was created doesn't count toward it."
    } else {
        ""
    };
    format!(
        "{name}: {} of {} spent this month, {state}.{earlier}",
        dollars(spent.max(0.0)),
        dollars(budget.amount)
    )
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
        length: (start < end).then(|| end - start),
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
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<(AccountSnapshot, Seat), ProviderError> {
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
        // A token GitHub has rate-limited waits for its retry time here too, and a limit met here holds back the
        // organization's billing requests with the same token.
        let key = token_key(token);
        if let Some(retry_at) = backed_off(key, now) {
            return Err(ProviderError::RateLimited { retry_at });
        }
        let response = self.http.get(USER_ENDPOINT, &headers)?;
        if let Some(retry_at) = rate_limited_until(&response, now) {
            return Err(back_off(key, retry_at));
        }
        match response.status {
            200..=299 => parse_user(&response.body, username, now).map(|account| (account, parse_seat(&response.body))),
            401 | 403 => Err(ProviderError::Expired { hint: expired }),
            // 404: this account has no Copilot seat.
            404 => Err(ProviderError::Unexpected { detail: NO_SEAT }),
            status => Err(ProviderError::Http { status }),
        }
    }

    /// A GitHub REST API GET with `token`.
    fn get(&self, path: &str, token: &str) -> Result<crate::HttpResponse, ProviderError> {
        let url = format!("{REST_API}{path}");
        let bearer = format!("Bearer {token}");
        let headers = [
            ("Authorization", bearer.as_str()),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", REST_API_VERSION),
            ("User-Agent", USER_AGENT),
        ];
        self.http.get(&url, &headers)
    }

    /// A GitHub REST API GET with `token`; the body on success.
    fn rest(&self, path: &str, token: &str) -> Result<String, ProviderError> {
        let response = self.get(path, token)?;
        match response.status {
            200..=299 => Ok(response.body),
            status => Err(ProviderError::Http { status }),
        }
    }

    /// Like `rest`, for an organization's own billing API (#233): a missing permission, a rate limit and an unknown
    /// organization each say so.
    /// Once GitHub limits the rate, no request goes out with that token before its retry time, even from the next
    /// refresh's provider.
    fn org_rest(&self, path: &str, token: &str, now: DateTime<Utc>) -> Result<String, ProviderError> {
        let key = token_key(token);
        if let Some(retry_at) = backed_off(key, now) {
            return Err(ProviderError::RateLimited { retry_at });
        }
        let response = self.get(path, token)?;
        if let Some(retry_at) = rate_limited_until(&response, now) {
            return Err(back_off(key, retry_at));
        }
        match response.status {
            200..=299 => Ok(response.body),
            403 => Err(ProviderError::Refused { reason: ORG_NO_ACCESS }),
            404 => Err(ProviderError::Refused { reason: ORG_NOT_FOUND }),
            status => Err(ProviderError::Http { status }),
        }
    }

    /// AI credits this month on `day` or for the whole month. An enterprise's report is narrowed by `filter`
    /// (`organization=…` or `user=…`); an organization billed on its own has a report of its own, read for Copilot as
    /// codexbar-ios does, and `filter` doesn't apply to it.
    fn ai_credits(
        &self,
        billing: &OrgBilling,
        filter: &str,
        day: Option<u32>,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Credits>, ProviderError> {
        let day = day.map(|day| format!("&day={day}")).unwrap_or_default();
        let (year, month) = (now.year(), now.month());
        let body = match &billing.enterprise {
            Some(enterprise) => self.rest(
                &format!(
                    "/enterprises/{}/settings/billing/ai_credit/usage?year={year}&month={month}{day}&{filter}",
                    encode(enterprise),
                ),
                token,
            )?,
            None => self.org_rest(
                &format!(
                    "/organizations/{}/settings/billing/ai_credit/usage?year={year}&month={month}{day}&product=Copilot",
                    encode(&billing.organization),
                ),
                token,
                now,
            )?,
        };
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
        let sum = |key: &str| {
            items
                .iter()
                .filter_map(|item| item.get(key).and_then(Value::as_f64))
                .sum()
        };
        Ok(Some(Credits {
            used: sum("grossQuantity"),
            net: sum("netAmount"),
        }))
    }

    /// The organization's AI credits this month; a month with no usage yet is zero.
    fn org_consumed(&self, billing: &OrgBilling, token: &str, now: DateTime<Utc>) -> Result<Credits, ProviderError> {
        let filter = format!("organization={}", encode(&billing.organization));
        Ok(self.ai_credits(billing, &filter, None, token, now)?.unwrap_or_default())
    }

    /// The organization's monthly pool: the configured total, else seats times the allowance per seat. An enterprise's
    /// seats are Enterprise seats; an organization billed on its own says which plan its seats are on.
    fn pool(&self, billing: &OrgBilling, token: &str, now: DateTime<Utc>) -> (Option<u64>, Option<String>) {
        let configured = billing.pool_total.filter(|total| *total > 0);
        // The same authenticated billing source supplies the seat count and explicit plan, even when the
        // configured pool overrides its count. A refused optional lookup never discards the configured total.
        let value = self
            .org_rest(
                &format!("/orgs/{}/copilot/billing", encode(&billing.organization)),
                token,
                now,
            )
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok());
        let Some(json) = value else {
            return (configured, None);
        };
        let plan = json
            .get("plan_type")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(crate::codex::plan_name);
        let computed = json
            .pointer("/seat_breakdown/total")
            .and_then(Value::as_u64)
            .filter(|seats| *seats > 0)
            .and_then(|seats| {
                let per_seat = match &billing.enterprise {
                    Some(_) => credits_per_seat(now.year(), now.month()),
                    None => {
                        plan_credits_per_seat(json.get("plan_type").and_then(Value::as_str).unwrap_or("business"), now)
                    }
                };
                seats.checked_mul(per_seat)
            });
        (configured.or(computed), plan)
    }

    /// What an organization billed on its own spent this month on every AI credit past its pool, whatever the product:
    /// its AI-credit report without the product filter.
    fn all_ai_credits_net(&self, billing: &OrgBilling, token: &str, now: DateTime<Utc>) -> Result<f64, ProviderError> {
        let body = self.org_rest(
            &format!(
                "/organizations/{}/settings/billing/ai_credit/usage?year={}&month={}",
                encode(&billing.organization),
                now.year(),
                now.month()
            ),
            token,
            now,
        )?;
        let json: Value = serde_json::from_str(&body).map_err(|_| ProviderError::Unexpected {
            detail: "unreadable AI-credit report",
        })?;
        let items = json
            .get("usageItems")
            .and_then(Value::as_array)
            .ok_or(ProviderError::Unexpected {
                detail: "unreadable AI-credit report",
            })?;
        Ok(items
            .iter()
            .filter_map(|item| item.get("netAmount").and_then(Value::as_f64))
            .sum())
    }

    /// The Copilot budgets of an organization billed on its own, from every page of its budget list.
    fn copilot_budgets(
        &self,
        billing: &OrgBilling,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<Budget>, ProviderError> {
        let unreadable = ProviderError::Unexpected {
            detail: "unreadable budget list",
        };
        let mut all = Vec::new();
        // Every page: the list holds user and repository budgets too, so an organization's own can come late. The cap
        // only stops a list that never ends.
        for page in 1..=MAX_BUDGET_PAGES {
            let body = self.org_rest(
                &format!(
                    "/organizations/{}/settings/billing/budgets?per_page=100&page={page}",
                    encode(&billing.organization)
                ),
                token,
                now,
            )?;
            let (budgets, more) = parse_budgets(&body).map_err(|_| unreadable.clone())?;
            all.extend(budgets);
            if !more {
                all.retain(|budget| caps_copilot(budget, &billing.organization));
                return Ok(all);
            }
        }
        Err(unreadable)
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
            return Ok(month.used);
        }
        let mut total = 0.0;
        for day in 1..=now.day() {
            total += self
                .ai_credits(billing, &filter, Some(day), token, now)?
                .map_or(0.0, |credits| credits.used);
        }
        Ok(total)
    }

    /// Every organization billed on its own that `username` is set to read: one GitHub account can be an owner or
    /// billing manager of several (#233).
    fn org_only_billing(&self, username: &str) -> Vec<&OrgBilling> {
        self.billing
            .iter()
            .filter(|(user, billing)| user.eq_ignore_ascii_case(username) && billing.enterprise.is_none())
            .map(|(_, billing)| billing)
            .collect()
    }

    /// The enterprise billing set for `username`: its seat's own share, so one at most.
    fn enterprise_billing(&self, username: &str) -> Option<&OrgBilling> {
        self.billing
            .iter()
            .find(|(user, billing)| user.eq_ignore_ascii_case(username) && billing.enterprise.is_some())
            .map(|(_, billing)| billing)
    }

    /// A configured organization whose token couldn't be had keeps its card as a failure, with its last good usage,
    /// rather than dropping off the dashboard.
    fn org_unreadable(&self, username: &str, error: &ProviderError, orgs: &mut Vec<OrgRead>) {
        for billing in self.org_only_billing(username) {
            if !orgs
                .iter()
                .any(|org| org.billing.organization.eq_ignore_ascii_case(&billing.organization))
            {
                orgs.push(OrgRead {
                    billing: billing.clone(),
                    credits: Err(error.clone()),
                    pool: None,
                    plan: None,
                    budgets: None,
                    all_net: None,
                });
            }
        }
    }

    /// Reads the organization's AI credits with `token` and keeps what it reads in `orgs`, one per organization. Each
    /// part comes from whichever configured token can read it: a later token fills in the credits after a failure, the
    /// pool (the seat count is for owners only) and the budgets (billing managers can read them) that an earlier one
    /// couldn't, so the card never depends on the order the accounts are configured in.
    fn add_org(
        &self,
        billing: &OrgBilling,
        token: &str,
        now: DateTime<Utc>,
        orgs: &mut Vec<OrgRead>,
    ) -> Result<Credits, ProviderError> {
        let consumed = self.org_consumed(billing, token, now);
        let index = match orgs
            .iter()
            .position(|org| org.billing.organization.eq_ignore_ascii_case(&billing.organization))
        {
            Some(index) => index,
            None => {
                orgs.push(OrgRead {
                    billing: billing.clone(),
                    credits: consumed.clone(),
                    pool: None,
                    plan: None,
                    budgets: None,
                    all_net: None,
                });
                orgs.len() - 1
            }
        };
        let org = &mut orgs[index];
        if let Ok(credits) = &consumed {
            if org.credits.is_err() {
                org.credits = Ok(*credits);
            }
            if org.pool.is_none() || org.plan.is_none() {
                let (pool, plan) = self.pool(billing, token, now);
                org.pool = org.pool.or(pool);
                org.plan = org.plan.take().or(plan);
            }
            if org.budgets.is_none() && billing.enterprise.is_none() {
                org.budgets = self.copilot_budgets(billing, token, now).ok();
            }
            // A budget on every AI credit is measured against every product's spend, read only when there is one.
            let bundle = org.budgets.as_ref().is_some_and(|budgets| {
                budgets
                    .iter()
                    .any(|budget| budget_covers(budget) == BudgetCovers::AllAiCredits)
            });
            if bundle && org.all_net.is_none() {
                org.all_net = self.all_ai_credits_net(billing, token, now).ok();
            }
        }
        consumed
    }

    /// Adds org billing to a fetched account. An organization billed on its own (#233) is read by its owners and
    /// billing managers whatever their seat, and the account itself is left as it is. Through an enterprise, the
    /// account must be on an Enterprise seat in the organization: its AI credits become the first metric and its share
    /// of the organization's a note.
    fn add_billing(
        &self,
        account: AccountSnapshot,
        seat: &Seat,
        token: &str,
        now: DateTime<Utc>,
        orgs: &mut Vec<OrgRead>,
    ) -> AccountSnapshot {
        let username = account.label().unwrap_or_default().to_owned();
        for billing in self.org_only_billing(&username) {
            let _ = self.add_org(billing, token, now, orgs);
        }
        let Some(billing) = self.enterprise_billing(&username) else {
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
        let org_consumed = self.add_org(billing, token, now, orgs);
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
        if let Some(plan) = account.plan() {
            billed = billed.with_plan(plan);
        }
        for message in account.messages() {
            billed = billed.with_message(message.clone());
        }
        if let Some(org) = org_consumed.ok().map(|org| org.used).filter(|org| *org > 0.0) {
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
        let mut orgs = Vec::new();
        if self.use_cli {
            match self.cli_accounts() {
                Ok(found) => accounts = found,
                // Without the GitHub CLI, CodexBar's own accounts still fetch, and the configured GitHub CLI
                // accounts report the failure (keeping their last usage). With every gh account wanted, which
                // ones they are is unknown, so the provider fails as a whole and nothing is dropped.
                // No GitHub CLI installed: it has no accounts to lose.
                // The organizations its configured accounts bill keep their cards, as failures.
                Err(err @ ProviderError::NotSignedIn { hint: GH_MISSING })
                    if !self.logins.is_empty() || !self.unreadable.is_empty() =>
                {
                    let cli_users: Vec<&String> = self
                        .billing
                        .iter()
                        .map(|(user, _)| user)
                        .filter(|user| self.login(user).is_none())
                        .filter(|user| !self.unreadable.iter().any(|u| u.eq_ignore_ascii_case(user)))
                        .collect();
                    for user in cli_users {
                        self.org_unreadable(user, &err, &mut orgs);
                    }
                }
                Err(err) => match &self.only {
                    Some(only) if !self.logins.is_empty() || !self.unreadable.is_empty() => {
                        // The organizations those accounts bill keep their cards as failures too.
                        for user in only.iter().filter(|user| self.login(user).is_none()) {
                            self.org_unreadable(user, &err, &mut orgs);
                            cli_failures.push(AccountOutcome::Failed {
                                account: account_id(user),
                                label: Some(user.clone()),
                                error: err.clone(),
                            });
                        }
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
        // An account whose saved token couldn't be read is reported, not taken for signed out, and the organizations
        // it bills keep their cards as failures.
        for username in &self.unreadable {
            accounts.retain(|known| !known.eq_ignore_ascii_case(username));
            let error = ProviderError::Unexpected {
                detail: concat!(
                    "its saved token couldn't be read from ",
                    codexbar_core::secret_store_name!()
                ),
            };
            self.org_unreadable(username, &error, &mut orgs);
            outcomes.push(AccountOutcome::Failed {
                account: account_id(username),
                label: Some(username.clone()),
                error,
            });
        }
        if accounts.is_empty() && outcomes.is_empty() {
            return Err(ProviderError::NotSignedIn { hint: GH_MISSING });
        }
        for username in &accounts {
            let failed = |error| AccountOutcome::Failed {
                account: account_id(username),
                label: Some(username.clone()),
                error,
            };
            let token = match self.token(username) {
                Ok(token) => token,
                Err(error) => {
                    self.org_unreadable(username, &error, &mut orgs);
                    outcomes.push(failed(error));
                    continue;
                }
            };
            match self.fetch_account(username, &token, now) {
                Ok((snapshot, seat)) => {
                    let snapshot = self.add_billing(snapshot, &seat, &token, now, &mut orgs);
                    outcomes.push(AccountOutcome::Fresh(snapshot));
                }
                Err(error) => {
                    // An organization billed on its own is read through its own billing API, apart from the account's
                    // quotas (#233): a seatless owner or billing manager still reads it, and so does any account
                    // whose quota request failed, which keeps its own failure. A seatless account says why it has no
                    // quotas instead of failing.
                    let (mut shown, mut unread) = (Vec::new(), Vec::new());
                    for billing in self.org_only_billing(username) {
                        match self.add_org(billing, &token, now, &mut orgs) {
                            Ok(_) => shown.push(billing.organization.clone()),
                            Err(_) => unread.push(billing.organization.clone()),
                        }
                    }
                    let seatless = error == (ProviderError::Unexpected { detail: NO_SEAT });
                    if seatless && !(shown.is_empty() && unread.is_empty()) {
                        let mut message = "No Copilot seat on this account.".to_owned();
                        if !shown.is_empty() {
                            message.push_str(&format!(" It shows {}'s AI credits.", shown.join(" and ")));
                        }
                        if !unread.is_empty() {
                            message.push_str(&format!(
                                " {}'s AI credits couldn't be read with it.",
                                unread.join(" and ")
                            ));
                        }
                        outcomes.push(AccountOutcome::Fresh(
                            AccountSnapshot::new(account_id(username), Provider::Copilot, Vec::new(), now)
                                .with_label(username)
                                .with_message(message),
                        ));
                    } else {
                        outcomes.push(failed(error));
                    }
                }
            }
        }
        outcomes.extend(orgs.iter().map(|org| org.outcome(now)));
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
        assert_eq!(account.display_name(), "Copilot · HemSoft · Individual Pro");
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
            enterprise: Some("acme".to_owned()),
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
        assert_eq!(provider.http.urls("/copilot/billing"), 1);
        assert_eq!(org.plan(), None, "a refused optional lookup must not invent a plan");
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

    // Copilot Business on an organization with no enterprise (#233). The reports are recorded from GitHub's live
    // `/organizations/hemsoft-dev/...` answers of 2026-10-21, renamed.
    const BUSINESS_USER: &str = r#"{"login":"owner","copilot_plan":"business","organization_login_list":["acme-biz"],
        "access_type_sku":"copilot_for_business_seat_quota","quota_reset_date":"2026-11-01","quota_snapshots":{
        "premium_interactions":{"entitlement":300,"remaining":200,"unlimited":false}}}"#;
    const ORG_REPORT: &str = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme-biz","product":"copilot",
        "usageItems":[{"product":"Copilot","sku":"Copilot AI Credits","model":"Code Review model",
        "unitType":"ai-credits","pricePerUnit":0.01,"grossQuantity":101.238535,"grossAmount":1.01238535,
        "discountQuantity":101.238535,"discountAmount":1.01238535,"netQuantity":0.0,"netAmount":0.0},
        {"product":"Copilot","sku":"Copilot AI Credits","model":"Claude Sonnet 4.5","unitType":"ai-credits",
        "pricePerUnit":0.01,"grossQuantity":4000.0,"grossAmount":40.0,"discountQuantity":1900.0,
        "discountAmount":19.0,"netQuantity":2100.0,"netAmount":21.0}]}"#;
    const EMPTY_REPORT: &str = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme-biz",
        "product":"copilot","usageItems":[]}"#;
    const BUSINESS_BILLING: &str = r#"{"seat_breakdown":{"pending_invitation":0,"pending_cancellation":0,
        "added_this_cycle":1,"total":2,"active_this_cycle":1,"inactive_this_cycle":0},
        "seat_management_setting":"assign_selected","plan_type":"business"}"#;
    /// Every product's AI credits: the Copilot items and a Spark one.
    const ALL_PRODUCTS_REPORT: &str = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme-biz",
        "usageItems":[{"product":"Copilot","sku":"Copilot AI Credits","grossQuantity":4101.24,"netAmount":21.0},
        {"product":"Spark","sku":"Spark AI Credits","grossQuantity":250.0,"netAmount":2.5}]}"#;
    const BUDGETS: &str = r#"{"budgets":[{"id":"1","budget_type":"ProductPricing","budget_product_sku":"actions",
        "budget_scope":"organization","budget_amount":0,"prevent_further_usage":true,"budget_entity_name":"acme-biz",
        "budget_alerting":{"will_alert":true}},{"id":"2","budget_type":"BundlePricing",
        "budget_product_sku":"ai_credits","budget_scope":"organization","budget_amount":20,
        "prevent_further_usage":true,"budget_entity_name":"acme-biz","budget_alerting":{"will_alert":true}},
        {"id":"3","budget_type":"ProductPricing","budget_product_sku":"copilot","budget_scope":"organization",
        "budget_amount":50,"prevent_further_usage":false,"budget_entity_name":"acme-biz"},
        {"id":"4","budget_type":"SkuPricing","budget_product_sku":"copilot_ai_credit",
        "budget_scope":"organization","budget_amount":5,"prevent_further_usage":true,"budget_entity_name":"acme-biz"}],
        "has_next_page":false,"total_count":4}"#;

    fn business(pool_total: Option<u64>) -> OrgBilling {
        OrgBilling {
            enterprise: None,
            organization: "acme-biz".to_owned(),
            pool_total,
        }
    }

    fn org_card(outcomes: &[AccountOutcome]) -> &AccountOutcome {
        outcomes
            .iter()
            .find(|outcome| match outcome {
                AccountOutcome::Fresh(account) => account.id().as_str() == "copilotorg-acme-biz",
                AccountOutcome::Failed { account, .. } => account.as_str() == "copilotorg-acme-biz",
            })
            .expect("the organization's card")
    }

    #[test]
    fn an_organization_without_an_enterprise_reads_its_own_ai_credit_report() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            (
                "/organizations/acme-biz/settings/billing/ai_credit/usage?year=2026&month=10&product=Copilot",
                ok(ORG_REPORT),
            ),
            (
                "/organizations/acme-biz/settings/billing/ai_credit/usage?year=2026&month=10",
                ok(ALL_PRODUCTS_REPORT),
            ),
            ("/orgs/acme-biz/copilot/billing", ok(BUSINESS_BILLING)),
            ("/organizations/acme-biz/settings/billing/budgets", ok(BUDGETS)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_own")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        // The user keeps its own quotas: no enterprise share, no Enterprise-seat warning.
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert_eq!(user.primary().map(Metric::label), Some("Premium requests"));
        assert!(user.messages().is_empty(), "{:?}", user.messages());
        // The organization: 4,101 credits of 2 Business seats x 1,900, on a month-end projection.
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        let Some(Metric::Quota {
            label,
            used,
            limit,
            pace,
            ..
        }) = org.primary()
        else {
            panic!("pool")
        };
        assert_eq!((label.as_str(), *used, *limit), ("AI credits", 4101, 3800));
        assert!(pace.is_some(), "a month-end projection");
        assert_eq!(org.label(), Some("acme-biz"));
        // Its Copilot budgets, not the Actions one, each measured against the usage it covers: the AI-credit bundle
        // against every product's (Spark too), the product budget against Copilot's, the SKU budget not at all.
        assert_eq!(
            org.messages(),
            [
                "AI credits budget: $23.50 of $20.00 spent this month, stops usage at the limit. Spend from before the \
                 budget was created doesn't count toward it.",
                "Copilot budget: $21.00 of $50.00 spent this month, doesn't stop usage.",
                "Budget on copilot_ai_credit: $5.00, stops usage at the limit. Its spend isn't measured here.",
            ]
        );
        assert_eq!(provider.http.urls("/enterprises/"), 0);
        assert_eq!(provider.http.urls("user=owner"), 0, "no enterprise share to read");
    }

    #[test]
    fn an_enterprise_set_beside_the_organization_keeps_the_enterprise_report() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(ENTERPRISE_USER)),
            ("organization=acme-eng", credits(10.0)),
            ("user=dev", credits(1.0)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("dev", "gho_own")
            .with_billing("dev", billing(Some(100)));
        provider.fetch_outcomes(now()).unwrap();
        assert_eq!(
            provider.http.urls("/enterprises/acme/settings/billing/ai_credit/usage"),
            2
        );
        assert_eq!(provider.http.urls("/organizations/"), 0);
        assert_eq!(provider.http.urls("product=Copilot"), 0);
    }

    #[test]
    fn an_owner_configured_after_a_billing_manager_completes_the_pool() {
        // Both bill acme-biz with no pool total. The billing manager's token reads the credits but not the owner-only
        // seat count; the owner's, read second, can, and its pool replaces the card without one.
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(ORG_REPORT)),
            ("/orgs/acme-biz/copilot/billing|Bearer gho_owner", ok(BUSINESS_BILLING)),
            ("/orgs/acme-biz/copilot/billing", HttpResponse::new(403, "")),
            ("/budgets", ok(r#"{"budgets":[],"has_next_page":false}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("manager", "gho_manager")
            .with_login("owner", "gho_owner")
            .with_billing("manager", business(None))
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let cards = outcomes
            .iter()
            .filter(
                |outcome| matches!(outcome, AccountOutcome::Fresh(card) if card.id().as_str() == "copilotorg-acme-biz"),
            )
            .count();
        assert_eq!(cards, 1, "one card per organization");
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        let Some(Metric::Quota { used, limit, .. }) = org.primary() else {
            panic!("the owner's token completes the pool: {:?}", org.messages())
        };
        assert_eq!((*used, *limit), (4101, 3800));
    }

    #[test]
    fn an_empty_month_for_an_organization_is_zero_not_an_error() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(EMPTY_REPORT)),
            ("/copilot/billing", ok(BUSINESS_BILLING)),
            ("/budgets", ok(r#"{"budgets":[],"has_next_page":false}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_own")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("an empty month is fresh")
        };
        let Some(Metric::Quota { used, limit, .. }) = org.primary() else {
            panic!("pool")
        };
        assert_eq!((*used, *limit), (0, 3800));
        assert!(org.messages().is_empty());
    }

    #[test]
    fn an_owner_with_no_copilot_seat_still_sees_the_organization() {
        // copilot_internal/user answers 404: no seat anywhere. The configured pool skips the seat count.
        let http = Routes::new(vec![
            ("ai_credit/usage", ok(ORG_REPORT)),
            ("/budgets", HttpResponse::new(500, "")),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_own")
            .with_billing("owner", business(Some(10_000)));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("a seatless owner isn't a failure")
        };
        assert!(user.metrics().is_empty());
        assert_eq!(
            user.messages(),
            ["No Copilot seat on this account. It shows acme-biz's AI credits."]
        );
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        let Some(Metric::Quota { used, limit, .. }) = org.primary() else {
            panic!("pool")
        };
        assert_eq!((*used, *limit), (4101, 10_000));
        assert_eq!(org.messages(), ["Its Copilot budgets couldn't be read."]);
    }

    #[test]
    fn a_rate_limited_token_waits_for_githubs_retry_time_across_refreshes() {
        let limited = || {
            Routes::new(vec![
                ("copilot_internal/user", ok(BUSINESS_USER)),
                (
                    "ai_credit/usage",
                    HttpResponse {
                        retry_after_secs: Some(1800),
                        ..HttpResponse::new(429, "")
                    },
                ),
            ])
        };
        let provider = |http| {
            CopilotProvider::new(http, NoGh)
                .without_cli()
                .with_login("owner", "gho_rate_limited")
                .with_billing("owner", business(None))
        };
        let first = provider(limited());
        first.fetch_outcomes(now()).unwrap();
        assert_eq!(first.http.urls("ai_credit/usage"), 1);
        // The next refresh builds a new provider; ten minutes on, GitHub's 30-minute wait still holds.
        let later = provider(limited());
        let outcomes = later.fetch_outcomes(now() + chrono::Duration::minutes(10)).unwrap();
        assert_eq!(
            later.http.urls("/organizations/"),
            0,
            "no request before the retry time"
        );
        let AccountOutcome::Failed { error, .. } = org_card(&outcomes) else {
            panic!("org")
        };
        assert_eq!(
            error,
            &ProviderError::RateLimited {
                retry_at: now() + chrono::Duration::minutes(30)
            }
        );
        // Past it, the token asks again.
        let after = provider(limited());
        after.fetch_outcomes(now() + chrono::Duration::minutes(31)).unwrap();
        assert_eq!(after.http.urls("ai_credit/usage"), 1);
    }

    #[test]
    fn a_seatless_owner_whose_organization_cant_be_read_isnt_told_it_shows() {
        let http = Routes::new(vec![("ai_credit/usage", HttpResponse::new(403, ""))]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_refused")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert_eq!(
            user.messages(),
            ["No Copilot seat on this account. acme-biz's AI credits couldn't be read with it."]
        );
        assert!(matches!(org_card(&outcomes), AccountOutcome::Failed { .. }));
    }

    #[test]
    fn budgets_past_the_tenth_page_are_still_read() {
        let page = |more: bool, product: &str| {
            ok(&format!(
                r#"{{"budgets":[{{"budget_type":"ProductPricing","budget_product_sku":"{product}",
                "budget_scope":"organization","budget_amount":5,"prevent_further_usage":true,
                "budget_entity_name":"acme-biz"}}],"has_next_page":{more}}}"#
            ))
        };
        // Eleven pages of other budgets, then the Copilot one on the twelfth. The last page's route comes first, as
        // "page=1" also matches "page=10" and "page=11".
        let mut routes = vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(EMPTY_REPORT)),
            ("/copilot/billing", ok(BUSINESS_BILLING)),
            ("budgets?per_page=100&page=12", page(false, "copilot")),
        ];
        for n in 1..=11 {
            let fragment: &'static str = Box::leak(format!("budgets?per_page=100&page={n}").into_boxed_str());
            routes.push((fragment, page(true, "actions")));
        }
        let provider = CopilotProvider::new(Routes::new(routes), NoGh)
            .without_cli()
            .with_login("owner", "gho_many_budgets")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        assert_eq!(
            org.messages(),
            ["Copilot budget: $0.00 of $5.00 spent this month, stops usage at the limit."]
        );
    }

    #[test]
    fn a_failed_quota_request_still_reads_the_organization() {
        // copilot_internal/user fails with a 502; the organization's own billing API answers.
        let http = Routes::new(vec![
            ("copilot_internal/user", HttpResponse::new(502, "")),
            ("ai_credit/usage", ok(ORG_REPORT)),
            ("/copilot/billing", ok(BUSINESS_BILLING)),
            ("/budgets", ok(r#"{"budgets":[],"has_next_page":false}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_quota_down")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert!(
            matches!(
                &outcomes[0],
                AccountOutcome::Failed {
                    error: ProviderError::Http { status: 502 },
                    ..
                }
            ),
            "the account keeps its own failure"
        );
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("the organization is still read")
        };
        assert!(org.primary().is_some());
    }

    #[test]
    fn a_rate_limited_seat_count_holds_back_the_budget_request() {
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(ORG_REPORT)),
            (
                "/copilot/billing",
                HttpResponse {
                    retry_after_secs: Some(600),
                    ..HttpResponse::new(429, "")
                },
            ),
            ("/budgets", ok(r#"{"budgets":[],"has_next_page":false}"#)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_seats_limited")
            .with_billing("owner", business(None));
        provider.fetch_outcomes(now()).unwrap();
        assert_eq!(provider.http.urls("/copilot/billing"), 1);
        assert_eq!(
            provider.http.urls("/budgets"),
            0,
            "nothing more before GitHub's retry time"
        );
    }

    #[test]
    fn one_account_reads_every_organization_it_is_set_for() {
        let http = Routes::new(vec![
            ("ai_credit/usage", ok(EMPTY_REPORT)),
            ("/budgets", ok(r#"{"budgets":[]}"#)),
        ]);
        let other = OrgBilling {
            organization: "acme-two".to_owned(),
            ..business(Some(1000))
        };
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_two_orgs")
            .with_billing("owner", business(Some(1000)))
            .with_billing("owner", other);
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let ids: Vec<&str> = fresh(&outcomes).iter().map(|a| a.id().as_str()).collect();
        assert_eq!(ids, ["copilot-owner", "copilotorg-acme-biz", "copilotorg-acme-two"]);
        let AccountOutcome::Fresh(user) = &outcomes[0] else {
            panic!("user")
        };
        assert_eq!(
            user.messages(),
            ["No Copilot seat on this account. It shows acme-biz and acme-two's AI credits."]
        );
    }

    #[test]
    fn an_unreadable_token_keeps_the_organization_card_as_a_failure() {
        // The GitHub CLI lists fhemmerrelias, but its token can't be had; its organization's card stays, failed.
        let gh = FakeGh {
            tokens: HashMap::from([("HemSoft", "tok-a")]),
        };
        let provider = CopilotProvider::new(Routes::new(vec![("copilot_internal/user", ok(USER))]), gh)
            .with_billing("fhemmerrelias", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { error, .. } = org_card(&outcomes) else {
            panic!("the organization's card stays, failed")
        };
        assert_eq!(error, &ProviderError::NotSignedIn { hint: GH_LOGIN });
    }

    #[test]
    fn a_rate_limited_quota_request_holds_back_the_organization() {
        let http = Routes::new(vec![
            (
                "copilot_internal/user",
                HttpResponse {
                    retry_after_secs: Some(900),
                    ..HttpResponse::new(429, "")
                },
            ),
            ("ai_credit/usage", ok(ORG_REPORT)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_quota_limited")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert_eq!(
            provider.http.urls("/organizations/"),
            0,
            "no billing request before the retry time"
        );
        let retry_at = now() + chrono::Duration::minutes(15);
        assert!(
            matches!(&outcomes[0], AccountOutcome::Failed { error: ProviderError::RateLimited { retry_at: at }, .. } if *at == retry_at)
        );
        assert!(matches!(
            org_card(&outcomes),
            AccountOutcome::Failed {
                error: ProviderError::RateLimited { .. },
                ..
            }
        ));
    }

    #[test]
    fn an_unreadable_saved_token_keeps_the_organization_card_as_a_failure() {
        let provider = CopilotProvider::new(Routes::new(Vec::new()), NoGh)
            .without_cli()
            .with_unreadable_login("owner")
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert!(
            matches!(org_card(&outcomes), AccountOutcome::Failed { .. }),
            "{outcomes:?}"
        );
    }

    #[test]
    fn a_later_token_fills_in_the_spend_a_bundle_budget_is_measured_against() {
        // Both tokens list the budgets; only the second can read the every-product report.
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("product=Copilot", ok(ORG_REPORT)),
            (
                "ai_credit/usage?year=2026&month=10|Bearer gho_bundle_second",
                ok(ALL_PRODUCTS_REPORT),
            ),
            ("ai_credit/usage?year=2026&month=10", HttpResponse::new(502, "")),
            ("/copilot/billing", ok(BUSINESS_BILLING)),
            ("/budgets", ok(BUDGETS)),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("first", "gho_bundle_first")
            .with_login("second", "gho_bundle_second")
            .with_billing("first", business(None))
            .with_billing("second", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        assert!(
            org.messages()
                .iter()
                .any(|m| m.starts_with("AI credits budget: $23.50 of $20.00")),
            "{:?}",
            org.messages()
        );
    }

    #[test]
    fn a_failed_cli_discovery_keeps_its_accounts_organization_cards() {
        // gh auth status fails outright; the configured CLI account's organization keeps its card, failed, beside
        // the account CodexBar signed in itself.
        struct BrokenGh;
        impl CommandRunner for BrokenGh {
            fn run(&self, _: &str, _: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
                Err(CommandError::Failed)
            }
        }
        let provider = CopilotProvider::new(Routes::new(vec![("copilot_internal/user", ok(USER))]), BrokenGh)
            .only_accounts(vec!["cliuser".into(), "HemSoft".into()])
            .with_login("HemSoft", "gho_managed")
            .with_billing("cliuser", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        assert!(
            matches!(org_card(&outcomes), AccountOutcome::Failed { .. }),
            "{outcomes:?}"
        );
    }

    #[test]
    fn a_missing_github_cli_keeps_its_accounts_organization_cards() {
        let provider = CopilotProvider::new(Routes::new(vec![("copilot_internal/user", ok(USER))]), NoGh)
            .only_accounts(vec!["cliuser".into(), "HemSoft".into()])
            .with_login("HemSoft", "gho_managed_two")
            .with_billing("cliuser", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Failed { error, .. } = org_card(&outcomes) else {
            panic!("the organization's card stays, failed: {outcomes:?}")
        };
        assert_eq!(error, &ProviderError::NotSignedIn { hint: GH_MISSING });
    }

    #[test]
    fn a_seatless_account_without_org_billing_still_fails() {
        let provider = CopilotProvider::new(Routes::new(Vec::new()), NoGh)
            .without_cli()
            .with_login("owner", "gho_own");
        assert_eq!(
            provider.fetch(now()),
            Err(ProviderError::Unexpected { detail: NO_SEAT })
        );
    }

    #[test]
    fn organization_billing_failures_say_what_went_wrong() {
        let cases = [
            (
                HttpResponse::new(403, r#"{"message":"Must have admin rights"}"#),
                ProviderError::Refused { reason: ORG_NO_ACCESS },
            ),
            (
                HttpResponse::new(404, r#"{"message":"Not Found"}"#),
                ProviderError::Refused { reason: ORG_NOT_FOUND },
            ),
            (
                HttpResponse {
                    rate_limit_reset: Some(now() + chrono::Duration::minutes(47)),
                    ..HttpResponse::new(403, r#"{"message":"API rate limit exceeded"}"#)
                },
                ProviderError::RateLimited {
                    retry_at: now() + chrono::Duration::minutes(47),
                },
            ),
            (
                HttpResponse {
                    retry_after_secs: Some(120),
                    ..HttpResponse::new(429, "")
                },
                ProviderError::RateLimited {
                    retry_at: now() + chrono::Duration::seconds(120),
                },
            ),
            (
                HttpResponse::new(403, r#"{"message":"API rate limit exceeded for user ID 1."}"#),
                ProviderError::RateLimited {
                    retry_at: now() + chrono::Duration::seconds(RATE_LIMIT_WAIT),
                },
            ),
        ];
        // Each case its own token: a rate limit is remembered per token.
        for (ix, (response, expected)) in cases.into_iter().enumerate() {
            let http = Routes::new(vec![
                ("copilot_internal/user", ok(BUSINESS_USER)),
                ("ai_credit/usage", response),
            ]);
            let provider = CopilotProvider::new(http, NoGh)
                .without_cli()
                .with_login("owner", format!("gho_failure_case_{ix}"))
                .with_billing("owner", business(None));
            let outcomes = provider.fetch_outcomes(now()).unwrap();
            assert!(
                matches!(&outcomes[0], AccountOutcome::Fresh(_)),
                "the user keeps its quotas"
            );
            let AccountOutcome::Failed { error, label, .. } = org_card(&outcomes) else {
                panic!("org failure")
            };
            assert_eq!(error, &expected);
            assert_eq!(label.as_deref(), Some("acme-biz"));
        }
    }

    #[test]
    fn only_copilot_budgets_of_the_organization_are_shown() {
        let budget = |kind: &str, targets: &[&str], scope: &str, entity: &str| Budget {
            scope: scope.to_owned(),
            kind: kind.to_owned(),
            targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            entity: entity.to_owned(),
            amount: 10.0,
            stops_usage: false,
            alerts: None,
        };
        assert!(caps_copilot(
            &budget("BundlePricing", &["ai_credits"], "organization", "Acme-Biz"),
            "acme-biz"
        ));
        assert!(caps_copilot(
            &budget("ProductPricing", &["copilot"], "organization", ""),
            "acme-biz"
        ));
        assert!(caps_copilot(
            &budget("SkuPricing", &["copilot_ai_credit"], "organization", "acme-biz"),
            "acme-biz"
        ));
        assert!(
            !caps_copilot(
                &budget("SkuPricing", &["copilot_for_business"], "organization", "acme-biz"),
                "acme-biz"
            ),
            "a license budget counts seats, not AI credits"
        );
        assert!(!caps_copilot(
            &budget("ProductPricing", &["actions"], "organization", "acme-biz"),
            "acme-biz"
        ));
        assert!(!caps_copilot(
            &budget("BundlePricing", &["ai_credits"], "user", "owner"),
            "acme-biz"
        ));
        assert!(!caps_copilot(
            &budget("BundlePricing", &["ai_credits"], "organization", "other"),
            "acme-biz"
        ));
        assert!(caps_copilot(
            &budget("SkuPricing", &["coding_agent_ai_credit"], "organization", ""),
            "acme-biz"
        ));
        assert!(!caps_copilot(
            &budget("SkuPricing", &["spark_ai_credits"], "organization", ""),
            "acme-biz"
        ));
        assert_eq!(
            budget_line(&budget("ProductPricing", &["copilot"], "organization", ""), Some(2.5)),
            "Copilot budget: $2.50 of $10.00 spent this month, doesn't stop usage."
        );
        // A bundle whose every-product report couldn't be read is unmeasured, not shown at zero.
        assert_eq!(
            budget_line(&budget("BundlePricing", &["ai_credits"], "organization", ""), None),
            "AI credits budget: $10.00, doesn't stop usage. Its spend isn't measured here."
        );
    }

    /// GitHub: any budget set to $0 USD stops usage immediately for the users it applies to.
    #[test]
    fn a_zero_dollar_budget_stops_usage_at_once() {
        let hard_zero = Budget {
            scope: "organization".into(),
            kind: "BundlePricing".into(),
            targets: vec!["ai_credits".into()],
            entity: String::new(),
            amount: 0.0,
            stops_usage: true,
            alerts: None,
        };
        assert_eq!(
            budget_line(&hard_zero, Some(0.0)),
            "AI credits budget: $0.00 of $0.00 spent this month, usage is stopped: a $0 budget stops it at once."
        );
        assert_eq!(
            budget_line(&hard_zero, None),
            "AI credits budget: $0.00, usage is stopped: a $0 budget stops it at once. Its spend isn't measured here."
        );
    }

    #[test]
    fn complementary_tokens_each_fill_in_the_part_they_can_read() {
        // The owner, first, reads the seats but its budget list fails; the billing manager, second, reads the budgets
        // but not the owner-only seats. The card has both.
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(ORG_REPORT)),
            ("/copilot/billing|Bearer gho_first_owner", ok(BUSINESS_BILLING)),
            ("/copilot/billing", HttpResponse::new(403, "")),
            ("/budgets|Bearer gho_second_manager", ok(BUDGETS)),
            ("/budgets", HttpResponse::new(500, "")),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("owner", "gho_first_owner")
            .with_login("manager", "gho_second_manager")
            .with_billing("owner", business(None))
            .with_billing("manager", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        let Some(Metric::Quota { limit, .. }) = org.primary() else {
            panic!("the owner's pool")
        };
        assert_eq!(*limit, 3800);
        assert_eq!(org.messages().len(), 3, "the manager's budgets: {:?}", org.messages());
        assert!(!org.messages().iter().any(|m| m == BUDGETS_UNREAD));
    }

    #[test]
    fn a_later_token_that_reads_the_budgets_completes_the_card() {
        // Both tokens read the credits and the pool; only the second can read the budgets.
        let http = Routes::new(vec![
            ("copilot_internal/user", ok(BUSINESS_USER)),
            ("ai_credit/usage", ok(ORG_REPORT)),
            ("/copilot/billing", ok(BUSINESS_BILLING)),
            (
                "/budgets|Bearer gho_owner",
                ok(r#"{"budgets":[],"has_next_page":false}"#),
            ),
            ("/budgets", HttpResponse::new(403, "")),
        ]);
        let provider = CopilotProvider::new(http, NoGh)
            .without_cli()
            .with_login("manager", "gho_manager")
            .with_login("owner", "gho_owner")
            .with_billing("manager", business(None))
            .with_billing("owner", business(None));
        let outcomes = provider.fetch_outcomes(now()).unwrap();
        let AccountOutcome::Fresh(org) = org_card(&outcomes) else {
            panic!("org")
        };
        assert!(org.primary().is_some());
        assert!(org.messages().is_empty(), "{:?}", org.messages());
    }

    #[test]
    fn the_allowance_per_seat_follows_the_month() {
        assert_eq!(credits_per_seat(2026, 5), 3900);
        assert_eq!(credits_per_seat(2026, 6), 7000);
        assert_eq!(credits_per_seat(2026, 8), 7000);
        assert_eq!(credits_per_seat(2026, 9), 3900);
        assert_eq!(credits_per_seat(2027, 7), 3900);
        assert_eq!(business_credits_per_seat(2026, 7), 3000);
        assert_eq!(business_credits_per_seat(2026, 10), 1900);
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
