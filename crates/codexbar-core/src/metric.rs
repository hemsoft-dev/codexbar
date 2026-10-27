use chrono::{DateTime, Duration, Utc};

/// The currency a provider bills in (#75). Claude bills extra usage in the account's local currency (#82).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Currency {
    #[default]
    Usd,
    Eur,
    Gbp,
    Cny,
    Jpy,
    Cad,
    Aud,
    Nzd,
    Sgd,
    Hkd,
    Chf,
    Inr,
    Brl,
    Mxn,
    Krw,
    Sek,
    Nok,
    Dkk,
    Pln,
}

impl Currency {
    pub const ALL: [Self; 19] = [
        Self::Usd,
        Self::Eur,
        Self::Gbp,
        Self::Cny,
        Self::Jpy,
        Self::Cad,
        Self::Aud,
        Self::Nzd,
        Self::Sgd,
        Self::Hkd,
        Self::Chf,
        Self::Inr,
        Self::Brl,
        Self::Mxn,
        Self::Krw,
        Self::Sek,
        Self::Nok,
        Self::Dkk,
        Self::Pln,
    ];

    /// The ISO 4217 code ("USD").
    pub fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Gbp => "GBP",
            Self::Cny => "CNY",
            Self::Jpy => "JPY",
            Self::Cad => "CAD",
            Self::Aud => "AUD",
            Self::Nzd => "NZD",
            Self::Sgd => "SGD",
            Self::Hkd => "HKD",
            Self::Chf => "CHF",
            Self::Inr => "INR",
            Self::Brl => "BRL",
            Self::Mxn => "MXN",
            Self::Krw => "KRW",
            Self::Sek => "SEK",
            Self::Nok => "NOK",
            Self::Dkk => "DKK",
            Self::Pln => "PLN",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|currency| currency.code().eq_ignore_ascii_case(code))
    }

    /// What precedes an amount: a symbol, or the code and a space where symbols are ambiguous.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Usd => "$",
            Self::Eur => "€",
            Self::Gbp => "£",
            Self::Cny => "CN¥",
            Self::Jpy => "¥",
            Self::Cad => "CA$",
            Self::Aud => "A$",
            Self::Nzd => "NZ$",
            Self::Sgd => "S$",
            Self::Hkd => "HK$",
            Self::Chf => "CHF ",
            Self::Inr => "₹",
            Self::Brl => "R$",
            Self::Mxn => "MX$",
            Self::Krw => "₩",
            Self::Sek => "SEK ",
            Self::Nok => "NOK ",
            Self::Dkk => "DKK ",
            Self::Pln => "PLN ",
        }
    }

    /// Digits after the decimal point in its minor unit (yen and won have none).
    pub fn minor_digits(self) -> u32 {
        match self {
            Self::Jpy | Self::Krw => 0,
            _ => 2,
        }
    }
}

/// An amount of money in its currency's minor units (cents) so totals never pick up floating-point drift.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money {
    cents: i64,
    currency: Currency,
}

impl Money {
    /// US dollars and cents.
    pub const fn from_cents(cents: i64) -> Self {
        Self {
            cents,
            currency: Currency::Usd,
        }
    }

    /// An amount in `currency`'s minor units.
    pub const fn new(minor: i64, currency: Currency) -> Self {
        Self { cents: minor, currency }
    }

    /// The amount in minor units (cents for dollars).
    pub const fn cents(self) -> i64 {
        self.cents
    }

    pub const fn currency(self) -> Currency {
        self.currency
    }

    /// The amount in major units (dollars), for history and charts.
    pub fn major(self) -> f64 {
        self.cents as f64 / 10f64.powi(self.currency.minor_digits() as i32)
    }

    /// `self - other` in the same currency; `None` across currencies.
    pub fn minus(self, other: Self) -> Option<Self> {
        (self.currency == other.currency).then(|| Self::new(self.cents - other.cents, self.currency))
    }

    /// "$18.42", "€3.10", "¥1200". Locale-aware formatting arrives with issue #84.
    pub fn display(self) -> String {
        let sign = if self.cents < 0 { "-" } else { "" };
        let abs = self.cents.unsigned_abs();
        let symbol = self.currency.symbol();
        match self.currency.minor_digits() {
            0 => format!("{sign}{symbol}{abs}"),
            digits => {
                let scale = 10u64.pow(digits);
                format!(
                    "{sign}{symbol}{}.{:0width$}",
                    abs / scale,
                    abs % scale,
                    width = digits as usize
                )
            }
        }
    }
}

/// How fast a limit is being consumed, as a fraction of the whole limit per hour.
/// How a limit's pace plays out by its reset (#206).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outlook {
    /// The share of the limit used by the reset, at most the whole of it.
    Share(f64),
    /// The limit runs out this long before the reset.
    Early(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    fraction_per_hour: f64,
}

impl Pace {
    pub fn per_hour(fraction_per_hour: f64) -> Self {
        Self {
            fraction_per_hour: fraction_per_hour.max(0.0),
        }
    }

    pub fn per_day(fraction_per_day: f64) -> Self {
        Self::per_hour(fraction_per_day / 24.0)
    }

    pub fn fraction_per_hour(self) -> f64 {
        self.fraction_per_hour
    }
}

/// The longest window a provider can plausibly report: a year, with a day to spare.
const MAX_WINDOW_DAYS: i64 = 366;

/// A window length from a count of seconds read from outside (a provider's payload, a saved file), or `None` when it
/// isn't a plausible window (#226): zero or less, or longer than a year, which also keeps `Duration::seconds` and the
/// date arithmetic on the window's start from overflowing.
pub fn window_length_from_seconds(seconds: i64) -> Option<Duration> {
    (seconds > 0 && seconds <= MAX_WINDOW_DAYS * 24 * 3600).then(|| Duration::seconds(seconds))
}

/// One limit an account reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Metric {
    /// A rolling or calendar window measured as a fraction used, such as a 5-hour or weekly window.
    Window {
        label: String,
        used: f64,
        resets_at: DateTime<Utc>,
        pace: Option<Pace>,
        /// How long one window lasts, when the provider says (#223): five hours, a week. `None` when unknown.
        length: Option<Duration>,
    },
    /// A counted allowance, such as premium requests per month.
    Quota {
        label: String,
        used: u64,
        limit: u64,
        resets_at: DateTime<Utc>,
        pace: Option<Pace>,
        /// How long one period lasts, when the provider says (#223). `None` when unknown.
        length: Option<Duration>,
    },
    /// Money spent in a period, optionally against a spend limit (#75): a budget or a monthly cap.
    Spend {
        label: String,
        spent: Money,
        limit: Option<Money>,
        /// When the period starts over, if it does.
        resets_at: Option<DateTime<Utc>>,
        /// How fast the limit is being used, as a fraction of it per hour; only meaningful with a limit.
        pace: Option<Pace>,
    },
    /// Prepaid credit that runs down with spend.
    Balance {
        label: String,
        remaining: Money,
        burn_per_day: Option<Money>,
    },
    /// A count of activity with no limit, no reset and no balance (#103): Greptile's completed reviews. It is
    /// never drawn as a bar or a percentage, and it raises no alert.
    Activity { label: String, count: u64 },
    /// A prepaid pool counted in the provider's own credits, never money (#150): Codex's credits.
    Credits {
        label: String,
        /// Credits left; `None` when the pool is unlimited.
        balance: Option<f64>,
        /// The balance the pool started from, remembered since the last top-up; the bar runs down from it.
        start: Option<f64>,
    },
}

impl Metric {
    pub fn label(&self) -> &str {
        match self {
            Self::Window { label, .. }
            | Self::Quota { label, .. }
            | Self::Spend { label, .. }
            | Self::Balance { label, .. }
            | Self::Activity { label, .. }
            | Self::Credits { label, .. } => label,
        }
    }

    /// Stable identity of this metric within its account ("5-hour-window", "weekly", "credits"), for history.
    pub fn key(&self) -> String {
        let mut key = String::new();
        for ch in self.label().chars() {
            if ch.is_alphanumeric() {
                key.extend(ch.to_lowercase());
            } else if !key.ends_with('-') && !key.is_empty() {
                key.push('-');
            }
        }
        key.trim_end_matches('-').to_owned()
    }

    /// The value history stores: fraction used for limits, money in major units for balances and spend, and the
    /// count for credits. Spend is always money, capped or not, so adding or removing a cap doesn't mix units in one
    /// series. Unlimited credits have no value to store.
    pub fn history_value(&self) -> Option<f64> {
        match self {
            Self::Balance { remaining, .. } => Some(remaining.major()),
            Self::Credits { balance, .. } => *balance,
            Self::Activity { count, .. } => Some(*count as f64),
            Self::Spend { spent, .. } => Some(spent.major()),
            _ => self.used_fraction(),
        }
    }

    /// What is left under a spend limit, in its currency.
    pub fn headroom(&self) -> Option<Money> {
        match self {
            Self::Spend {
                spent,
                limit: Some(limit),
                ..
            } => limit.minus(*spent),
            _ => None,
        }
    }

    /// Fraction of the limit used, clamped to 0..=1. Balances have no fixed limit and return `None`.
    pub fn used_fraction(&self) -> Option<f64> {
        match self {
            Self::Window { used, .. } => Some(used.clamp(0.0, 1.0)),
            Self::Quota { used, limit, .. } if *limit > 0 => Some((*used as f64 / *limit as f64).clamp(0.0, 1.0)),
            Self::Spend {
                spent,
                limit: Some(limit),
                ..
            } if limit.cents() > 0 && limit.currency() == spent.currency() => {
                Some((spent.cents() as f64 / limit.cents() as f64).clamp(0.0, 1.0))
            }
            Self::Quota { .. }
            | Self::Spend { .. }
            | Self::Balance { .. }
            | Self::Activity { .. }
            | Self::Credits { .. } => None,
        }
    }

    /// How full the metric's bar is, 0..=1: the fraction used for limits, and for credits how much of the remembered
    /// starting balance is spent. `None` when there is nothing to draw against (unlimited credits, no start yet).
    pub fn progress(&self) -> Option<f64> {
        match self {
            Self::Credits {
                balance: Some(balance),
                start: Some(start),
                ..
            } if *start > 0.0 => Some((1.0 - balance / start).clamp(0.0, 1.0)),
            Self::Credits { .. } => None,
            _ => self.used_fraction(),
        }
    }

    /// True when the limit is fully used.
    pub fn is_exhausted(&self) -> bool {
        self.used_fraction().is_some_and(|used| used >= 1.0)
    }

    /// True for credits that can still be spent: a positive balance, or an unlimited pool.
    pub fn has_spendable_credits(&self) -> bool {
        match self {
            Self::Credits { balance, .. } => balance.is_none_or(|balance| balance > 0.0),
            _ => false,
        }
    }

    pub fn resets_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Window { resets_at, .. } | Self::Quota { resets_at, .. } => Some(*resets_at),
            Self::Spend { resets_at, .. } => *resets_at,
            Self::Balance { .. } | Self::Activity { .. } | Self::Credits { .. } => None,
        }
    }

    /// How long one window or period lasts, when the provider reports it (#223).
    pub fn window_length(&self) -> Option<Duration> {
        match self {
            Self::Window { length, .. } | Self::Quota { length, .. } => *length,
            _ => None,
        }
    }

    /// Time until the limit is exhausted at the current pace, when the pace is known.
    pub fn time_to_limit(&self) -> Option<Duration> {
        let (used, pace) = match self {
            Self::Window { pace: Some(pace), .. }
            | Self::Quota { pace: Some(pace), .. }
            | Self::Spend { pace: Some(pace), .. } => (self.used_fraction()?, *pace),
            _ => return None,
        };
        if pace.fraction_per_hour() <= 0.0 {
            return None;
        }
        let hours = (1.0 - used) / pace.fraction_per_hour();
        Some(Duration::seconds((hours * 3600.0).round() as i64))
    }

    /// The fraction of the limit used by the reset if the current pace holds (#83): above 1 when it runs out first.
    /// `None` without a pace, a limit or a reset still ahead, so nothing is projected from too little information.
    pub fn projected_at_reset(&self, now: DateTime<Utc>) -> Option<f64> {
        // Windows retain the source's raw share above 100%; counted quotas keep their existing capped share.
        let (used, pace) = match self {
            Self::Window {
                used, pace: Some(pace), ..
            } => (*used, *pace),
            Self::Quota { pace: Some(pace), .. } | Self::Spend { pace: Some(pace), .. } => {
                (self.used_fraction()?, *pace)
            }
            _ => return None,
        };
        let left = self.resets_at()? - now;
        if left <= Duration::zero() || !pace.fraction_per_hour().is_finite() || pace.fraction_per_hour() < 0.0 {
            return None;
        }
        let projected = used + pace.fraction_per_hour() * left.num_seconds() as f64 / 3600.0;
        projected.is_finite().then_some(projected)
    }

    /// How the pace plays out by the reset (#206): the share it reaches, or how early the limit runs out. A limit
    /// hit at the reset, or within a minute of it, is a full share rather than "0m early".
    pub fn outlook(&self, now: DateTime<Utc>) -> Option<Outlook> {
        let projected = self.projected_at_reset(now)?;
        if let (Some(to_limit), Some(resets_at)) = (self.time_to_limit(), self.resets_at()) {
            let early = resets_at - (now + to_limit);
            if early >= Duration::minutes(1) {
                return Some(Outlook::Early(early));
            }
        }
        Some(Outlook::Share(projected.min(1.0)))
    }

    /// True when the current pace exhausts the limit before it resets.
    pub fn exhausts_before_reset(&self, now: DateTime<Utc>) -> bool {
        match (self.time_to_limit(), self.resets_at()) {
            (Some(to_limit), Some(resets_at)) => now + to_limit < resets_at,
            _ => false,
        }
    }

    /// Days of credit left at the current burn rate.
    pub fn days_of_credit(&self) -> Option<f64> {
        match self {
            Self::Balance {
                remaining,
                burn_per_day: Some(burn),
                ..
            } if burn.cents() > 0 => Some(remaining.cents() as f64 / burn.cents() as f64),
            _ => None,
        }
    }

    /// The value column ("91%", "1,284", "$18.42 left").
    pub fn used_display(&self) -> String {
        match self {
            Self::Window { .. } => format!("{:.0}%", self.used_fraction().unwrap_or_default() * 100.0),
            Self::Quota { used, .. } => group_thousands(*used),
            Self::Spend {
                spent,
                limit: Some(limit),
                ..
            } => format!("{} of {}", spent.display(), limit.display()),
            Self::Spend { spent, limit: None, .. } => format!("{} spent", spent.display()),
            Self::Balance { remaining, .. } => format!("{} left", remaining.display()),
            Self::Credits {
                balance: Some(balance), ..
            } => format!("{} left", format_credits(*balance)),
            Self::Credits { balance: None, .. } => "Unlimited".to_owned(),
            Self::Activity { count, .. } => group_thousands(*count),
        }
    }
}

/// A credit count: "62,500", or two decimals when the balance has a fraction ("1,234.50").
pub fn format_credits(count: f64) -> String {
    let hundredths = (count.abs() * 100.0).round() as u64;
    let sign = if count < 0.0 && hundredths > 0 { "-" } else { "" };
    let whole = group_thousands(hundredths / 100);
    match hundredths % 100 {
        0 => format!("{sign}{whole}"),
        cents => format!("{sign}{whole}.{cents:02}"),
    }
}

/// "1284" -> "1,284".
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_window_length_must_be_plausible() {
        use super::window_length_from_seconds;
        assert_eq!(window_length_from_seconds(18_000), Some(Duration::hours(5)));
        assert_eq!(window_length_from_seconds(366 * 24 * 3600), Some(Duration::days(366)));
        for bad in [0, -60, 366 * 24 * 3600 + 1, i64::MAX / 1000, i64::MAX] {
            assert_eq!(window_length_from_seconds(bad), None, "{bad}");
        }
    }

    use super::*;

    mod outlook {
        use super::*;

        fn at(used: f64, hours_left: f64, per_hour: Option<f64>) -> Option<Outlook> {
            let now: DateTime<Utc> = "2026-10-19T08:00:00Z".parse().unwrap();
            let metric = Metric::Window {
                label: "5-hour window".into(),
                used,
                resets_at: now + Duration::seconds((hours_left * 3600.0) as i64),
                pace: per_hour.map(Pace::per_hour),
                length: None,
            };
            metric.outlook(now)
        }

        #[test]
        fn a_pace_that_runs_out_before_the_reset_says_how_early() {
            // 91% used, 0.2/h: the limit in 27m, the reset in 60m.
            assert_eq!(at(0.91, 1.0, Some(0.2)), Some(Outlook::Early(Duration::minutes(33))));
            // 50% used, 0.2/day over five days: the limit in 2.5 days.
            assert_eq!(
                at(0.5, 120.0, Some(0.2 / 24.0)),
                Some(Outlook::Early(Duration::hours(60)))
            );
        }

        #[test]
        fn a_pace_that_lands_on_the_reset_is_a_full_share_not_zero_early() {
            // 50% used, 0.5/h, one hour left: exactly at the reset.
            assert_eq!(at(0.5, 1.0, Some(0.5)), Some(Outlook::Share(1.0)));
            // Thirty seconds early rounds into the reset too.
            assert!(matches!(at(0.5, 1.0 + 30.0 / 3600.0, Some(0.5)), Some(Outlook::Share(share)) if share >= 1.0));
        }

        #[test]
        fn a_pace_under_the_limit_is_its_share() {
            assert!(matches!(at(0.35, 48.0, Some(0.01)), Some(Outlook::Share(share)) if (share - 0.83).abs() < 1e-9));
        }

        #[test]
        fn no_pace_is_no_outlook() {
            assert_eq!(at(0.35, 48.0, None), None);
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    #[test]
    fn money_display_negative_and_cents_formats_with_sign() {
        assert_eq!(Money::from_cents(1842).display(), "$18.42");
        assert_eq!(Money::from_cents(-905).display(), "-$9.05");
        assert_eq!(Money::from_cents(7).display(), "$0.07");
    }

    #[test]
    fn key_label_variants_slugify_stably() {
        let window = |label: &str| Metric::Window {
            label: label.into(),
            used: 0.1,
            resets_at: now(),
            pace: None,
            length: None,
        };
        assert_eq!(window("5-hour window").key(), "5-hour-window");
        assert_eq!(window("Weekly").key(), "weekly");
        assert_eq!(window("  Go usage! ").key(), "go-usage");
    }

    #[test]
    fn history_value_balance_uses_dollars() {
        let balance = Metric::Balance {
            label: "Credits".into(),
            remaining: Money::from_cents(1842),
            burn_per_day: None,
        };
        assert_eq!(balance.history_value(), Some(18.42));
    }

    #[test]
    fn group_thousands_large_values_inserts_commas() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(612), "612");
        assert_eq!(group_thousands(1284), "1,284");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn used_fraction_quota_without_limit_returns_none() {
        let metric = Metric::Quota {
            label: "x".into(),
            used: 5,
            limit: 0,
            resets_at: now(),
            pace: None,
            length: None,
        };
        assert_eq!(metric.used_fraction(), None);
    }

    #[test]
    fn exhausts_before_reset_fast_pace_returns_true() {
        let metric = Metric::Window {
            label: "5-hour window".into(),
            used: 0.91,
            resets_at: now() + Duration::minutes(38),
            pace: Some(Pace::per_hour(0.216)),
            length: None,
        };
        let to_limit = metric.time_to_limit().unwrap();
        assert_eq!(to_limit.num_minutes(), 25);
        assert!(metric.exhausts_before_reset(now()));
    }

    #[test]
    fn projected_at_reset_adds_the_pace_until_the_reset() {
        let metric = Metric::Window {
            label: "Monthly".into(),
            used: 0.4,
            resets_at: now() + Duration::hours(10),
            pace: Some(Pace::per_hour(0.03)),
            length: None,
        };
        assert!((metric.projected_at_reset(now()).unwrap() - 0.7).abs() < 1e-9);
        // A pace that runs out first projects past the limit.
        let fast = Metric::Window {
            label: "Monthly".into(),
            used: 0.4,
            resets_at: now() + Duration::hours(10),
            pace: Some(Pace::per_hour(0.1)),
            length: None,
        };
        assert!((fast.projected_at_reset(now()).unwrap() - 1.4).abs() < 1e-9);
    }

    #[test]
    fn projected_at_reset_needs_pace_limit_and_a_future_reset() {
        let window = |resets_at, pace| Metric::Window {
            label: "Monthly".into(),
            used: 0.4,
            resets_at,
            pace,
            length: None,
        };
        assert_eq!(window(now() + Duration::hours(1), None).projected_at_reset(now()), None);
        let pace = Some(Pace::per_hour(0.1));
        assert_eq!(window(now(), pace).projected_at_reset(now()), None, "resetting now");
        assert_eq!(
            window(now() - Duration::hours(1), pace).projected_at_reset(now()),
            None,
            "expired"
        );
        let uncapped = Metric::Spend {
            label: "Spend".into(),
            spent: Money::from_cents(500),
            limit: None,
            resets_at: Some(now() + Duration::hours(5)),
            pace,
        };
        assert_eq!(uncapped.projected_at_reset(now()), None, "no limit to project against");
    }

    #[test]
    fn spend_with_a_pace_can_exhaust_before_reset() {
        let spend = Metric::Spend {
            label: "On-demand".into(),
            spent: Money::from_cents(1500),
            limit: Some(Money::from_cents(2000)),
            resets_at: Some(now() + Duration::hours(10)),
            pace: Some(Pace::per_hour(0.05)),
        };
        assert_eq!(spend.time_to_limit(), Some(Duration::hours(5)));
        assert!(spend.exhausts_before_reset(now()));
    }

    #[test]
    fn exhausts_before_reset_without_pace_returns_false() {
        let metric = Metric::Window {
            label: "Weekly".into(),
            used: 0.99,
            resets_at: now() + Duration::days(1),
            pace: None,
            length: None,
        };
        assert!(!metric.exhausts_before_reset(now()));
    }

    #[test]
    fn days_of_credit_with_burn_divides_remaining() {
        let metric = Metric::Balance {
            label: "Credits".into(),
            remaining: Money::from_cents(1842),
            burn_per_day: Some(Money::from_cents(310)),
        };
        assert!((metric.days_of_credit().unwrap() - 5.94).abs() < 0.01);
        assert_eq!(metric.used_display(), "$18.42 left");
    }
}

#[cfg(test)]
mod credit_tests {
    use super::*;

    fn credits(balance: Option<f64>, start: Option<f64>) -> Metric {
        Metric::Credits {
            label: "Credits".into(),
            balance,
            start,
        }
    }

    #[test]
    fn credits_are_a_count_never_a_percentage() {
        let metric = credits(Some(62_500.0), Some(100_000.0));
        assert_eq!(metric.used_fraction(), None);
        assert_eq!(metric.used_display(), "62,500 left");
        assert_eq!(metric.history_value(), Some(62_500.0));
        assert_eq!(metric.key(), "credits");
        assert_eq!(metric.resets_at(), None);
        assert!((metric.progress().unwrap() - 0.375).abs() < 1e-9);
    }

    #[test]
    fn credits_without_a_start_or_unlimited_have_no_bar() {
        assert_eq!(credits(Some(10.0), None).progress(), None);
        assert_eq!(credits(Some(10.0), Some(0.0)).progress(), None);
        let unlimited = credits(None, None);
        assert_eq!(unlimited.progress(), None);
        assert_eq!(unlimited.history_value(), None);
        assert_eq!(unlimited.used_display(), "Unlimited");
    }

    #[test]
    fn spendable_credits_need_a_positive_or_unlimited_balance() {
        assert!(credits(Some(0.5), None).has_spendable_credits());
        assert!(credits(None, None).has_spendable_credits());
        assert!(!credits(Some(0.0), None).has_spendable_credits());
    }

    #[test]
    fn format_credits_groups_and_keeps_fractions() {
        assert_eq!(format_credits(62_500.0), "62,500");
        assert_eq!(format_credits(1234.5), "1,234.50");
        assert_eq!(format_credits(0.0), "0");
    }
}

#[cfg(test)]
mod money_tests {
    use super::*;

    #[test]
    fn money_displays_in_its_currency() {
        assert_eq!(Money::from_cents(1842).display(), "$18.42");
        assert_eq!(Money::new(-310, Currency::Eur).display(), "-€3.10");
        assert_eq!(Money::new(1200, Currency::Jpy).display(), "¥1200");
        assert_eq!(Money::new(5, Currency::Cny).display(), "CN¥0.05");
        assert_eq!(Money::new(1200, Currency::Jpy).major(), 1200.0);
        assert_eq!(Currency::from_code("eur"), Some(Currency::Eur));
    }

    #[test]
    fn spend_reports_fraction_headroom_and_display() {
        let capped = Metric::Spend {
            label: "Monthly spend".into(),
            spent: Money::from_cents(1240),
            limit: Some(Money::from_cents(5000)),
            resets_at: None,
            pace: None,
        };
        assert!((capped.used_fraction().unwrap() - 0.248).abs() < 1e-9);
        assert_eq!(capped.headroom(), Some(Money::from_cents(3760)));
        assert_eq!(capped.used_display(), "$12.40 of $50.00");
        assert_eq!(capped.key(), "monthly-spend");
        assert_eq!(capped.history_value(), Some(12.4), "money, like uncapped spend");
        let open = Metric::Spend {
            label: "Spend".into(),
            spent: Money::new(990, Currency::Eur),
            limit: None,
            resets_at: None,
            pace: None,
        };
        assert_eq!(open.used_fraction(), None);
        assert_eq!(open.history_value(), Some(9.9));
        assert_eq!(open.used_display(), "€9.90 spent");
        // Different currencies never mix.
        let mixed = Metric::Spend {
            label: "Spend".into(),
            spent: Money::new(100, Currency::Eur),
            limit: Some(Money::from_cents(500)),
            resets_at: None,
            pace: None,
        };
        assert_eq!((mixed.used_fraction(), mixed.headroom()), (None, None));
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn an_activity_count_has_no_fraction_bar_reset_or_alert() {
        let reviews = Metric::Activity {
            label: "Completed reviews".into(),
            count: 1_284,
        };
        assert_eq!(reviews.used_display(), "1,284");
        assert_eq!(reviews.used_fraction(), None);
        assert_eq!(reviews.progress(), None);
        assert_eq!(reviews.resets_at(), None);
        assert!(!reviews.is_exhausted());
        assert_eq!(reviews.history_value(), Some(1_284.0));
        assert_eq!(reviews.key(), "completed-reviews");
        let now = "2026-10-12T12:00:00Z".parse().unwrap();
        assert_eq!(reviews.projected_at_reset(now), None);
        let assessment = crate::assess(&reviews, now);
        assert_eq!(assessment.severity(), crate::Severity::Normal);
        assert_eq!(assessment.label(), None, "no pill");
    }
}
