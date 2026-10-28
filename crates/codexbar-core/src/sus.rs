//! Weekly Sub Utilization Score. The projection and exhaustion time are the metric's existing forecast.
use crate::{AccountSnapshot, Metric, Provider};
use chrono::{DateTime, Duration, Utc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Green,
    Yellow,
    Red,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Score(u8);
impl Score {
    pub fn value(self) -> u8 {
        self.0
    }
    pub fn from_value(value: u8) -> Option<Self> {
        (value <= 100).then_some(Self(value))
    }
    pub fn band(self) -> Band {
        match self.0 {
            90..=100 => Band::Green,
            75..=89 => Band::Yellow,
            _ => Band::Red,
        }
    }
    fn rounded(value: f64) -> Self {
        Self(value.clamp(0., 100.).round_ties_even() as u8)
    }
}

/// The Python reference rounds ties to even. Missing pre-reset exhaustion means no lockout penalty.
pub fn calculate(projected_percent: f64, out_hours: Option<f64>, reset_hours: f64, length_hours: f64) -> Option<Score> {
    if !projected_percent.is_finite()
        || projected_percent < 0.
        || !reset_hours.is_finite()
        || reset_hours <= 0.
        || !length_hours.is_finite()
        || length_hours <= 0.
    {
        return None;
    }
    let value = if projected_percent <= 100. {
        projected_percent
    } else {
        let dark = out_hours
            .filter(|out| out.is_finite())
            .map_or(0., |out| (reset_hours - out).max(0.));
        100. - 100. * dark / length_hours
    };
    Some(Score::rounded(value))
}
/// Minimum positive elapsed time for an early SUS. Early values are display-only, without risk or color.
pub const EARLY_SUS: Duration = Duration::milliseconds(1);
/// Weekly forecasts and SUS firm up together at one hour.
pub const WEEKLY_MATURITY: Duration = Duration::hours(1);

/// A weekly score as shown: early from [`EARLY_SUS`], firm from [`WEEKLY_MATURITY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Early(Score),
    Firm(Score),
}
impl Reading {
    pub fn is_early(self) -> bool {
        matches!(self, Self::Early(_))
    }
    /// "SUS 84", or "SUS 84 early".
    pub fn text(self) -> String {
        match self {
            Self::Early(score) => format!("SUS {} early", score.value()),
            Self::Firm(score) => format!("SUS {}", score.value()),
        }
    }
}

/// A provider week: seven days, or a calendar week that crosses an offset change.
pub fn is_week_length(length: Duration) -> bool {
    (Duration::days(6)..=Duration::days(8)).contains(&length)
}
pub fn is_weekly(metric: &Metric) -> bool {
    let declared_weekly = matches!(metric, Metric::Window { .. } | Metric::Quota { .. })
        && metric
            .label()
            .to_ascii_lowercase()
            .split_whitespace()
            .any(|part| part == "weekly");
    match metric.window_length() {
        Some(length) if length == Duration::days(7) => true,
        // Provider calendar weeks can cross offset changes. Keep their actual length for age and lockout math;
        // the weekly label is required outside exactly seven days, and short/monthly periods stay excluded.
        Some(length) => declared_weekly && is_week_length(length),
        None => declared_weekly,
    }
}
fn hours(d: Duration) -> f64 {
    d.num_milliseconds() as f64 / 3_600_000.
}
/// A weekly metric's length, reset and age, or `None` when it isn't weekly or they are unknown.
fn week(metric: &Metric, now: DateTime<Utc>) -> Option<(Duration, DateTime<Utc>, Duration)> {
    if !is_weekly(metric) {
        return None;
    }
    let length = metric.window_length()?;
    let reset = metric.resets_at()?;
    Some((
        length,
        reset,
        now.signed_duration_since(reset.checked_sub_signed(length)?),
    ))
}
fn score(metric: &Metric, now: DateTime<Utc>, length: Duration, reset: DateTime<Utc>) -> Option<Score> {
    calculate(
        metric.projected_at_reset(now)? * 100.,
        metric.time_to_limit_at(now).map(hours),
        hours(reset - now),
        hours(length),
    )
}
/// The firm score: the metric's own forecast, from [`WEEKLY_MATURITY`]. Only firm scores may color or rank.
pub fn weekly(metric: &Metric, now: DateTime<Utc>) -> Option<Score> {
    let (length, reset, age) = week(metric, now)?;
    if age < WEEKLY_MATURITY {
        return None;
    }
    score(metric, now, length, reset)
}
/// The early score from [`EARLY_SUS`] until [`WEEKLY_MATURITY`]: usage so far over the time elapsed, projected to
/// the reset. The score remains display-only and raises no forecast risk.
pub fn early(metric: &Metric, now: DateTime<Utc>) -> Option<Score> {
    let (length, reset, age) = week(metric, now)?;
    if !(EARLY_SUS..WEEKLY_MATURITY).contains(&age) {
        return None;
    }
    let projected = metric.elapsed_projection(now)?;
    let used = metric.used_fraction()?;
    let rate = used / hours(age.max(Duration::minutes(1)));
    calculate(
        projected * 100.,
        Some((1. - used).max(0.) / rate),
        hours(reset - now),
        hours(length),
    )
}
/// The score to show: firm once mature, early before that, `None` for `SUS n/a`.
pub fn reading(metric: &Metric, now: DateTime<Utc>) -> Option<Reading> {
    weekly(metric, now)
        .map(Reading::Firm)
        .or_else(|| early(metric, now).map(Reading::Early))
}

pub fn text(metric: &Metric, now: DateTime<Utc>) -> Option<String> {
    if is_weekly(metric) {
        Some(reading(metric, now).map_or_else(|| "SUS n/a".into(), Reading::text))
    } else if matches!(metric,Metric::Activity{label,..} if label=="Turns/7d") {
        Some("SUS n/a".into())
    } else {
        None
    }
}

/// Monthly USD cents and an explicitly dated future price. Unknown prices never count as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Price {
    pub monthly_cents: Option<u64>,
    /// A dated target may itself be unknown; the old price must then stop applying.
    pub scheduled: Option<(Option<u64>, DateTime<Utc>)>,
}
impl Price {
    pub fn fixed(cents: u64) -> Self {
        Self {
            monthly_cents: Some(cents),
            scheduled: None,
        }
    }
    pub fn at(&self, now: DateTime<Utc>) -> Option<u64> {
        match self.scheduled {
            Some((cents, at)) if now >= at => cents,
            _ => self.monthly_cents,
        }
        .filter(|p| *p > 0)
    }
}

/// Average one account's eligible pools. Missing scores are excluded.
pub fn subscription_score(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<Score> {
    let scores: Vec<_> = account
        .status_metrics()
        .filter(|metric| account.sus_eligible(metric, now))
        .filter_map(|metric| weekly(metric, now))
        .collect();
    (!scores.is_empty())
        .then(|| Score::rounded(scores.iter().map(|s| s.value() as f64).sum::<f64>() / scores.len() as f64))
}
#[derive(Clone, Debug, PartialEq)]
pub struct Fleet {
    pub score: Option<Score>,
    pub equal_weights: bool,
    pub subscriptions: usize,
}
/// Items are subscriptions, not buckets. The UI supplies only available accounts and effective prices.
pub fn fleet(items: impl IntoIterator<Item = (Option<Score>, Option<u64>)>) -> Fleet {
    let values: Vec<_> = items
        .into_iter()
        .filter_map(|(s, p)| s.map(|s| (s, p.filter(|p| *p > 0))))
        .collect();
    let equal_weights = values.iter().any(|(_, price)| price.is_none());
    let total: f64 = values
        .iter()
        .map(|(_, p)| if equal_weights { 1. } else { p.unwrap() as f64 })
        .sum();
    let weighted: f64 = values
        .iter()
        .map(|(s, p)| s.value() as f64 * if equal_weights { 1. } else { p.unwrap() as f64 })
        .sum();
    Fleet {
        score: (!values.is_empty()).then(|| Score::rounded(weighted / total)),
        equal_weights,
        subscriptions: values.len(),
    }
}
/// Explicit prices requested for these known tiers; never infer a tier from quota limits.
pub fn known_price(provider: Provider, plan: &str) -> Option<Price> {
    let cents = match (provider, plan) {
        (Provider::Codex, "pro") | (Provider::Claude, "Max 20x") => 20_000,
        (Provider::Codex, "prolite") | (Provider::Claude, "Max 5x") => 10_000,
        _ => return None,
    };
    Some(Price::fixed(cents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountId, Pace};
    fn now() -> DateTime<Utc> {
        "2026-10-23T14:00:00Z".parse().unwrap()
    }
    fn metric(age: Duration, projected: f64) -> Metric {
        let left = Duration::days(7) - age;
        Metric::Window {
            label: "Weekly".into(),
            used: 0.01,
            resets_at: now() + left,
            pace: Some(Pace::per_hour((projected - 0.01) / (left.num_seconds() as f64 / 3600.))),
            length: Some(Duration::days(7)),
        }
    }
    #[test]
    fn unused_argon_cannot_replace_a_missing_models_weekly_status() {
        let at = now();
        let argon = Metric::Window {
            label: "Argon weekly".into(),
            used: 0.9,
            resets_at: at + Duration::days(5),
            pace: Some(crate::Pace::per_hour(0.1)),
            length: Some(Duration::days(7)),
        };
        let account = AccountSnapshot::new(crate::AccountId::new("gemini"), Provider::Gemini, vec![argon], at);
        assert_eq!(
            subscription_score(&account, at),
            None,
            "unused Argon must not supply SUS"
        );
        assert_eq!(
            account.assess(at).severity(),
            crate::Severity::Normal,
            "unused Argon must not raise risk"
        );
        let alerts = crate::alerts::evaluate(
            &crate::alerts::AlertSettings {
                enabled: true,
                ..Default::default()
            },
            &Default::default(),
            &[account],
            at,
        );
        assert!(alerts.notify.is_empty(), "unused Argon must not notify");
    }

    #[test]
    fn a_reached_quota_uses_the_same_zero_time_to_limit_as_its_bar() {
        let metric = Metric::Window {
            label: "Weekly".into(),
            used: 1.1,
            resets_at: now() + Duration::hours(100),
            pace: Some(Pace::per_hour(0.02)),
            length: Some(Duration::days(7)),
        };
        assert_eq!(metric.time_to_limit(), Some(Duration::zero()));
        assert_eq!(weekly(&metric, now()).unwrap().value(), 40);
    }
    #[test]
    fn declared_calendar_weeks_use_their_actual_length() {
        for hours in [167, 169] {
            let metric = Metric::Window {
                label: "Weekly usage".into(),
                used: 0.74,
                resets_at: now() + Duration::hours(hours - 24),
                pace: Some(Pace::per_hour(0.02)),
                length: Some(Duration::hours(hours)),
            };
            assert!(is_weekly(&metric));
            assert_eq!(weekly(&metric, now()).unwrap().value(), 22);
            assert_eq!(text(&metric, now()).as_deref(), Some("SUS 22"));
            let account = AccountSnapshot::new(AccountId::new("calendar"), Provider::Grok, vec![metric], now());
            assert_eq!(
                fleet([(subscription_score(&account, now()), Some(20_000))])
                    .score
                    .unwrap()
                    .value(),
                22
            );
        }
        for hours in [5, 24, 24 * 28] {
            let metric = Metric::Window {
                label: "Weekly".into(),
                used: 0.5,
                resets_at: now() + Duration::hours(1),
                pace: Some(Pace::per_hour(0.01)),
                length: Some(Duration::hours(hours)),
            };
            assert!(!is_weekly(&metric));
            assert_eq!(text(&metric, now()), None);
        }
        let metric = Metric::Window {
            label: "Monthly".into(),
            used: 0.5,
            resets_at: now() + Duration::hours(100),
            pace: Some(Pace::per_hour(0.01)),
            length: Some(Duration::hours(169)),
        };
        assert!(!is_weekly(&metric), "a duration near a week alone is insufficient");
    }
    #[test]
    fn reference_vectors_and_bankers_rounding() {
        for (p, out, reset, expected) in [
            (208., Some(25.), 113., 48),
            (132., Some(52.), 92., 76),
            (80., None, 50., 80),
            (100., None, 1., 100),
            (400., Some(1.), 167., 1),
            (900., Some(0.), 168., 0),
            (110., None, 10., 100),
            (150., Some(-3.), 20., 86),
            (74.5, None, 10., 74),
            (75.5, None, 10., 76),
        ] {
            assert_eq!(calculate(p, out, reset, 168.).unwrap().value(), expected);
        }
    }
    #[test]
    fn one_hour_boundary_and_missing_forecasts() {
        assert_eq!(
            weekly(&metric(Duration::hours(1) - Duration::seconds(1), 0.8), now()),
            None
        );
        assert_eq!(weekly(&metric(Duration::hours(12), 0.8), now()).unwrap().value(), 80);
        let mut m = metric(Duration::hours(13), 0.8);
        if let Metric::Window { length, .. } = &mut m {
            *length = None;
        }
        assert_eq!(weekly(&m, now()), None);
        if let Metric::Window { length, .. } = &mut m {
            *length = Some(Duration::hours(5));
        }
        assert_eq!(weekly(&m, now()), None);
        assert_eq!(calculate(f64::NAN, None, 5., 168.), None);
    }
    /// `used` of a seven-day week `age` old, with the provider pace that only exists from the one-hour maturity.
    fn young(used: f64, age: Duration) -> Metric {
        Metric::Window {
            label: "Weekly".into(),
            used,
            resets_at: now() + Duration::days(7) - age,
            pace: (age >= WEEKLY_MATURITY).then(|| Pace::per_hour(used / (age.num_seconds() as f64 / 3600.))),
            length: Some(Duration::days(7)),
        }
    }
    #[test]
    fn early_on_usage_and_firm_from_one_hour() {
        let second = Duration::seconds(1);
        for (age, expected) in [
            (EARLY_SUS - second, None),
            (Duration::minutes(59), Some(Reading::Early(Score(7)))),
            (Duration::hours(6), Some(Reading::Firm(Score(45)))),
            (WEEKLY_MATURITY - second, Some(Reading::Early(Score(7)))),
            (WEEKLY_MATURITY, Some(Reading::Firm(Score(7)))),
        ] {
            let metric = young(0.08, age);
            assert_eq!(reading(&metric, now()), expected, "{age}");
            assert_eq!(weekly(&metric, now()).is_some(), age >= WEEKLY_MATURITY, "{age}");
            let shown = expected.map_or_else(|| "SUS n/a".into(), Reading::text);
            assert_eq!(text(&metric, now()), Some(shown), "{age}");
        }
        assert_eq!(Reading::Early(Score(55)).text(), "SUS 55 early");
        assert!(Reading::Early(Score(55)).is_early() && !Reading::Firm(Score(55)).is_early());
        // Five-hour windows still have no SUS at any age.
        let mut session = young(0.3, Duration::minutes(30));
        if let Metric::Window { length, resets_at, .. } = &mut session {
            *length = Some(Duration::hours(5));
            *resets_at = now() + Duration::hours(3);
        }
        assert_eq!(reading(&session, now()), None);
        assert_eq!(text(&session, now()), None);
    }
    #[test]
    fn an_early_score_never_forecasts_raises_risk_alerts_or_fleet() {
        // 30% two hours into a week projects a lockout: a red-range early score, and nothing else may act on it.
        let at = now();
        let metric = young(0.3, Duration::minutes(30));
        assert_eq!(reading(&metric, at), Some(Reading::Early(Score(1))));
        assert_eq!(metric.projected_at_reset(at), None);
        assert_eq!(metric.time_to_limit(), None);
        let account = AccountSnapshot::new(AccountId::new("early"), Provider::Claude, vec![metric], at);
        assert_eq!(account.assess(at).severity(), crate::Severity::Normal);
        assert_eq!(subscription_score(&account, at), None, "fleet SUS stays firm-only");
        let alerts = crate::alerts::evaluate(
            &crate::alerts::AlertSettings {
                enabled: true,
                ..Default::default()
            },
            &Default::default(),
            &[account],
            at,
        );
        assert!(alerts.notify.is_empty());
    }
    #[test]
    fn score_inputs_reject_each_invalid_dimension_and_accept_zero_usage() {
        assert_eq!(calculate(0., None, 5., 168.).unwrap().value(), 0);
        for value in [-1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(calculate(value, None, 5., 168.), None, "projected {value}");
        }
        for value in [0., -1., f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(calculate(80., None, value, 168.), None, "reset {value}");
            assert_eq!(calculate(80., None, 5., value), None, "length {value}");
        }
    }
    #[test]
    fn bands_are_numeric_and_have_exact_boundaries() {
        for (n, b) in [
            (74, Band::Red),
            (75, Band::Yellow),
            (89, Band::Yellow),
            (90, Band::Green),
        ] {
            assert_eq!(Score(n).band(), b);
        }
    }
    #[test]
    fn fleet_averages_eligible_pools_before_weighting_one_subscription() {
        let a = AccountSnapshot::new(
            AccountId::new("multiple-pools"),
            Provider::Claude,
            vec![metric(Duration::hours(24), 0.8), metric(Duration::hours(24), 1.)],
            now(),
        );
        assert_eq!(subscription_score(&a, now()), Some(Score(90)));
        assert_eq!(
            fleet([(Some(Score(76)), Some(20_000)), (Some(Score(48)), Some(20_000))]).score,
            Some(Score(62))
        );
        assert_eq!(
            fleet([(Some(Score(76)), Some(10_000)), (Some(Score(48)), Some(30_000))]).score,
            Some(Score(55))
        );
        assert_eq!(
            fleet([(Some(Score(100)), Some(20_000)), (Some(Score(0)), Some(1999))]).score,
            Some(Score(91))
        );
        let equal = fleet([(Some(Score(100)), Some(20_000)), (Some(Score(0)), None)]);
        assert_eq!(equal.score, Some(Score(50)));
        assert!(equal.equal_weights);
        assert_eq!(fleet([(None, Some(20_000))]).score, None);
    }
    #[test]
    fn explicit_price_changes_apply_at_the_instant_not_before() {
        for provider in [Provider::Claude, Provider::Codex] {
            let price = Price {
                monthly_cents: Some(20_000),
                scheduled: Some((Some(10_000), now())),
            };
            assert_eq!(price.at(now() - Duration::milliseconds(1)), Some(20_000));
            assert_eq!(price.at(now()), Some(10_000));
            assert_eq!(
                fleet([(Some(Score(100)), price.at(now())), (Some(Score(0)), Some(20_000))]).score,
                Some(Score(33)),
                "{provider:?}"
            );
        }
    }
}
