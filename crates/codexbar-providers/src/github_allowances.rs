//! GitHub Billing included allowances and budgets (#105): how much of each plan's included Actions minutes and
//! storage, Packages, Git LFS and (personal) Codespaces usage this month has used, and an organization's returned
//! budgets.
//!
//! Every figure is verified or left out with its reason; one metric that can't be verified never hides another.
//! Allowance sizes follow GitHub's "Product usage included with each plan" table and standard runner prices follow
//! "Actions runner pricing", both checked on 2026-10-12. Enterprise plans pool allowances above the organization and
//! aren't shown.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, TimeZone, Utc};
use codexbar_core::{Metric, Money};
use serde_json::Value;

use crate::github_billing::{Summary, UnknownShape};
use crate::pace::elapsed_pace;

/// Whose plan an allowance comes from: a personal account's or an organization's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Personal,
    Organization,
}

/// One plan's included usage a month. Storage limits are GB held on average; the rest are per month.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub label: &'static str,
    pub actions_minutes: f64,
    pub actions_storage_gb: f64,
    pub packages_storage_gb: f64,
    pub packages_transfer_gb: f64,
    pub lfs_storage_gb: f64,
    pub lfs_bandwidth_gb: f64,
    /// Codespaces is included only on personal plans.
    pub codespaces_core_hours: Option<f64>,
    pub codespaces_storage_gb: Option<f64>,
}

/// The plan's included usage, for the plans GitHub's table lists at the owner's scope.
pub fn plan(name: &str, scope: Scope) -> Option<Plan> {
    let name = name.trim().to_lowercase();
    Some(match (scope, name.as_str()) {
        (Scope::Personal, "free") => Plan {
            label: "Free",
            actions_minutes: 2_000.0,
            actions_storage_gb: 0.5,
            packages_storage_gb: 0.5,
            packages_transfer_gb: 1.0,
            lfs_storage_gb: 10.0,
            lfs_bandwidth_gb: 10.0,
            codespaces_core_hours: Some(120.0),
            codespaces_storage_gb: Some(15.0),
        },
        (Scope::Personal, "pro") => Plan {
            label: "Pro",
            actions_minutes: 3_000.0,
            actions_storage_gb: 1.0,
            packages_storage_gb: 2.0,
            packages_transfer_gb: 10.0,
            lfs_storage_gb: 10.0,
            lfs_bandwidth_gb: 10.0,
            codespaces_core_hours: Some(180.0),
            codespaces_storage_gb: Some(20.0),
        },
        (Scope::Organization, "free") => Plan {
            label: "Free",
            actions_minutes: 2_000.0,
            actions_storage_gb: 0.5,
            packages_storage_gb: 0.5,
            packages_transfer_gb: 1.0,
            lfs_storage_gb: 10.0,
            lfs_bandwidth_gb: 10.0,
            codespaces_core_hours: None,
            codespaces_storage_gb: None,
        },
        (Scope::Organization, "team") => Plan {
            label: "Team",
            actions_minutes: 3_000.0,
            actions_storage_gb: 2.0,
            packages_storage_gb: 2.0,
            packages_transfer_gb: 10.0,
            lfs_storage_gb: 250.0,
            lfs_bandwidth_gb: 250.0,
            codespaces_core_hours: None,
            codespaces_storage_gb: None,
        },
        _ => return None,
    })
}

/// Letters and digits, lowercased: "Actions macOS 3-core" and "actions_macos_3_core" read the same.
fn normalized(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}

/// The per-minute price of each standard GitHub-hosted runner, whose minutes count toward the included minutes.
const STANDARD_RUNNERS: [(&str, f64); 6] = [
    ("actionslinuxslim", 0.002),
    ("actionslinux", 0.006),
    ("actionslinuxarm", 0.005),
    ("actionswindows", 0.010),
    ("actionswindowsarm", 0.010),
    ("actionsmacos", 0.062),
];
/// Included minutes are counted at the Linux 2-core rate: a macOS minute uses about ten of them.
const ALLOWANCE_MINUTE_PRICE: f64 = 0.006;

#[derive(Debug, PartialEq)]
enum Runner {
    Standard(f64),
    /// Paid larger runners and self-hosted runners don't use included minutes.
    Excluded,
    Unknown,
}

/// A SKU as the summary's id and the detail report's display name both read it: "Actions Linux" and `actions_linux`
/// are `actionslinux`. The detail report says "Actions macOS 3-core" for the summary's `actions_macos`.
fn sku_key(sku: &str) -> String {
    let sku = normalized(sku);
    if sku == "actionsmacos3core" {
        "actionsmacos".to_owned()
    } else {
        sku
    }
}

fn runner(sku: &str) -> Runner {
    let sku = sku_key(sku);
    if let Some((_, price)) = STANDARD_RUNNERS.iter().find(|(name, _)| *name == sku) {
        return Runner::Standard(*price);
    }
    let larger = sku.contains("core")
        || sku.contains("gpu")
        || sku.contains("advanced")
        || sku.ends_with("macosl")
        || sku.ends_with("macosxl");
    if sku.contains("selfhosted") || larger {
        Runner::Excluded
    } else {
        Runner::Unknown
    }
}

/// One row of the billing detail report: usage by day, SKU and repository.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailLine {
    pub product: String,
    pub sku: String,
    pub unit: String,
    pub quantity: f64,
    pub price_per_unit: Option<f64>,
    pub net: f64,
    pub repository: Option<String>,
    pub organization: Option<String>,
}

/// Parses `/…/settings/billing/usage`.
pub fn parse_detail(body: &str) -> Result<Vec<DetailLine>, UnknownShape> {
    let value: Value = serde_json::from_str(body).map_err(|_| UnknownShape)?;
    let items = value.get("usageItems").and_then(Value::as_array).ok_or(UnknownShape)?;
    items
        .iter()
        .map(|item| {
            let text = |key: &str| {
                item.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
            };
            let number = |key: &str| item.get(key).and_then(Value::as_f64).filter(|n| n.is_finite());
            Ok(DetailLine {
                product: text("product").ok_or(UnknownShape)?,
                sku: text("sku").ok_or(UnknownShape)?,
                unit: text("unitType").ok_or(UnknownShape)?,
                quantity: number("quantity").filter(|q| *q >= 0.0).ok_or(UnknownShape)?,
                price_per_unit: number("pricePerUnit"),
                net: number("netAmount").ok_or(UnknownShape)?,
                repository: text("repositoryName"),
                organization: text("organizationName"),
            })
        })
        .collect()
}

/// One allowance this month: verified, or left out with the reason.
#[derive(Clone, Debug, PartialEq)]
pub enum Allowance {
    Verified {
        label: &'static str,
        unit: &'static str,
        used: f64,
        included: f64,
    },
    Unavailable {
        label: &'static str,
        reason: String,
    },
}

/// The repositories whose visibility decides which Actions usage counts toward included usage.
pub fn repositories_to_classify(detail: &[DetailLine]) -> Vec<String> {
    let mut names: Vec<String> = detail
        .iter()
        .filter(|line| normalized(&line.product) == "actions")
        .filter_map(|line| line.repository.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    names.sort();
    names
}

/// The billing month's length in hours, to turn GB-hours into GB held on average.
fn month_hours(summary: &Summary) -> Option<f64> {
    let start = Utc.with_ymd_and_hms(summary.year, summary.month, 1, 0, 0, 0).single()?;
    let (year, month) = if summary.month == 12 {
        (summary.year + 1, 1)
    } else {
        (summary.year, summary.month + 1)
    };
    let end = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).single()?;
    Some((end - start).num_hours() as f64)
}

fn is_unit(unit: Option<&str>, expected: &str) -> bool {
    unit.is_some_and(|unit| normalized(unit) == expected)
}

/// Summary lines of one product whose SKU contains `part`.
fn summary_lines<'a>(
    summary: &'a Summary,
    product: &str,
    part: &'a str,
) -> impl Iterator<Item = &'a crate::github_billing::UsageLine> + 'a {
    let product = normalized(product);
    summary
        .lines
        .iter()
        .filter(move |line| normalized(&line.product) == product && normalized(&line.sku).contains(part))
}

/// Gross, discounted and billable quantities must add up; a line that doesn't makes its allowance unverifiable.
fn consistent(line: &crate::github_billing::UsageLine) -> bool {
    match (line.discount_quantity, line.net_quantity) {
        (Some(discount), Some(net)) => {
            discount >= -1e-6
                && net >= -1e-6
                && (line.gross_quantity - discount - net).abs() <= 1e-3_f64.max(line.gross_quantity * 1e-6)
        }
        // A summary without the split can't show billable usage before the allowance runs out.
        _ => true,
    }
}

/// An allowance read from the summary: `convert` turns a line's gross quantity into the allowance's unit, or `None`
/// when it can't.
fn from_summary(
    label: &'static str,
    unit: &'static str,
    included: f64,
    lines: Vec<&crate::github_billing::UsageLine>,
    convert: &dyn Fn(&crate::github_billing::UsageLine) -> Option<f64>,
) -> Allowance {
    if let Some(line) = lines.iter().find(|line| !consistent(line)) {
        return Allowance::Unavailable {
            label,
            reason: format!("GitHub's quantities for {} don't add up", line.sku),
        };
    }
    let mut used = 0.0;
    for line in &lines {
        match convert(line) {
            Some(amount) => used += amount,
            None => {
                return Allowance::Unavailable {
                    label,
                    reason: format!("{} is reported in a unit CodexBar doesn't know", line.sku),
                };
            }
        }
    }
    let billable = lines.iter().filter_map(|line| line.net_quantity).sum::<f64>();
    if billable > 1e-6 && used + 1e-6 < included {
        return Allowance::Unavailable {
            label,
            reason: "GitHub bills usage the included amount should still cover".to_owned(),
        };
    }
    Allowance::Verified {
        label,
        unit,
        used,
        included,
    }
}

/// Each allowance of `plan` this month, from the summary and the detail report. `visibility` says which repositories
/// are private (only those use included Actions usage); a repository missing from it can't be classified.
pub fn allowances(
    plan: &Plan,
    scope: Scope,
    owner: &str,
    summary: &Summary,
    detail: &[DetailLine],
    visibility: &HashMap<String, bool>,
) -> Vec<Allowance> {
    let mut out = Vec::new();
    let Some(hours) = month_hours(summary) else {
        return out;
    };
    // Organization detail must all be this organization's.
    let foreign = scope == Scope::Organization
        && detail.iter().any(|line| {
            line.organization
                .as_deref()
                .is_some_and(|org| !org.eq_ignore_ascii_case(owner))
        });

    // GitHub bills a covered SKU only once its included usage is used up, so billable usage on one means the allowance
    // is spent and the billable part is exactly what went past it. That needs no repository visibility, which GitHub
    // only reports as it is now, not as it was when the usage happened (a repository made public or moved this month).
    let billed = |product: &str, part: &str, convert: &dyn Fn(&crate::github_billing::UsageLine) -> Option<f64>| {
        let mut billed = 0.0;
        for line in summary_lines(summary, product, part) {
            billed += convert(line)?;
        }
        Some(billed)
    };
    let billed_minutes = billed("actions", "", &|line| match runner(&line.sku) {
        Runner::Standard(price) if is_unit(line.unit.as_deref(), "minutes") => {
            let priced = line.price_per_unit.is_none_or(|p| (p - price).abs() <= 1e-9);
            priced.then_some(line.net_quantity? * price / ALLOWANCE_MINUTE_PRICE)
        }
        // Larger and self-hosted runners don't use included minutes; an unknown runner's billable minutes can't be
        // told apart, so the billed total isn't verified.
        Runner::Excluded => Some(0.0),
        Runner::Unknown
            if is_unit(line.unit.as_deref(), "minutes") && line.net_quantity.is_some_and(|net| net > 1e-9) =>
        {
            None
        }
        _ => Some(0.0),
    });

    // Actions minutes, at the Linux rate, from private repositories on standard runners.
    let minutes = || -> Result<f64, String> {
        if let Some(billed) = billed_minutes.filter(|billed| *billed > 0.01) {
            return Ok(plan.actions_minutes + billed);
        }
        if foreign {
            return Err("GitHub's detail report names another organization".to_owned());
        }
        let mut used = 0.0;
        let mut unclassified = 0;
        for line in detail
            .iter()
            .filter(|line| normalized(&line.product) == "actions" && normalized(&line.unit) == "minutes")
        {
            let price = match runner(&line.sku) {
                Runner::Excluded => continue,
                Runner::Unknown => return Err(format!("{} is a runner CodexBar doesn't know", line.sku)),
                Runner::Standard(price) => price,
            };
            if line
                .price_per_unit
                .is_none_or(|returned| (returned - price).abs() > 1e-9)
            {
                return Err(format!(
                    "{} isn't billed at GitHub's published {price}/minute",
                    line.sku
                ));
            }
            match line.repository.as_ref().and_then(|repo| visibility.get(repo)) {
                Some(true) => used += line.quantity * price / ALLOWANCE_MINUTE_PRICE,
                Some(false) => {}
                None => unclassified += 1,
            }
        }
        if unclassified > 0 {
            return Err("CodexBar couldn't tell which repositories are private".to_owned());
        }
        Ok(used)
    };
    out.push(match minutes() {
        Ok(used) => Allowance::Verified {
            label: "Actions minutes",
            unit: "minutes",
            used,
            included: plan.actions_minutes,
        },
        Err(reason) => Allowance::Unavailable {
            label: "Actions minutes",
            reason,
        },
    });

    // Actions storage: artifact GB-hours from private repositories, held on average over the month. Cache and custom
    // image storage have allowances of their own. Artifact storage shares its allowance with Packages storage, which
    // can't be classified (see below), so Packages storage in use leaves it unverified until something is billable.
    let artifact_storage = |sku: &str| normalized(sku) == "actionsstorage";
    let billed_storage = summary
        .lines
        .iter()
        .filter(|line| {
            (normalized(&line.product) == "actions" && artifact_storage(&line.sku))
                || (normalized(&line.product) == "packages" && normalized(&line.sku).contains("storage"))
        })
        .map(|line| is_unit(line.unit.as_deref(), "gigabytehours").then_some(line.net_quantity? / hours))
        .sum::<Option<f64>>();
    let packages_storage_used = summary.lines.iter().any(|line| {
        normalized(&line.product) == "packages"
            && normalized(&line.sku).contains("storage")
            && line.gross_quantity > 0.0
    });
    let storage = || -> Result<f64, String> {
        if let Some(billed) = billed_storage.filter(|billed| *billed > 1e-9) {
            return Ok(plan.actions_storage_gb + billed);
        }
        if packages_storage_used {
            return Err(
                "it shares its allowance with Packages storage, and GitHub doesn't say which package usage is free"
                    .to_owned(),
            );
        }
        if foreign {
            return Err("GitHub's detail report names another organization".to_owned());
        }
        let mut gb_hours = 0.0;
        for line in detail
            .iter()
            .filter(|line| normalized(&line.product) == "actions" && artifact_storage(&line.sku))
        {
            if normalized(&line.unit) != "gigabytehours" {
                return Err(format!("{} is reported in a unit CodexBar doesn't know", line.sku));
            }
            match line.repository.as_ref().and_then(|repo| visibility.get(repo)) {
                Some(true) => gb_hours += line.quantity,
                Some(false) => {}
                None => return Err("CodexBar couldn't tell which repositories are private".to_owned()),
            }
        }
        Ok(gb_hours / hours)
    };
    out.push(match storage() {
        Ok(used) => Allowance::Verified {
            label: "Actions storage",
            unit: "GB",
            used,
            included: plan.actions_storage_gb,
        },
        Err(reason) => Allowance::Unavailable {
            label: "Actions storage",
            reason,
        },
    });

    // Packages: GitHub doesn't say which package usage is free (public packages, Actions downloads), so only a
    // verified zero, or billable usage past the allowance, can be shown.
    for (label, part, included) in [
        ("Packages storage", "storage", plan.packages_storage_gb),
        ("Packages data transfer", "transfer", plan.packages_transfer_gb),
    ] {
        let lines: Vec<_> = summary
            .lines
            .iter()
            .filter(|line| normalized(&line.product) == "packages")
            .filter(|line| {
                let sku = normalized(&line.sku);
                sku.contains(part) || (part == "transfer" && sku.contains("bandwidth"))
            })
            .collect();
        let gb = |line: &crate::github_billing::UsageLine, quantity: f64| match normalized(
            line.unit.as_deref().unwrap_or_default(),
        )
        .as_str()
        {
            "gigabytehours" => Some(quantity / hours),
            "gigabytes" => Some(quantity),
            _ => None,
        };
        let billable: Option<f64> = lines.iter().map(|line| gb(line, line.net_quantity?)).sum();
        out.push(if let Some(billable) = billable.filter(|billable| *billable > 1e-9) {
            Allowance::Verified {
                label,
                unit: "GB",
                used: included + billable,
                included,
            }
        } else if lines.iter().all(|line| line.gross_quantity <= 0.0) {
            Allowance::Verified {
                label,
                unit: "GB",
                used: 0.0,
                included,
            }
        } else {
            Allowance::Unavailable {
                label,
                reason: "GitHub doesn't say which package usage is free".to_owned(),
            }
        });
    }

    // Git LFS storage (GB-hours) and bandwidth (GB).
    out.push(from_summary(
        "Git LFS storage",
        "GB",
        plan.lfs_storage_gb,
        summary_lines(summary, "gitlfs", "storage").collect(),
        &|line| is_unit(line.unit.as_deref(), "gigabytehours").then(|| line.gross_quantity / hours),
    ));
    out.push(from_summary(
        "Git LFS bandwidth",
        "GB",
        plan.lfs_bandwidth_gb,
        summary_lines(summary, "gitlfs", "bandwidth").collect(),
        &|line| is_unit(line.unit.as_deref(), "gigabytes").then_some(line.gross_quantity),
    ));

    // Codespaces, personal plans only: machine hours times each SKU's documented cores, and storage.
    if let (Some(core_hours), Some(storage_gb)) = (plan.codespaces_core_hours, plan.codespaces_storage_gb) {
        out.push(from_summary(
            "Codespaces compute",
            "core hours",
            core_hours,
            summary_lines(summary, "codespaces", "compute").collect(),
            &|line| {
                let cores = cores(&line.sku)?;
                is_unit(line.unit.as_deref(), "hours").then_some(line.gross_quantity * cores)
            },
        ));
        out.push(from_summary(
            "Codespaces storage",
            "GB",
            storage_gb,
            summary_lines(summary, "codespaces", "storage").collect(),
            &|line| is_unit(line.unit.as_deref(), "gigabytehours").then(|| line.gross_quantity / hours),
        ));
    }
    out
}

/// The core count in a Codespaces compute SKU: GitHub's ids end in `_d<cores>` (`codespaces_compute_d4`), its display
/// names say "Codespaces compute (4-core)".
fn cores(sku: &str) -> Option<f64> {
    let lower = sku.trim().to_lowercase();
    if let Some((_, suffix)) = lower.rsplit_once("_d")
        && !suffix.is_empty()
        && suffix.chars().all(|c| c.is_ascii_digit())
    {
        return suffix.parse::<f64>().ok().filter(|cores| *cores > 0.0);
    }
    let at = lower.find("core")?;
    let digits: String = lower[..at]
        .chars()
        .rev()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse::<f64>().ok().filter(|cores| *cores > 0.0)
}

fn amount(value: f64, unit: &str) -> String {
    let text = if unit == "minutes" || unit == "core hours" {
        codexbar_core::format_credits(value.round())
    } else {
        let rounded = (value * 100.0).round() / 100.0;
        codexbar_core::format_credits(rounded)
    };
    format!("{text} {unit}")
}

/// Metrics and messages for `allowances`: a window per verified allowance, which can pass 100%, and a line each
/// saying used, included and left (or over).
pub fn show(
    allowances: &[Allowance],
    month_start: DateTime<Utc>,
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> (Vec<Metric>, Vec<String>) {
    let mut metrics = Vec::new();
    let mut messages = Vec::new();
    for allowance in allowances {
        match allowance {
            Allowance::Verified {
                label,
                unit,
                used,
                included,
            } => {
                let fraction = used / included;
                metrics.push(Metric::Window {
                    label: (*label).to_owned(),
                    used: fraction,
                    resets_at,
                    pace: elapsed_pace(fraction, month_start, resets_at, now),
                    length: (month_start < resets_at).then(|| resets_at - month_start),
                });
                let left = if used <= included {
                    format!("{} left", amount(included - used, unit))
                } else {
                    format!("{} over", amount(used - included, unit))
                };
                messages.push(format!(
                    "{label}: {} of {} included used ({:.0}%), {left}.",
                    amount(*used, unit),
                    amount(*included, unit),
                    fraction * 100.0
                ));
            }
            Allowance::Unavailable { label, reason } => messages.push(format!("{label}: not shown, {reason}.")),
        }
    }
    (metrics, messages)
}

/// An organization budget as GitHub returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct Budget {
    pub scope: String,
    pub kind: String,
    /// The products (`ProductPricing`) or SKUs (`SkuPricing`) it caps; empty caps everything.
    pub targets: Vec<String>,
    pub entity: String,
    pub amount: f64,
    pub stops_usage: bool,
    /// Whether it sends alerts; `None` when GitHub didn't say.
    pub alerts: Option<bool>,
}

/// One page of `/organizations/{org}/settings/billing/budgets`, and whether another follows.
pub fn parse_budgets(body: &str) -> Result<(Vec<Budget>, bool), UnknownShape> {
    let value: Value = serde_json::from_str(body).map_err(|_| UnknownShape)?;
    let list = value.get("budgets").and_then(Value::as_array).ok_or(UnknownShape)?;
    let more = value.get("has_next_page").and_then(Value::as_bool).unwrap_or(false);
    let budgets = list
        .iter()
        .map(|budget| {
            let text = |key: &str| budget.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
            // A single budget names its target in `budget_product_sku`; GitHub documents the list with an array,
            // `budget_product_skus`, and returns either.
            let targets = match budget.get("budget_product_skus") {
                Some(Value::Array(skus)) => skus
                    .iter()
                    .map(|sku| sku.as_str().map(str::to_owned).ok_or(UnknownShape))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => Some(text("budget_product_sku"))
                    .filter(|t| !t.is_empty())
                    .into_iter()
                    .collect(),
            };
            Ok(Budget {
                scope: text("budget_scope"),
                kind: text("budget_type"),
                targets,
                entity: text("budget_entity_name"),
                amount: budget
                    .get("budget_amount")
                    .and_then(Value::as_f64)
                    .filter(|a| a.is_finite() && *a >= 0.0)
                    .ok_or(UnknownShape)?,
                stops_usage: budget
                    .get("prevent_further_usage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                alerts: budget.pointer("/budget_alerting/will_alert").and_then(Value::as_bool),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((budgets, more))
}

/// One line per budget: what it caps, how much of it this month's usage has spent, and what it does at its limit.
/// Organization and repository budgets on products or SKUs are measured from the owner's own usage; other scopes
/// (cost centers, enterprises) can't be, and say so. `detail` is `None` when the detail report couldn't be read, which
/// leaves repository budgets unmeasured rather than at zero.
pub fn budget_lines(budgets: &[Budget], owner: &str, summary: &Summary, detail: Option<&[DetailLine]>) -> Vec<String> {
    let currency = summary.currency;
    budgets
        .iter()
        .map(|budget| {
            let target = if budget.targets.is_empty() {
                "all products".to_owned()
            } else {
                budget.targets.join(", ")
            };
            let by_sku = budget.kind.eq_ignore_ascii_case("SkuPricing");
            // Only product and SKU budgets can be matched to usage lines. A bundle (`BundlePricing` covers every AI
            // credit SKU) or a type GitHub adds later is listed unmeasured, never at a spend that misses its SKUs.
            let measurable = by_sku || budget.kind.eq_ignore_ascii_case("ProductPricing");
            let wanted: Vec<String> = budget
                .targets
                .iter()
                .map(|t| if by_sku { sku_key(t) } else { normalized(t) })
                .collect();
            let covers = |product: &str, sku: &str| {
                let key = if by_sku { sku_key(sku) } else { normalized(product) };
                wanted.is_empty() || wanted.contains(&key)
            };
            let behaviour = match (budget.stops_usage, budget.alerts) {
                (true, Some(false)) => "stops usage at the limit, no alerts",
                (true, _) => "stops usage at the limit",
                (false, Some(true)) => "alerts only",
                (false, Some(false)) => "neither stops usage nor alerts",
                (false, None) => "doesn't stop usage",
            };
            // An organization-scoped budget from the organization's own list is that organization's, named or not.
            let own_organization = budget.entity.is_empty() || budget.entity.eq_ignore_ascii_case(owner);
            let spent = match (budget.scope.as_str(), currency) {
                (_, None) => None,
                _ if !measurable => None,
                ("organization", Some(_)) if own_organization => Some(
                    summary
                        .lines
                        .iter()
                        .filter(|line| covers(&line.product, &line.sku))
                        .map(|line| line.net)
                        .sum::<f64>(),
                ),
                ("repository", Some(_)) => detail.map(|detail| {
                    let repo = budget.entity.rsplit('/').next().unwrap_or(&budget.entity);
                    detail
                        .iter()
                        .filter(|line| {
                            line.repository
                                .as_deref()
                                .is_some_and(|r| r.rsplit('/').next().unwrap_or(r).eq_ignore_ascii_case(repo))
                        })
                        .filter(|line| covers(&line.product, &line.sku))
                        .map(|line| line.net)
                        .sum::<f64>()
                }),
                _ => None,
            };
            let entity = if budget.entity.is_empty() && budget.scope == "organization" {
                owner
            } else {
                &budget.entity
            };
            let money = |dollars: f64| {
                let currency = currency.unwrap_or_default();
                Money::new((dollars * 100.0).round() as i64, currency).display()
            };
            match spent {
                Some(spent) => format!(
                    "Budget for {target} ({} {entity}): {} of {} spent this month, {behaviour}.",
                    budget.scope,
                    money(spent),
                    money(budget.amount)
                ),
                None => format!(
                    "Budget for {target} ({} {entity}): {}, {behaviour}; this account's usage can't show what it has \
                     spent.",
                    budget.scope,
                    money(budget.amount)
                ),
            }
        })
        .collect()
}

/// Repository visibility read earlier, by "owner/repo", so a refresh doesn't ask GitHub for each repository again.
static VISIBILITY: Mutex<BTreeMap<String, (bool, Instant)>> = Mutex::new(BTreeMap::new());
const VISIBILITY_TTL: StdDuration = StdDuration::from_secs(6 * 3600);
/// More repositories than this in one month's Actions usage are left unclassified rather than looked up one by one.
pub const MAX_REPOSITORIES: usize = 40;

/// Remembered visibility of `owner/repo`, when still fresh.
pub fn remembered(owner: &str, repo: &str) -> Option<bool> {
    let key = format!("{}/{}", owner.to_lowercase(), repo.to_lowercase());
    let cache = VISIBILITY.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&key)
        .filter(|(_, at)| at.elapsed() < VISIBILITY_TTL)
        .map(|(private, _)| *private)
}

pub fn remember(owner: &str, repo: &str, private: bool) {
    let key = format!("{}/{}", owner.to_lowercase(), repo.to_lowercase());
    VISIBILITY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, (private, Instant::now()));
}

/// The billing month's first instant.
pub fn month_start(summary: &Summary) -> Option<DateTime<Utc>> {
    Utc.with_ymd_and_hms(summary.year, summary.month, 1, 0, 0, 0).single()
}

/// The billing month's end.
pub fn month_end(summary: &Summary) -> Option<DateTime<Utc>> {
    month_start(summary).map(|start| start + Duration::hours(month_hours(summary).unwrap_or_default() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_billing::{Owner, parse_summary};
    use codexbar_core::Currency;

    fn summary(lines: &str) -> Summary {
        let body = format!(r#"{{"timePeriod":{{"year":2026,"month":10}},"user":"HemSoft","usageItems":[{lines}]}}"#);
        parse_summary(&body, &Owner::Personal("HemSoft".into())).unwrap()
    }

    fn detail(lines: &str) -> Vec<DetailLine> {
        parse_detail(&format!(r#"{{"usageItems":[{lines}]}}"#)).unwrap()
    }

    fn minutes(sku: &str, quantity: f64, price: f64, repo: &str) -> String {
        format!(
            r#"{{"product":"actions","sku":"{sku}","quantity":{quantity},"unitType":"Minutes","pricePerUnit":{price},
               "grossAmount":0,"discountAmount":0,"netAmount":0,"repositoryName":"{repo}"}}"#
        )
    }

    fn private(names: &[(&str, bool)]) -> HashMap<String, bool> {
        names
            .iter()
            .map(|(name, private)| ((*name).to_owned(), *private))
            .collect()
    }

    fn pro() -> Plan {
        plan("pro", Scope::Personal).unwrap()
    }

    fn find<'a>(all: &'a [Allowance], name: &str) -> &'a Allowance {
        all.iter()
            .find(|a| match a {
                Allowance::Verified { label, .. } | Allowance::Unavailable { label, .. } => *label == name,
            })
            .unwrap()
    }

    #[test]
    fn plans_follow_githubs_included_usage_table() {
        assert_eq!(pro().actions_storage_gb, 1.0, "Pro storage is 1 GB as of 2026-10");
        assert_eq!(plan("team", Scope::Organization).unwrap().lfs_storage_gb, 250.0);
        assert_eq!(plan("Free", Scope::Organization).unwrap().codespaces_core_hours, None);
        assert_eq!(
            plan("enterprise", Scope::Organization),
            None,
            "enterprise pools above the organization"
        );
        assert_eq!(plan("team", Scope::Personal), None);
    }

    #[test]
    fn private_standard_minutes_count_at_the_linux_rate() {
        let lines = [
            minutes("Actions Linux", 100.0, 0.006, "secret"),
            minutes("Actions macOS 3-core", 10.0, 0.062, "secret"),
            minutes("Actions Linux", 500.0, 0.006, "open"),
            minutes("Actions Linux 4-core", 50.0, 0.012, "secret"),
            minutes("Actions self-hosted", 70.0, 0.0, "secret"),
        ]
        .join(",");
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &summary(""),
            &detail(&lines),
            &private(&[("secret", true), ("open", false)]),
        );
        let Allowance::Verified { used, included, .. } = find(&all, "Actions minutes") else {
            panic!("verified");
        };
        // 100 Linux minutes plus 10 macOS minutes at ten and a third times the rate.
        assert!((used - (100.0 + 10.0 * 0.062 / 0.006)).abs() < 1e-9, "{used}");
        assert_eq!(*included, 3_000.0);
    }

    #[test]
    fn unknown_runners_prices_or_repositories_leave_minutes_out() {
        let unknown = detail(&minutes("Actions Quantum", 1.0, 0.006, "secret"));
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &summary(""),
            &unknown,
            &private(&[("secret", true)]),
        );
        assert!(
            matches!(find(&all, "Actions minutes"), Allowance::Unavailable { reason, .. } if reason.contains("doesn't know"))
        );
        let priced = detail(&minutes("Actions Linux", 1.0, 0.007, "secret"));
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &summary(""),
            &priced,
            &private(&[("secret", true)]),
        );
        assert!(matches!(find(&all, "Actions minutes"), Allowance::Unavailable { .. }));
        let unclassified = detail(&minutes("Actions Linux", 1.0, 0.006, "mystery"));
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &summary(""),
            &unclassified,
            &HashMap::new(),
        );
        assert!(
            matches!(find(&all, "Actions minutes"), Allowance::Unavailable { reason, .. } if reason.contains("private"))
        );
        // One unverifiable allowance doesn't hide the others.
        assert!(matches!(find(&all, "Git LFS bandwidth"), Allowance::Verified { .. }));
    }

    #[test]
    fn storage_is_gb_held_on_average_over_the_month() {
        // October has 744 hours: 744 GB-hours is 1 GB held all month.
        let lines = r#"{"product":"actions","sku":"Actions storage","quantity":744,"unitType":"GigabyteHours","pricePerUnit":0.0003,
            "grossAmount":0,"discountAmount":0,"netAmount":0,"repositoryName":"secret"},
            {"product":"actions","sku":"Actions storage","quantity":372,"unitType":"GigabyteHours","pricePerUnit":0.0003,
            "grossAmount":0,"discountAmount":0,"netAmount":0,"repositoryName":"open"}"#;
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &summary(""),
            &detail(lines),
            &private(&[("secret", true), ("open", false)]),
        );
        assert!(
            matches!(find(&all, "Actions storage"), Allowance::Verified { used, .. } if (*used - 1.0).abs() < 1e-9)
        );
    }

    #[test]
    fn packages_show_only_a_verified_zero() {
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &summary(""), &[], &HashMap::new());
        assert!(matches!(find(&all, "Packages storage"), Allowance::Verified { used, .. } if *used == 0.0));
        let used = summary(
            r#"{"product":"Packages","sku":"packages_storage","grossQuantity":5,"grossAmount":0,"discountAmount":0,"netAmount":0,"unitType":"GigabyteHours"}"#,
        );
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &used, &[], &HashMap::new());
        assert!(matches!(find(&all, "Packages storage"), Allowance::Unavailable { .. }));
    }

    #[test]
    fn codespaces_compute_uses_each_skus_cores_and_billable_usage_must_fit() {
        // GitHub's ids and display names.
        assert_eq!(cores("codespaces_compute_d2"), Some(2.0));
        assert_eq!(cores("codespaces_compute_d32"), Some(32.0));
        assert_eq!(cores("Codespaces compute (16-core)"), Some(16.0));
        assert_eq!(cores("codespaces_compute_d"), None);
        assert_eq!(cores("codespaces_storage"), None);
        let compute = summary(
            r#"{"product":"Codespaces","sku":"codespaces_compute_d4","grossQuantity":10,"discountQuantity":10,"netQuantity":0,
               "grossAmount":3.6,"discountAmount":3.6,"netAmount":0,"unitType":"Hours"}"#,
        );
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &compute, &[], &HashMap::new());
        assert!(matches!(find(&all, "Codespaces compute"), Allowance::Verified { used, .. } if *used == 40.0));
        // Billed while the included core hours aren't used up: the evidence contradicts itself.
        let billed = summary(
            r#"{"product":"Codespaces","sku":"codespaces_compute_d4","grossQuantity":10,"discountQuantity":5,"netQuantity":5,
               "grossAmount":3.6,"discountAmount":1.8,"netAmount":1.8,"unitType":"Hours"}"#,
        );
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &billed, &[], &HashMap::new());
        assert!(matches!(
            find(&all, "Codespaces compute"),
            Allowance::Unavailable { .. }
        ));
        // Quantities that don't add up.
        let broken = summary(
            r#"{"product":"Git LFS","sku":"git_lfs_bandwidth","grossQuantity":10,"discountQuantity":2,"netQuantity":2,
               "grossAmount":0,"discountAmount":0,"netAmount":0,"unitType":"Gigabytes"}"#,
        );
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &broken, &[], &HashMap::new());
        assert!(
            matches!(find(&all, "Git LFS bandwidth"), Allowance::Unavailable { reason, .. } if reason.contains("add up"))
        );
        // Organizations have no Codespaces allowance.
        let team = plan("team", Scope::Organization).unwrap();
        let all = allowances(&team, Scope::Organization, "acme", &summary(""), &[], &HashMap::new());
        assert!(all.iter().all(|a| !matches!(a, Allowance::Verified { label, .. } | Allowance::Unavailable { label, .. } if label.starts_with("Codespaces"))));
    }

    #[test]
    fn allowances_show_used_included_and_left_or_over() {
        let start = "2026-10-01T00:00:00Z".parse().unwrap();
        let end = "2026-11-01T00:00:00Z".parse().unwrap();
        let now = "2026-10-13T00:00:00Z".parse().unwrap();
        let (metrics, messages) = show(
            &[
                Allowance::Verified {
                    label: "Actions minutes",
                    unit: "minutes",
                    used: 3_450.0,
                    included: 3_000.0,
                },
                Allowance::Verified {
                    label: "Git LFS storage",
                    unit: "GB",
                    used: 0.0,
                    included: 10.0,
                },
                Allowance::Unavailable {
                    label: "Packages storage",
                    reason: "GitHub doesn't say which package usage is free".into(),
                },
            ],
            start,
            end,
            now,
        );
        assert!(
            matches!(&metrics[0], Metric::Window { used, .. } if (*used - 1.15).abs() < 1e-9),
            "overage passes 100%"
        );
        assert_eq!(metrics[1].used_display(), "0%");
        assert_eq!(
            messages,
            [
                "Actions minutes: 3,450 minutes of 3,000 minutes included used (115%), 450 minutes over.",
                "Git LFS storage: 0 GB of 10 GB included used (0%), 10 GB left.",
                "Packages storage: not shown, GitHub doesn't say which package usage is free.",
            ]
        );
    }

    #[test]
    fn budgets_are_measured_only_where_this_owners_usage_can_show_them() {
        // The list's shape as GitHub returned it for an organization on 2026-10-12, plus the documented array form.
        let (budgets, more) = parse_budgets(
            r#"{"budgets":[
                {"budget_type":"ProductPricing","budget_product_sku":"actions","budget_scope":"organization","budget_amount":10,
                 "prevent_further_usage":true,"budget_entity_name":"acme","budget_alerting":{"will_alert":true}},
                {"budget_type":"ProductPricing","budget_product_skus":["actions"],"budget_scope":"repository","budget_amount":5,
                 "prevent_further_usage":false,"budget_entity_name":"acme/site","budget_alerting":{"will_alert":true}},
                {"budget_type":"SkuPricing","budget_product_skus":["actions_macos"],"budget_scope":"organization",
                 "budget_amount":3,"prevent_further_usage":false,"budget_alerting":{"will_alert":false}},
                {"budget_type":"ProductPricing","budget_product_sku":"copilot","budget_scope":"cost_center","budget_amount":50,
                 "prevent_further_usage":false,"budget_entity_name":"Platform"}],
              "has_next_page":true,"total_count":4}"#,
        )
        .unwrap();
        assert!(more, "another page follows");
        let summary = summary(
            r#"{"product":"Actions","sku":"actions_linux","grossQuantity":1,"grossAmount":4.25,"discountAmount":0,"netAmount":4.25},
               {"product":"Actions","sku":"actions_macos","grossQuantity":1,"grossAmount":1.24,"discountAmount":0,"netAmount":1.24}"#,
        );
        let detail = detail(
            r#"{"product":"actions","sku":"Actions Linux","quantity":1,"unitType":"Minutes","pricePerUnit":0.006,"grossAmount":1.5,
                "discountAmount":0,"netAmount":1.5,"repositoryName":"site","organizationName":"acme"}"#,
        );
        assert_eq!(
            budget_lines(&budgets, "acme", &summary, Some(&detail)),
            [
                "Budget for actions (organization acme): $5.49 of $10.00 spent this month, stops usage at the limit.",
                "Budget for actions (repository acme/site): $1.50 of $5.00 spent this month, alerts only.",
                "Budget for actions_macos (organization acme): $1.24 of $3.00 spent this month, neither stops usage nor alerts.",
                "Budget for copilot (cost_center Platform): $50.00, doesn't stop usage; this account's usage can't show what \
                 it has spent.",
            ]
        );
        assert_eq!(summary.currency, Some(Currency::Usd));
        // Without the detail report a repository budget's spend is unknown, never $0.00.
        assert_eq!(
            budget_lines(&budgets[1..2], "acme", &summary, None),
            [
                "Budget for actions (repository acme/site): $5.00, alerts only; this account's usage can't show what it has \
              spent."
            ]
        );
        assert!(parse_budgets(r#"{"budgets":[{"budget_product_skus":[1],"budget_amount":1}]}"#).is_err());
        // A bundle of every AI credit SKU, or a type GitHub adds later, is listed but never measured.
        let (bundles, _) = parse_budgets(
            r#"{"budgets":[
                {"budget_type":"BundlePricing","budget_product_sku":"ai_credits","budget_scope":"organization",
                 "budget_amount":25,"prevent_further_usage":true,"budget_entity_name":"acme"},
                {"budget_type":"SomethingNew","budget_product_sku":"actions","budget_scope":"organization",
                 "budget_amount":5,"prevent_further_usage":true,"budget_entity_name":"acme"}]}"#,
        )
        .unwrap();
        assert!(
            budget_lines(&bundles, "acme", &summary, Some(&detail))
                .iter()
                .all(|line| line.ends_with("this account's usage can't show what it has spent.")),
            "never a spend that misses the bundle's SKUs"
        );
    }

    #[test]
    fn cache_and_custom_image_storage_are_not_artifact_storage() {
        // 744 GB-hours over October is 1 GB held on average; the cache and image storage have allowances of their own.
        let storage = summary(
            r#"{"product":"Actions","sku":"actions_storage","grossQuantity":744,"discountQuantity":744,"netQuantity":0,
                "grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"unitType":"gigabyte-hours"},
               {"product":"Actions","sku":"actions_cache_storage","grossQuantity":5000,"discountQuantity":5000,"netQuantity":0,
                "grossAmount":1,"discountAmount":1,"netAmount":0,"unitType":"gigabyte-hours"}"#,
        );
        let lines = r#"{"product":"actions","sku":"Actions storage","quantity":744,"unitType":"GigabyteHours",
                "pricePerUnit":0.0003,"grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"repositoryName":"site"},
               {"product":"actions","sku":"Actions cache storage","quantity":5000,"unitType":"GigabyteHours",
                "pricePerUnit":0.0003,"grossAmount":1,"discountAmount":1,"netAmount":0,"repositoryName":"site"},
               {"product":"actions","sku":"Actions custom image storage","quantity":900,"unitType":"GigabyteHours",
                "pricePerUnit":0.0003,"grossAmount":1,"discountAmount":1,"netAmount":0,"repositoryName":"site"}"#;
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &storage,
            &detail(lines),
            &HashMap::from([("site".to_owned(), true)]),
        );
        assert!(
            all.iter().any(|a| matches!(a, Allowance::Verified { label: "Actions storage", used, .. } if (*used - 1.0).abs() < 1e-9)),
            "{all:?}"
        );
    }

    #[test]
    fn artifact_storage_shares_its_allowance_with_packages() {
        let shared = summary(
            r#"{"product":"Actions","sku":"actions_storage","grossQuantity":744,"discountQuantity":744,"netQuantity":0,
                "grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"unitType":"gigabyte-hours"},
               {"product":"Packages","sku":"packages_storage","grossQuantity":744,"discountQuantity":744,"netQuantity":0,
                "grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"unitType":"gigabyte-hours"}"#,
        );
        let lines = r#"{"product":"actions","sku":"Actions storage","quantity":744,"unitType":"GigabyteHours",
                "pricePerUnit":0.0003,"grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"repositoryName":"site"}"#;
        let visibility = HashMap::from([("site".to_owned(), true)]);
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &shared, &detail(lines), &visibility);
        assert!(
            all.iter().any(
                |a| matches!(a, Allowance::Unavailable { label: "Actions storage", reason }
                if reason.contains("shares its allowance with Packages storage"))
            ),
            "{all:?}"
        );
        // Billable shared storage means the pool is spent, Packages or not.
        let billed = summary(
            r#"{"product":"Actions","sku":"actions_storage","grossQuantity":744,"discountQuantity":744,"netQuantity":0,
                "grossAmount":0.25,"discountAmount":0.25,"netAmount":0,"unitType":"gigabyte-hours"},
               {"product":"Packages","sku":"packages_storage","grossQuantity":1488,"discountQuantity":744,"netQuantity":744,
                "grossAmount":0.5,"discountAmount":0.25,"netAmount":0.25,"unitType":"gigabyte-hours"}"#,
        );
        let all = allowances(&pro(), Scope::Personal, "HemSoft", &billed, &detail(lines), &visibility);
        assert!(
            all.iter().any(
                |a| matches!(a, Allowance::Verified { label: "Actions storage", used, included, .. }
                if (*used - (included + 1.0)).abs() < 1e-9)
            ),
            "{all:?}"
        );
    }

    #[test]
    fn an_unknown_runners_billable_minutes_leave_the_billed_total_unverified() {
        let mixed = summary(
            r#"{"product":"Actions","sku":"actions_linux","grossQuantity":9000,"discountQuantity":8000,"netQuantity":1000,
                "grossAmount":54,"discountAmount":48,"netAmount":6,"pricePerUnit":0.006,"unitType":"minutes"},
               {"product":"Actions","sku":"actions_future_runner","grossQuantity":100,"discountQuantity":0,"netQuantity":100,
                "grossAmount":1,"discountAmount":0,"netAmount":1,"pricePerUnit":0.01,"unitType":"minutes"}"#,
        );
        let lines = minutes("Actions future runner", 100.0, 0.01, "site");
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &mixed,
            &detail(&lines),
            &HashMap::new(),
        );
        assert!(
            matches!(&all[0], Allowance::Unavailable { label: "Actions minutes", reason } if reason.contains("doesn't know")),
            "{all:?}"
        );
    }

    #[test]
    fn visibility_is_remembered_per_repository() {
        // The cache is process-wide and github_billing's tests remember `acme/*` repositories in parallel, so this
        // test uses an owner nothing else touches; with `acme`, `acme/other` was sometimes already cached.
        remember("Visibility-Test", "Site", true);
        assert_eq!(remembered("visibility-test", "site"), Some(true));
        assert_eq!(remembered("visibility-test", "other"), None);
    }

    #[test]
    fn billable_usage_means_the_allowance_is_spent_whatever_the_repositories_are_now() {
        // Pro includes 3,000 minutes; GitHub billed 1,000 Linux minutes and 100 macOS minutes past them.
        let billed = summary(
            r#"{"product":"Actions","sku":"actions_linux","grossQuantity":9000,"discountQuantity":8000,"netQuantity":1000,
                "grossAmount":54,"discountAmount":48,"netAmount":6,"pricePerUnit":0.006,"unitType":"minutes"},
               {"product":"Actions","sku":"actions_macos","grossQuantity":500,"discountQuantity":400,"netQuantity":100,
                "grossAmount":31,"discountAmount":24.8,"netAmount":6.2,"pricePerUnit":0.062,"unitType":"minutes"},
               {"product":"Actions","sku":"actions_storage","grossQuantity":1488,"discountQuantity":744,"netQuantity":744,
                "grossAmount":0.5,"discountAmount":0.25,"netAmount":0.25,"pricePerUnit":0.0003,"unitType":"gigabyte-hours"}"#,
        );
        // Every repository looks public now (made public or moved since): visibility isn't needed.
        let lines = minutes("Actions Linux", 9000.0, 0.006, "was-private");
        let all = allowances(
            &pro(),
            Scope::Personal,
            "HemSoft",
            &billed,
            &detail(&lines),
            &private(&[("was-private", false)]),
        );
        let Allowance::Verified { used, .. } = find(&all, "Actions minutes") else {
            panic!("verified");
        };
        assert!(
            (used - (3_000.0 + 1_000.0 + 100.0 * 0.062 / 0.006)).abs() < 1e-6,
            "{used}"
        );
        // 744 billable GB-hours is one more GB held all October.
        assert!(
            matches!(find(&all, "Actions storage"), Allowance::Verified { used, .. } if (*used - 2.0).abs() < 1e-9)
        );
    }
}
