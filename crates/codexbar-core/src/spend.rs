//! What the Spend view adds up (#215): each account's spend this period and its prepaid balances, with totals kept
//! per currency, never converted.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

use crate::account::AccountSnapshot;
use crate::metric::{Currency, Metric, Money};

/// One account's spend: the metric its total counts and what it reports beside it.
#[derive(Debug)]
pub struct SpendRow<'a> {
    pub account: &'a AccountSnapshot,
    /// The account's first spend metric. Providers list their headline first (GitHub Billing's net spend), and
    /// the others break it down, so only this one counts toward a total.
    pub counted: &'a Metric,
    /// The account's other spend metrics, such as gross charges and discounts: shown, never added in.
    pub breakdown: Vec<&'a Metric>,
}

impl SpendRow<'_> {
    pub fn spent(&self) -> Money {
        spent(self.counted).unwrap_or_default()
    }
}

/// One prepaid balance.
#[derive(Debug)]
pub struct BalanceRow<'a> {
    pub account: &'a AccountSnapshot,
    pub metric: &'a Metric,
}

impl BalanceRow<'_> {
    pub fn remaining(&self) -> Money {
        match self.metric {
            Metric::Balance { remaining, .. } => *remaining,
            _ => Money::default(),
        }
    }

    /// Nothing left to spend.
    pub fn is_empty(&self) -> bool {
        self.remaining().cents() <= 0
    }

    /// When the balance runs out at its burn rate, counted from when it was fetched, so the date holds still while
    /// the same reading stays on screen; `None` when it is already empty or no rate is known.
    pub fn runs_out(&self) -> Option<DateTime<Utc>> {
        if self.is_empty() {
            return None;
        }
        let days = self.metric.days_of_credit()?;
        Some(self.account.fetched_at() + Duration::seconds((days * 86_400.0).round() as i64))
    }
}

/// Every account that reports money, in the order given.
#[derive(Debug)]
pub struct SpendSheet<'a> {
    pub spend: Vec<SpendRow<'a>>,
    pub balances: Vec<BalanceRow<'a>>,
}

impl<'a> SpendSheet<'a> {
    pub fn of(accounts: &'a [AccountSnapshot]) -> Self {
        let mut spend = Vec::new();
        let mut balances = Vec::new();
        for account in accounts {
            let mut spends = account.metrics().iter().filter(|metric| spent(metric).is_some());
            if let Some(counted) = spends.next() {
                spend.push(SpendRow {
                    account,
                    counted,
                    breakdown: spends.collect(),
                });
            }
            balances.extend(
                account
                    .metrics()
                    .iter()
                    .filter(|metric| matches!(metric, Metric::Balance { .. }))
                    .map(|metric| BalanceRow { account, metric }),
            );
        }
        Self { spend, balances }
    }

    /// No account reports spend or a balance.
    pub fn is_empty(&self) -> bool {
        self.spend.is_empty() && self.balances.is_empty()
    }

    /// The counted spend summed per currency, in currency order.
    pub fn spent(&self) -> Vec<Money> {
        totals(self.spend.iter().map(SpendRow::spent))
    }

    /// The prepaid balances summed per currency, in currency order. An overdrawn balance counts as nothing left:
    /// one provider's debt can't use up another's credit.
    pub fn left(&self) -> Vec<Money> {
        totals(self.balances.iter().map(|row| {
            let remaining = row.remaining();
            Money::new(remaining.cents().max(0), remaining.currency())
        }))
    }

    /// The balance that runs out first at its burn rate, and when.
    pub fn first_to_run_out(&self) -> Option<(&BalanceRow<'a>, DateTime<Utc>)> {
        self.balances
            .iter()
            .filter_map(|row| Some((row, row.runs_out()?)))
            .min_by_key(|(_, at)| *at)
    }

    /// The currencies spend is reported in, in currency order.
    pub fn currencies(&self) -> Vec<Currency> {
        self.spent().into_iter().map(Money::currency).collect()
    }
}

/// The amount a spend metric reports; `None` for any other metric.
pub fn spent(metric: &Metric) -> Option<Money> {
    match metric {
        Metric::Spend { spent, .. } => Some(*spent),
        _ => None,
    }
}

fn totals(amounts: impl Iterator<Item = Money>) -> Vec<Money> {
    let mut sums: BTreeMap<Currency, i64> = BTreeMap::new();
    for amount in amounts {
        *sums.entry(amount.currency()).or_default() += amount.cents();
    }
    sums.into_iter()
        .map(|(currency, minor)| Money::new(minor, currency))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountId, Provider};
    use crate::metric::Pace;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-19T12:00:00Z").unwrap().to_utc()
    }

    fn spend(label: &str, spent: Money, limit: Option<Money>) -> Metric {
        Metric::Spend {
            label: label.into(),
            spent,
            limit,
            resets_at: Some(now() + Duration::days(12)),
            pace: None,
        }
    }

    fn balance(label: &str, remaining: Money, burn_per_day: Option<Money>) -> Metric {
        Metric::Balance {
            label: label.into(),
            remaining,
            burn_per_day,
        }
    }

    fn account(id: &str, provider: Provider, metrics: Vec<Metric>) -> AccountSnapshot {
        AccountSnapshot::new(AccountId::new(id), provider, metrics, now())
    }

    fn eur(cents: i64) -> Money {
        Money::new(cents, Currency::Eur)
    }

    #[test]
    fn github_billing_counts_only_its_net_spend() {
        let accounts = [account(
            "github-billing",
            Provider::GitHubBilling,
            vec![
                spend("Net spend", Money::from_cents(2389), None),
                spend("Gross charges", Money::from_cents(60552), None),
                spend("Discounts", Money::from_cents(58163), None),
            ],
        )];
        let sheet = SpendSheet::of(&accounts);
        assert_eq!(sheet.spend.len(), 1);
        assert_eq!(sheet.spend[0].counted.label(), "Net spend");
        let breakdown: Vec<&str> = sheet.spend[0].breakdown.iter().map(|metric| metric.label()).collect();
        assert_eq!(breakdown, ["Gross charges", "Discounts"]);
        assert_eq!(sheet.spent(), [Money::from_cents(2389)]);
    }

    #[test]
    fn totals_are_kept_per_currency() {
        let accounts = [
            account(
                "cursor",
                Provider::Cursor,
                vec![spend(
                    "On-demand",
                    Money::from_cents(1240),
                    Some(Money::from_cents(5000)),
                )],
            ),
            account(
                "claude",
                Provider::Claude,
                vec![spend("Extra usage", eur(420), Some(eur(2000)))],
            ),
            account(
                "github-billing",
                Provider::GitHubBilling,
                vec![spend("Net spend", Money::from_cents(2389), None)],
            ),
            account(
                "openrouter",
                Provider::OpenRouter,
                vec![balance("Credits", Money::from_cents(1842), None)],
            ),
            account(
                "moonshot",
                Provider::Moonshot,
                vec![balance("Balance", Money::from_cents(785), None)],
            ),
            // Overdrawn: shown as empty, and it takes nothing from the others' credit.
            account(
                "zen",
                Provider::OpenCode,
                vec![balance("Zen balance", Money::from_cents(-1000), None)],
            ),
        ];
        let sheet = SpendSheet::of(&accounts);
        assert_eq!(sheet.spent(), [Money::from_cents(3629), eur(420)]);
        assert_eq!(sheet.left(), [Money::from_cents(2627)]);
        assert!(sheet.balances[2].is_empty());
        assert_eq!(sheet.currencies(), [Currency::Usd, Currency::Eur]);
    }

    #[test]
    fn accounts_without_money_are_left_out() {
        let accounts = [account(
            "codex",
            Provider::Codex,
            vec![Metric::Window {
                label: "Weekly".into(),
                used: 0.4,
                resets_at: now() + Duration::days(3),
                pace: Some(Pace::per_day(0.1)),
                length: None,
            }],
        )];
        let sheet = SpendSheet::of(&accounts);
        assert!(sheet.is_empty());
        assert!(sheet.spent().is_empty());
        assert!(sheet.left().is_empty());
        assert!(sheet.first_to_run_out().is_none());
    }

    #[test]
    fn a_balance_runs_out_at_its_burn_rate() {
        let accounts = [
            account(
                "openrouter",
                Provider::OpenRouter,
                vec![balance(
                    "Credits",
                    Money::from_cents(1842),
                    Some(Money::from_cents(310)),
                )],
            ),
            account(
                "moonshot",
                Provider::Moonshot,
                vec![balance("Balance", Money::from_cents(785), Some(Money::from_cents(400)))],
            ),
            // No rate, and an empty balance, never run out on a date.
            account(
                "zen",
                Provider::OpenCode,
                vec![balance("Zen balance", Money::from_cents(500), None)],
            ),
            account(
                "x",
                Provider::XSubscription,
                vec![balance(
                    "Extra usage credits",
                    Money::from_cents(0),
                    Some(Money::from_cents(100)),
                )],
            ),
        ];
        let sheet = SpendSheet::of(&accounts);
        let openrouter = sheet.balances[0].runs_out().unwrap();
        // $18.42 at $3.10 a day: 5.94 days.
        assert_eq!((openrouter - now()).num_hours(), 142);
        assert_eq!(sheet.balances[2].runs_out(), None);
        assert!(sheet.balances[3].is_empty());
        assert_eq!(sheet.balances[3].runs_out(), None);
        let (first, at) = sheet.first_to_run_out().unwrap();
        assert_eq!(first.account.id().as_str(), "moonshot");
        assert_eq!((at - now()).num_hours(), 47);
    }
}
