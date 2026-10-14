//! GitHub Billing (#104): one owner's spend across GitHub products this billing month, apart from Copilot's own usage.
//! The owner is the signed-in user or an organization they can read billing for. Only read-only GitHub REST calls are
//! made: the signed-in profile and the owner's billing usage summary.
//!
//! Amounts come from GitHub's usage summary (`/users/{user}/settings/billing/usage/summary` or
//! `/organizations/{org}/settings/billing/usage/summary`) for the current UTC month. A summary for another month is
//! refused, never shown as this month's. GitHub bills in US dollars and its API returns no currency; CodexBar uses
//! dollars only while no currency is returned, and suppresses amounts when a returned currency is partial or conflicts.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Currency, Metric, Money, Provider};
use serde_json::Value;

use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

pub const API: &str = "https://api.github.com";
/// The scope GitHub requires for a user's own billing usage; an organization's accepts the GitHub CLI's `repo`.
pub const PERSONAL_SCOPE: &str = "user";
const SIGN_IN_HINT: &str = "Sign in to GitHub Billing in CodexBar settings.";
const NO_ACCESS: &str = "This GitHub sign-in can't read that owner's billing. An organization's billing needs an owner \
                         or billing manager.";
const NOT_FOUND: &str = "GitHub has no billing usage for that owner, or this sign-in can't see it.";
const OTHER_USER: &str = "The GitHub sign-in belongs to a different user than this account's owner. Sign in again.";
const RATE_LIMIT_WAIT: i64 = 60;

/// Whose billing an account follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The signed-in user's own account.
    Personal(String),
    Organization(String),
}

impl Owner {
    pub fn login(&self) -> &str {
        match self {
            Self::Personal(login) | Self::Organization(login) => login,
        }
    }

    fn summary_path(&self) -> String {
        match self {
            Self::Personal(user) => format!("/users/{user}/settings/billing/usage/summary"),
            Self::Organization(org) => format!("/organizations/{org}/settings/billing/usage/summary"),
        }
    }
}

/// A GitHub login or organization name: letters, digits and single hyphens, as GitHub allows them.
pub fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !login.starts_with('-')
        && !login.ends_with('-')
}

/// A login GitHub returned for the signed-in user. Enterprise Managed Users' logins carry an underscore before the
/// enterprise's shortcode (`octocat_acme`), which organization names never do.
pub fn valid_user_login(login: &str) -> bool {
    match login.rsplit_once('_') {
        Some((user, shortcode)) => {
            valid_login(user) && !shortcode.is_empty() && shortcode.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => valid_login(login),
    }
}

/// One product and SKU line of the summary. Amounts are dollars with GitHub's own precision.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageLine {
    pub product: String,
    pub sku: String,
    pub unit: Option<String>,
    pub price_per_unit: Option<f64>,
    pub gross_quantity: f64,
    /// The discounted and billable parts of the gross quantity, when GitHub splits it.
    pub discount_quantity: Option<f64>,
    pub net_quantity: Option<f64>,
    pub gross: f64,
    pub discount: f64,
    pub net: f64,
}

/// The owner's billing month as GitHub reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub year: i32,
    pub month: u32,
    pub lines: Vec<UsageLine>,
    /// The currency the amounts are in, or `None` when the returned evidence can't be trusted.
    pub currency: Option<Currency>,
}

/// A summary CodexBar can't use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownShape;

fn number(value: Option<&Value>) -> Option<f64> {
    value?.as_f64().filter(|n| n.is_finite())
}

/// Parses a usage summary for `owner`. The summary must name the same owner when it names one.
pub fn parse_summary(body: &str, owner: &Owner) -> Result<Summary, UnknownShape> {
    let value: Value = serde_json::from_str(body).map_err(|_| UnknownShape)?;
    let period = value.get("timePeriod").ok_or(UnknownShape)?;
    let year = period
        .get("year")
        .and_then(Value::as_i64)
        .and_then(|y| i32::try_from(y).ok())
        .ok_or(UnknownShape)?;
    let month = period
        .get("month")
        .and_then(Value::as_u64)
        .and_then(|m| u32::try_from(m).ok())
        .filter(|m| (1..=12).contains(m))
        .ok_or(UnknownShape)?;
    let named = match owner {
        Owner::Personal(_) => value.get("user"),
        Owner::Organization(_) => value.get("organization"),
    };
    if let Some(named) = named
        && !named.as_str().is_some_and(|n| n.eq_ignore_ascii_case(owner.login()))
    {
        return Err(UnknownShape);
    }
    let items = value.get("usageItems").and_then(Value::as_array).ok_or(UnknownShape)?;
    let mut lines = Vec::new();
    let mut currencies = Vec::new();
    currencies.push(value.get("currency"));
    for item in items {
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
        };
        currencies.push(item.get("currency"));
        lines.push(UsageLine {
            product: text("product").ok_or(UnknownShape)?.to_owned(),
            sku: text("sku").ok_or(UnknownShape)?.to_owned(),
            unit: text("unitType").map(str::to_owned),
            price_per_unit: number(item.get("pricePerUnit")).filter(|p| *p >= 0.0),
            gross_quantity: number(item.get("grossQuantity")).ok_or(UnknownShape)?,
            discount_quantity: number(item.get("discountQuantity")),
            net_quantity: number(item.get("netQuantity")),
            gross: number(item.get("grossAmount")).ok_or(UnknownShape)?,
            discount: number(item.get("discountAmount")).ok_or(UnknownShape)?,
            net: number(item.get("netAmount")).ok_or(UnknownShape)?,
        });
    }
    Ok(Summary {
        year,
        month,
        lines,
        currency: currency(&currencies),
    })
}

/// GitHub documents billing in US dollars and returns no currency. A currency on the summary or every line, all the
/// same, is used; one on only some places, conflicting ones or an unknown code mean no amounts can be shown.
fn currency(evidence: &[Option<&Value>]) -> Option<Currency> {
    let top = evidence.first().copied().flatten();
    let lines = &evidence[1..];
    let returned: Vec<&Value> = evidence
        .iter()
        .filter_map(|value| *value)
        .filter(|v| !v.is_null())
        .collect();
    if returned.is_empty() {
        return Some(Currency::Usd);
    }
    let codes: Option<Vec<Currency>> = returned
        .iter()
        .map(|value| value.as_str().and_then(Currency::from_code))
        .collect();
    let codes = codes?;
    // Either the summary names it, or every line does.
    let complete = top.is_some_and(|v| !v.is_null()) || lines.iter().all(|v| v.is_some_and(|v| !v.is_null()));
    let first = *codes.first()?;
    (complete && codes.iter().all(|code| *code == first)).then_some(first)
}

/// The first instant of the UTC month after `year`/`month`.
fn next_month(year: i32, month: u32) -> Option<DateTime<Utc>> {
    let (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single()
}

fn money(amount: f64, currency: Currency) -> Money {
    let scale = 10f64.powi(currency.minor_digits() as i32);
    Money::new((amount * scale).round() as i64, currency)
}

/// "$0.006/minute": the unit price with GitHub's own precision.
fn unit_price(price: f64, unit: Option<&str>, currency: Currency) -> String {
    let unit = unit.map(|unit| {
        let unit = unit.to_lowercase();
        unit.strip_suffix('s').map(str::to_owned).unwrap_or(unit)
    });
    match unit {
        Some(unit) => format!("{}{price}/{unit}", currency.symbol()),
        None => format!("{}{price} each", currency.symbol()),
    }
}

fn quantity(amount: f64) -> String {
    let rounded = amount.round();
    if (amount - rounded).abs() < 0.005 {
        codexbar_core::format_credits(rounded)
    } else {
        codexbar_core::format_credits(amount)
    }
}

/// The metrics and messages for a verified summary at `now`: net spend first, then gross charges and discounts, and
/// one line per product with each SKU's quantity and unit price.
pub fn usage(summary: &Summary, now: DateTime<Utc>) -> (Vec<Metric>, Vec<String>) {
    let mut metrics = Vec::new();
    let mut messages = Vec::new();
    let resets_at = next_month(summary.year, summary.month);
    let Some(currency) = summary.currency else {
        messages
            .push("GitHub returned amounts with a missing or conflicting currency, so no spend is shown.".to_owned());
        return (metrics, messages);
    };
    let total = |pick: fn(&UsageLine) -> f64| money(summary.lines.iter().map(pick).sum(), currency);
    let net = total(|line| line.net);
    for (label, spent) in [
        ("Net spend", net),
        ("Gross charges", total(|line| line.gross)),
        ("Discounts", total(|line| line.discount)),
    ] {
        metrics.push(Metric::Spend {
            label: label.into(),
            spent,
            limit: None,
            resets_at,
            pace: None,
        });
    }
    // A projection needs the current month and at least a day of it.
    if let (Some(start), Some(end)) = (
        Utc.with_ymd_and_hms(summary.year, summary.month, 1, 0, 0, 0).single(),
        resets_at,
    ) {
        let elapsed = now - start;
        if net.cents() > 0 && elapsed >= Duration::days(1) && now < end {
            let factor = (end - start).num_seconds() as f64 / elapsed.num_seconds() as f64;
            let projected = Money::new((net.cents() as f64 * factor).round() as i64, currency);
            messages.push(format!(
                "At this pace, about {} net by the end of the month.",
                projected.display()
            ));
        }
    }
    let mut products: BTreeMap<&str, Vec<&UsageLine>> = BTreeMap::new();
    for line in &summary.lines {
        products.entry(line.product.as_str()).or_default().push(line);
    }
    for (product, lines) in products {
        let sum = |pick: fn(&UsageLine) -> f64| money(lines.iter().map(|l| pick(l)).sum(), currency);
        let skus: Vec<String> = lines
            .iter()
            .map(|line| {
                let amount = match &line.unit {
                    Some(unit) => format!("{} {}", quantity(line.gross_quantity), unit.to_lowercase()),
                    None => quantity(line.gross_quantity),
                };
                match line.price_per_unit {
                    Some(price) => format!(
                        "{} {amount} at {}",
                        line.sku,
                        unit_price(price, line.unit.as_deref(), currency)
                    ),
                    None => format!("{} {amount}", line.sku),
                }
            })
            .collect();
        messages.push(format!(
            "{product}: {} net of {} gross, {} discounts. {}.",
            sum(|l| l.net).display(),
            sum(|l| l.gross).display(),
            sum(|l| l.discount).display(),
            skus.join("; ")
        ));
    }
    (metrics, messages)
}

fn get<H: HttpClient>(http: &H, path: &str, token: &str, now: DateTime<Utc>) -> Result<String, ProviderError> {
    let bearer = format!("Bearer {token}");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", "2022-11-28"),
    ];
    let response = http.get(&format!("{API}{path}"), &headers)?;
    match response.status {
        200 => Ok(response.body),
        401 => Err(ProviderError::Expired { hint: SIGN_IN_HINT }),
        403 if response.body.to_lowercase().contains("rate limit") => Err(ProviderError::RateLimited {
            retry_at: now
                + Duration::seconds(
                    response
                        .retry_after_secs
                        .map_or(RATE_LIMIT_WAIT, |s| s.min(3600) as i64),
                ),
        }),
        403 => Err(ProviderError::Refused { reason: NO_ACCESS }),
        404 => Err(ProviderError::Refused { reason: NOT_FOUND }),
        429 => Err(ProviderError::RateLimited {
            retry_at: now
                + Duration::seconds(
                    response
                        .retry_after_secs
                        .map_or(RATE_LIMIT_WAIT, |s| s.min(3600) as i64),
                ),
        }),
        status => Err(ProviderError::Http { status }),
    }
}

/// The signed-in user's login.
pub fn signed_in_login<H: HttpClient>(http: &H, token: &str, now: DateTime<Utc>) -> Result<String, ProviderError> {
    profile(http, token, now).map(|(login, _)| login)
}

/// The signed-in user's login and plan name (the plan needs the `user` scope; without it there is none).
fn profile<H: HttpClient>(
    http: &H,
    token: &str,
    now: DateTime<Utc>,
) -> Result<(String, Option<String>), ProviderError> {
    let body = get(http, "/user", token, now)?;
    let value = serde_json::from_str::<Value>(&body).ok();
    let login = value
        .as_ref()
        .and_then(|v| v.get("login")?.as_str().map(str::to_owned))
        .filter(|login| valid_user_login(login))
        .ok_or(ProviderError::Unexpected {
            detail: "a GitHub profile without a login",
        })?;
    let plan = value.and_then(|v| v.pointer("/plan/name")?.as_str().map(str::to_owned));
    Ok((login, plan))
}

/// The summary for `owner`'s current UTC billing month. A summary for any other month is refused.
pub fn current_summary<H: HttpClient>(
    http: &H,
    token: &str,
    owner: &Owner,
    now: DateTime<Utc>,
) -> Result<Summary, ProviderError> {
    let path = format!("{}?year={}&month={}", owner.summary_path(), now.year(), now.month());
    let body = get(http, &path, token, now)?;
    let summary = parse_summary(&body, owner).map_err(|UnknownShape| ProviderError::Unexpected {
        detail: "GitHub billing usage in an unknown shape",
    })?;
    if (summary.year, summary.month) != (now.year(), now.month()) {
        return Err(ProviderError::Unexpected {
            detail: "GitHub billing usage for another month",
        });
    }
    Ok(summary)
}

/// Checks a new sign-in before it is saved: who signed in, and that their token can read `organization`'s billing, or
/// their own when it is `None`. Returns the user's login.
pub fn verify<H: HttpClient>(
    http: &H,
    token: &str,
    organization: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String, ProviderError> {
    let login = signed_in_login(http, token, now)?;
    let owner = match organization {
        Some(org) => Owner::Organization(org.to_owned()),
        None => Owner::Personal(login.clone()),
    };
    current_summary(http, token, &owner, now)?;
    Ok(login)
}

/// One GitHub Billing account: a token CodexBar keeps for it and the owner it follows.
pub struct GitHubBillingProvider<H: HttpClient> {
    http: H,
    token: Option<String>,
    owner: Option<Owner>,
    account: String,
    label: String,
    unreadable: bool,
}

impl<H: HttpClient> GitHubBillingProvider<H> {
    /// The account's saved token couldn't be read: reported as such, not taken for signed out.
    pub fn with_unreadable_token(mut self) -> Self {
        self.unreadable = true;
        self
    }

    /// `owner` is `None` until the account has signed in once (its personal owner is the signed-in user).
    pub fn new(
        http: H,
        token: Option<String>,
        owner: Option<Owner>,
        account: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            http,
            token: token.filter(|t| !t.trim().is_empty()),
            owner,
            account: account.into(),
            label: label.into(),
            unreadable: false,
        }
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        if self.unreadable {
            return Err(ProviderError::Unexpected {
                detail: concat!(
                    "its saved token couldn't be read from ",
                    codexbar_core::secret_store_name!()
                ),
            });
        }
        let (Some(token), Some(owner)) = (&self.token, &self.owner) else {
            return Err(ProviderError::NotSignedIn { hint: SIGN_IN_HINT });
        };
        let personal_plan = match owner {
            Owner::Personal(user) => {
                let (login, plan) = profile(&self.http, token, now)?;
                if !login.eq_ignore_ascii_case(user) {
                    return Err(ProviderError::Refused { reason: OTHER_USER });
                }
                plan
            }
            Owner::Organization(_) => None,
        };
        let summary = current_summary(&self.http, token, owner, now)?;
        let (mut metrics, mut messages) = usage(&summary, now);
        // Included allowances and budgets (#105) add to what is shown; their failures never hide the spend.
        let (more_metrics, more_messages) = self.allowances(token, owner, personal_plan, &summary, now);
        metrics.extend(more_metrics);
        messages.extend(more_messages);
        let mut account = AccountSnapshot::new(
            AccountId::new(self.account.clone()),
            Provider::GitHubBilling,
            metrics,
            now,
        );
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        Ok(messages.into_iter().fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> GitHubBillingProvider<H> {
    /// Included allowances for the owner's plan and, for an organization, its budgets (#105).
    fn allowances(
        &self,
        token: &str,
        owner: &Owner,
        personal_plan: Option<String>,
        summary: &Summary,
        now: DateTime<Utc>,
    ) -> (Vec<Metric>, Vec<String>) {
        use crate::github_allowances::{self as allowances, Scope};
        let mut metrics = Vec::new();
        let mut messages = Vec::new();
        let (scope, base) = match owner {
            Owner::Personal(user) => (Scope::Personal, format!("/users/{user}")),
            Owner::Organization(org) => (Scope::Organization, format!("/organizations/{org}")),
        };
        let plan_name = match owner {
            Owner::Personal(_) => personal_plan.ok_or_else(|| "GitHub didn't return this account's plan".to_owned()),
            Owner::Organization(org) => get(&self.http, &format!("/orgs/{org}"), token, now)
                .map_err(|err| err.to_string())
                .and_then(|body| {
                    serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| v.pointer("/plan/name")?.as_str().map(str::to_owned))
                        // GitHub shows an organization's plan only to its owners, so a billing manager's sign-in
                        // reads its spend and budgets but not which allowances it includes.
                        .ok_or_else(|| {
                            "GitHub shows the organization's plan only to its owners, and this sign-in isn't one"
                                .to_owned()
                        })
                }),
        };
        let query = format!("?year={}&month={}", now.year(), now.month());
        let detail = get(&self.http, &format!("{base}/settings/billing/usage{query}"), token, now)
            .map_err(|err| err.to_string())
            .and_then(|body| {
                allowances::parse_detail(&body)
                    .map_err(|UnknownShape| "GitHub's billing detail came in an unknown shape".to_owned())
            });
        match (plan_name, &detail) {
            (Err(reason), _) => messages.push(format!("Included allowances: not shown, {reason}.")),
            (Ok(name), _) if allowances::plan(&name, scope).is_none() => messages.push(format!(
                "Included allowances: not shown for the {name} plan, which GitHub's included-usage table doesn't list \
                 here."
            )),
            (Ok(_), Err(reason)) => messages.push(format!("Included allowances: not shown, {reason}.")),
            (Ok(name), Ok(detail)) => {
                let plan = allowances::plan(&name, scope).unwrap_or_else(|| unreachable!("checked above"));
                let visibility =
                    self.visibility(token, owner.login(), &allowances::repositories_to_classify(detail), now);
                let list = allowances::allowances(&plan, scope, owner.login(), summary, detail, &visibility);
                if let (Some(start), Some(end)) = (allowances::month_start(summary), allowances::month_end(summary)) {
                    let (shown, lines) = allowances::show(&list, start, end, now);
                    metrics.extend(shown);
                    messages.push(format!("Included with {}:", plan.label));
                    messages.extend(lines);
                }
            }
        }
        if let Owner::Organization(org) = owner {
            match self.budgets(token, org, now) {
                Ok(budgets) => {
                    messages.extend(allowances::budget_lines(&budgets, org, summary, detail.as_deref().ok()));
                }
                Err(reason) => messages.push(format!("Budgets couldn't be read: {reason}")),
            }
        }
        (metrics, messages)
    }

    /// Every page of an organization's budgets. A list longer than the pages read is an error, never a short list.
    fn budgets(
        &self,
        token: &str,
        org: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<crate::github_allowances::Budget>, String> {
        const PAGES: u32 = 10;
        let mut all = Vec::new();
        for page in 1..=PAGES {
            let body = get(
                &self.http,
                &format!("/organizations/{org}/settings/billing/budgets?per_page=100&page={page}"),
                token,
                now,
            )
            .map_err(|err| err.to_string())?;
            let (budgets, more) = crate::github_allowances::parse_budgets(&body)
                .map_err(|UnknownShape| "they came in an unknown shape".to_owned())?;
            all.extend(budgets);
            if !more {
                return Ok(all);
            }
        }
        Err(format!("there are more than {PAGES} pages of them"))
    }

    /// Which of `repositories` are private, read once and remembered for hours. Too many to look up, or one that can't
    /// be read, stays unknown.
    fn visibility(
        &self,
        token: &str,
        owner: &str,
        repositories: &[String],
        now: DateTime<Utc>,
    ) -> std::collections::HashMap<String, bool> {
        use crate::github_allowances::{MAX_REPOSITORIES, remember, remembered};
        let mut known = std::collections::HashMap::new();
        if repositories.len() > MAX_REPOSITORIES {
            return known;
        }
        for name in repositories {
            // GitHub returns the bare name; `owner/repo` is read too, and another owner's repository stays unknown.
            let repo = match name.split_once('/') {
                Some((repo_owner, repo)) if repo_owner.eq_ignore_ascii_case(owner) => repo,
                Some(_) => continue,
                None => name.as_str(),
            };
            if !valid_login(owner) || repo.contains('/') || repo.is_empty() {
                continue;
            }
            let private = remembered(owner, repo).or_else(|| {
                let body = get(&self.http, &format!("/repos/{owner}/{repo}"), token, now).ok()?;
                let value = serde_json::from_str::<Value>(&body).ok()?;
                // A repository that has moved to another owner is answered by its new home, whose visibility says
                // nothing about this owner's usage of it.
                let moved = value
                    .get("full_name")
                    .and_then(Value::as_str)
                    .and_then(|name| name.split_once('/'))
                    .is_some_and(|(new_owner, _)| !new_owner.eq_ignore_ascii_case(owner));
                if moved {
                    return None;
                }
                let private = value.get("private")?.as_bool()?;
                remember(owner, repo, private);
                Some(private)
            });
            if let Some(private) = private {
                known.insert(name.clone(), private);
            }
        }
        known
    }
}

impl<H: HttpClient> UsageProvider for GitHubBillingProvider<H> {
    fn name(&self) -> &'static str {
        "GitHub Billing"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.snapshot(now).map(|account| vec![account])
    }

    /// Every failure is this account's own, so it shows on its row.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        Ok(vec![match self.snapshot(now) {
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::HttpResponse;

    fn now() -> DateTime<Utc> {
        "2026-10-13T00:00:00Z".parse().unwrap()
    }

    /// Answers GETs by path (query ignored) and records every URL.
    #[derive(Clone, Default)]
    struct GitHub {
        routes: Arc<Mutex<HashMap<String, HttpResponse>>>,
        urls: Arc<Mutex<Vec<String>>>,
    }

    impl GitHub {
        fn on(self, path: &str, status: u16, body: &str) -> Self {
            self.routes
                .lock()
                .unwrap()
                .insert(path.to_owned(), HttpResponse::new(status, body));
            self
        }
    }

    impl HttpClient for GitHub {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, ProviderError> {
            assert!(url.starts_with(API));
            assert!(headers.contains(&("Authorization", "Bearer gho_billing")));
            self.urls.lock().unwrap().push(url.to_owned());
            // A route with its query answers that query only; one without answers every query.
            let full = &url[API.len()..];
            let path = full.split('?').next().unwrap();
            let routes = self.routes.lock().unwrap();
            routes
                .get(full)
                .or_else(|| routes.get(path))
                .cloned()
                .ok_or(ProviderError::Network)
        }
    }

    const PERSONAL: &str = r#"{"timePeriod":{"year":2026,"month":10},"user":"HemSoft","usageItems":[
        {"product":"Actions","sku":"actions_linux","grossQuantity":16596.0,"discountQuantity":14722.666666666,
         "netQuantity":1873.333333334,"grossAmount":99.576,"discountAmount":88.336,"netAmount":11.24,"pricePerUnit":0.006,
         "unitType":"minutes"},
        {"product":"Actions","sku":"actions_macos","grossQuantity":8083.0,"grossAmount":501.146,"discountAmount":488.498,
         "netAmount":12.648,"pricePerUnit":0.062,"unitType":"minutes"},
        {"product":"Copilot","sku":"copilot_premium_request","grossQuantity":120.0,"grossAmount":4.8,"discountAmount":4.8,
         "netAmount":0.0,"pricePerUnit":0.04,"unitType":"requests"}]}"#;

    fn personal() -> Owner {
        Owner::Personal("HemSoft".into())
    }

    fn provider(github: &GitHub, owner: Owner) -> GitHubBillingProvider<GitHub> {
        GitHubBillingProvider::new(
            github.clone(),
            Some("gho_billing".into()),
            Some(owner),
            "gb-1",
            "Personal",
        )
    }

    #[test]
    fn a_personal_month_shows_net_gross_and_discounts_with_unit_prices() {
        let github = GitHub::default().on("/user", 200, r#"{"login":"HemSoft"}"#).on(
            "/users/HemSoft/settings/billing/usage/summary",
            200,
            PERSONAL,
        );
        let account = provider(&github, personal()).fetch(now()).unwrap().remove(0);
        assert_eq!(account.display_name(), "GitHub Billing · Personal");
        let spent: Vec<(String, String)> = account
            .metrics()
            .iter()
            .map(|m| (m.label().to_owned(), m.used_display()))
            .collect();
        assert_eq!(
            spent,
            [
                ("Net spend".to_owned(), "$23.89 spent".to_owned()),
                ("Gross charges".to_owned(), "$605.52 spent".to_owned()),
                ("Discounts".to_owned(), "$581.63 spent".to_owned()),
            ]
        );
        assert_eq!(account.metrics()[0].resets_at(), "2026-11-01T00:00:00Z".parse().ok());
        assert_eq!(account.metrics()[0].used_fraction(), None, "no limit is invented");
        let messages = account.messages();
        assert!(messages.contains(&"At this pace, about $61.72 net by the end of the month.".to_owned()));
        assert!(messages.contains(
            &"Actions: $23.89 net of $600.72 gross, $576.83 discounts. actions_linux 16,596 minutes at $0.006/minute; \
              actions_macos 8,083 minutes at $0.062/minute."
                .to_owned()
        ));
        assert!(messages.contains(
            &"Copilot: $0.00 net of $4.80 gross, $4.80 discounts. copilot_premium_request 120 requests at $0.04/request."
                .to_owned()
        ));
        let urls = github.urls.lock().unwrap().clone();
        assert!(urls[1].ends_with("/users/HemSoft/settings/billing/usage/summary?year=2026&month=10"));
    }

    #[test]
    fn an_organization_reads_its_own_summary_without_a_profile_check() {
        let org = r#"{"timePeriod":{"year":2026,"month":10},"organization":"hemsoft-dev","usageItems":[]}"#;
        let github = GitHub::default().on("/organizations/hemsoft-dev/settings/billing/usage/summary", 200, org);
        let account = provider(&github, Owner::Organization("hemsoft-dev".into()))
            .fetch(now())
            .unwrap()
            .remove(0);
        assert_eq!(account.metrics()[0].used_display(), "$0.00 spent");
        assert!(
            !account.messages().iter().any(|m| m.starts_with("At this pace")),
            "nothing spent, nothing projected"
        );
        assert!(
            github.urls.lock().unwrap().iter().all(|url| !url.ends_with("/user")),
            "an organization needs no profile"
        );
    }

    #[test]
    fn another_month_or_owner_is_never_shown_as_this_one() {
        let september = PERSONAL.replace(r#""month":10"#, r#""month":9"#);
        let github = GitHub::default().on("/user", 200, r#"{"login":"HemSoft"}"#).on(
            "/users/HemSoft/settings/billing/usage/summary",
            200,
            &september,
        );
        assert!(matches!(
            provider(&github, personal()).fetch(now()),
            Err(ProviderError::Unexpected { detail }) if detail.contains("another month")
        ));
        let someone_else = PERSONAL.replace(r#""user":"HemSoft""#, r#""user":"octocat""#);
        assert_eq!(parse_summary(&someone_else, &personal()), Err(UnknownShape));
    }

    #[test]
    fn a_personal_account_signed_in_as_someone_else_is_refused() {
        let github = GitHub::default().on("/user", 200, r#"{"login":"octocat"}"#);
        assert_eq!(
            provider(&github, personal()).fetch(now()).unwrap_err(),
            ProviderError::Refused { reason: OTHER_USER }
        );
    }

    #[test]
    fn a_returned_currency_must_be_complete_and_consistent() {
        let with = |top: &str, first: &str, second: &str| {
            format!(
                r#"{{"timePeriod":{{"year":2026,"month":10}},{top}"usageItems":[
                {{"product":"A","sku":"a","grossQuantity":1,"grossAmount":1,"discountAmount":0,"netAmount":1{first}}},
                {{"product":"A","sku":"b","grossQuantity":1,"grossAmount":1,"discountAmount":0,"netAmount":1{second}}}]}}"#
            )
        };
        let parsed = |body: String| parse_summary(&body, &personal()).unwrap().currency;
        assert_eq!(
            parsed(with("", "", "")),
            Some(Currency::Usd),
            "GitHub's documented dollars"
        );
        assert_eq!(parsed(with(r#""currency":"EUR","#, "", "")), Some(Currency::Eur));
        assert_eq!(
            parsed(with("", r#","currency":"EUR""#, r#","currency":"EUR""#)),
            Some(Currency::Eur)
        );
        assert_eq!(parsed(with("", r#","currency":"EUR""#, "")), None, "partial");
        assert_eq!(
            parsed(with("", r#","currency":"EUR""#, r#","currency":"USD""#)),
            None,
            "conflicting"
        );
        assert_eq!(parsed(with(r#""currency":"XYZ","#, "", "")), None, "unknown");
        let summary = parse_summary(&with("", r#","currency":"EUR""#, ""), &personal()).unwrap();
        let (metrics, messages) = usage(&summary, now());
        assert!(metrics.is_empty());
        assert!(messages[0].contains("no spend is shown"));
    }

    #[test]
    fn a_projection_needs_a_day_of_the_month() {
        let github = GitHub::default().on("/user", 200, r#"{"login":"HemSoft"}"#).on(
            "/users/HemSoft/settings/billing/usage/summary",
            200,
            PERSONAL,
        );
        let early = "2026-10-01T06:00:00Z".parse().unwrap();
        let account = provider(&github, personal()).fetch(early).unwrap().remove(0);
        assert!(!account.messages().iter().any(|m| m.starts_with("At this pace")));
    }

    #[test]
    fn refusals_and_missing_fields_are_the_accounts_own() {
        let failure =
            |github: GitHub, owner: Owner| match provider(&github, owner).fetch_outcomes(now()).unwrap().remove(0) {
                AccountOutcome::Failed { account, error, .. } => {
                    assert_eq!(account.as_str(), "gb-1");
                    error
                }
                AccountOutcome::Fresh(_) => panic!("a failure"),
            };
        let org = || Owner::Organization("acme".into());
        let path = "/organizations/acme/settings/billing/usage/summary";
        assert!(matches!(
            failure(GitHub::default().on(path, 401, ""), org()),
            ProviderError::Expired { .. }
        ));
        assert_eq!(
            failure(
                GitHub::default().on(path, 403, r#"{"message":"Must have admin rights"}"#),
                org()
            ),
            ProviderError::Refused { reason: NO_ACCESS }
        );
        assert!(matches!(
            failure(
                GitHub::default().on(path, 403, r#"{"message":"API rate limit exceeded"}"#),
                org()
            ),
            ProviderError::RateLimited { .. }
        ));
        assert_eq!(
            failure(GitHub::default().on(path, 404, ""), org()),
            ProviderError::Refused { reason: NOT_FOUND }
        );
        let broken = r#"{"timePeriod":{"year":2026,"month":10},"usageItems":[{"product":"Actions","sku":"x"}]}"#;
        assert!(matches!(
            failure(GitHub::default().on(path, 200, broken), org()),
            ProviderError::Unexpected { .. }
        ));
        let signed_out = GitHubBillingProvider::new(GitHub::default(), None, Some(org()), "gb-1", "x");
        assert!(matches!(
            signed_out.fetch(now()),
            Err(ProviderError::NotSignedIn { .. })
        ));
        let unreadable =
            GitHubBillingProvider::new(GitHub::default(), None, Some(org()), "gb-1", "x").with_unreadable_token();
        assert_eq!(
            unreadable.fetch(now()).unwrap_err(),
            ProviderError::Unexpected {
                detail: concat!(
                    "its saved token couldn't be read from ",
                    codexbar_core::secret_store_name!()
                )
            },
            "an unreadable token isn't taken for signed out"
        );
    }

    #[test]
    fn verifying_a_sign_in_reads_the_profile_and_the_chosen_owners_billing() {
        let github = GitHub::default().on("/user", 200, r#"{"login":"HemSoft"}"#).on(
            "/users/HemSoft/settings/billing/usage/summary",
            200,
            PERSONAL,
        );
        assert_eq!(verify(&github, "gho_billing", None, now()).unwrap(), "HemSoft");
        let github = GitHub::default().on("/user", 200, r#"{"login":"HemSoft"}"#).on(
            "/organizations/acme/settings/billing/usage/summary",
            403,
            "{}",
        );
        assert_eq!(
            verify(&github, "gho_billing", Some("acme"), now()).unwrap_err(),
            ProviderError::Refused { reason: NO_ACCESS }
        );
    }

    #[test]
    fn logins_follow_githubs_rules() {
        assert!(valid_login("hemsoft-dev") && valid_login("HemSoft"));
        for bad in ["", "-x", "x-", "a/b", "a b", "../x", &"x".repeat(40)] {
            assert!(!valid_login(bad), "{bad}");
        }
        // An Enterprise Managed User's login is a user's login, never an organization name.
        assert!(valid_user_login("octocat_acme") && valid_user_login("HemSoft"));
        assert!(!valid_login("octocat_acme"));
        for bad in [
            "_acme",
            "octocat_",
            "octocat_ac-me",
            "a/b_acme",
            "octocat_a/b",
            "-x_acme",
        ] {
            assert!(!valid_user_login(bad), "{bad}");
        }
    }

    #[test]
    fn an_enterprise_managed_users_login_is_accepted() {
        let github = GitHub::default().on("/user", 200, r#"{"login":"octocat_acme"}"#).on(
            "/organizations/acme/settings/billing/usage/summary",
            200,
            r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme","usageItems":[]}"#,
        );
        assert_eq!(
            verify(&github, "gho_billing", Some("acme"), now()).unwrap(),
            "octocat_acme"
        );
    }

    #[test]
    fn a_pro_account_shows_its_included_allowances_beside_the_spend() {
        let detail = r#"{"usageItems":[
            {"product":"actions","sku":"Actions Linux","quantity":1200,"unitType":"Minutes","pricePerUnit":0.006,
             "grossAmount":7.2,"discountAmount":7.2,"netAmount":0,"repositoryName":"secret"},
            {"product":"actions","sku":"Actions Linux","quantity":9000,"unitType":"Minutes","pricePerUnit":0.006,
             "grossAmount":54,"discountAmount":54,"netAmount":0,"repositoryName":"open"}]}"#;
        let github = GitHub::default()
            .on("/user", 200, r#"{"login":"HemSoft","plan":{"name":"pro"}}"#)
            .on("/users/HemSoft/settings/billing/usage/summary", 200, PERSONAL)
            .on("/users/HemSoft/settings/billing/usage", 200, detail)
            .on("/repos/HemSoft/secret", 200, r#"{"private":true}"#)
            .on("/repos/HemSoft/open", 200, r#"{"private":false}"#);
        let account = provider(&github, personal()).fetch(now()).unwrap().remove(0);
        let labels: Vec<&str> = account.metrics().iter().map(Metric::label).collect();
        assert_eq!(
            labels[..4],
            ["Net spend", "Gross charges", "Discounts", "Actions minutes"]
        );
        assert_eq!(account.metrics()[3].used_display(), "40%");
        assert!(account.messages().contains(&"Included with Pro:".to_owned()));
        assert!(account.messages().contains(
            &"Actions minutes: 1,200 minutes of 3,000 minutes included used (40%), 1,800 minutes left.".to_owned()
        ));
        // Visibility is remembered: a second refresh doesn't ask for the repositories again.
        let before = github.urls.lock().unwrap().len();
        provider(&github, personal()).fetch(now()).unwrap();
        let asked = github.urls.lock().unwrap()[before..]
            .iter()
            .filter(|url| url.contains("/repos/"))
            .count();
        assert_eq!(asked, 0);
    }

    #[test]
    fn allowances_that_cant_be_read_never_hide_the_spend() {
        let org = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme","usageItems":[]}"#;
        let github = GitHub::default()
            .on("/organizations/acme/settings/billing/usage/summary", 200, org)
            .on("/orgs/acme", 200, r#"{"plan":{"name":"enterprise"}}"#)
            .on(
                "/organizations/acme/settings/billing/usage",
                200,
                r#"{"usageItems":[]}"#,
            )
            .on("/organizations/acme/settings/billing/budgets", 403, "{}");
        let account = provider(&github, Owner::Organization("acme".into()))
            .fetch(now())
            .unwrap()
            .remove(0);
        assert_eq!(account.metrics().len(), 3, "spend only");
        assert!(
            account
                .messages()
                .iter()
                .any(|m| m.contains("not shown for the enterprise plan"))
        );
        assert!(
            account
                .messages()
                .iter()
                .any(|m| m.starts_with("Budgets couldn't be read"))
        );
    }

    #[test]
    fn every_page_of_budgets_is_read_and_a_billing_manager_is_told_why_allowances_are_missing() {
        let org = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme","usageItems":[]}"#;
        let budget = |amount: u32| {
            format!(
                r#"{{"budget_type":"ProductPricing","budget_product_sku":"actions","budget_scope":"organization",
                    "budget_amount":{amount},"prevent_further_usage":true,"budget_entity_name":"acme",
                    "budget_alerting":{{"will_alert":true}}}}"#
            )
        };
        let page = "/organizations/acme/settings/billing/budgets?per_page=100&page=";
        let github = GitHub::default()
            .on("/organizations/acme/settings/billing/usage/summary", 200, org)
            // A billing manager reads the organization's public profile, without its plan.
            .on("/orgs/acme", 200, r#"{"login":"acme"}"#)
            .on(
                "/organizations/acme/settings/billing/usage",
                200,
                r#"{"usageItems":[]}"#,
            )
            .on(
                &format!("{page}1"),
                200,
                &format!(r#"{{"budgets":[{}],"has_next_page":true}}"#, budget(10)),
            )
            .on(
                &format!("{page}2"),
                200,
                &format!(r#"{{"budgets":[{}],"has_next_page":false}}"#, budget(20)),
            );
        let account = provider(&github, Owner::Organization("acme".into()))
            .fetch(now())
            .unwrap()
            .remove(0);
        let budgets: Vec<_> = account
            .messages()
            .iter()
            .filter(|m| m.starts_with("Budget for"))
            .collect();
        assert_eq!(budgets.len(), 2, "{:?}", account.messages());
        assert!(budgets[1].contains("of $20.00"));
        assert!(
            account.messages().contains(
                &"Included allowances: not shown, GitHub shows the organization's plan only to its owners, and this \
              sign-in isn't one."
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_repository_named_with_its_owner_is_looked_up_under_that_owner_only() {
        let github = GitHub::default()
            .on("/repos/acme/site", 200, r#"{"full_name":"acme/site","private":true}"#)
            .on(
                "/repos/acme/other",
                200,
                r#"{"full_name":"acme/other","private":false}"#,
            );
        let provider = provider(&github, Owner::Organization("acme".into()));
        let known = provider.visibility(
            "gho_billing",
            "acme",
            &["acme/site".to_owned(), "other".to_owned(), "elsewhere/site".to_owned()],
            now(),
        );
        assert_eq!(known.get("acme/site"), Some(&true));
        assert_eq!(known.get("other"), Some(&false));
        assert_eq!(
            known.get("elsewhere/site"),
            None,
            "another owner's repository stays unknown"
        );
    }
}
