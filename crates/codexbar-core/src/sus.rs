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
            .filter(|out| out.is_finite() && *out >= 0.)
            .map_or(0., |out| (reset_hours - out).max(0.));
        100. - 100. * dark / length_hours
    };
    Some(Score::rounded(value))
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
        Some(length) => declared_weekly && (Duration::days(6)..=Duration::days(8)).contains(&length),
        None => declared_weekly,
    }
}
pub fn weekly(metric: &Metric, now: DateTime<Utc>) -> Option<Score> {
    if !is_weekly(metric) {
        return None;
    }
    let length = metric.window_length()?;
    let reset = metric.resets_at()?;
    if now.signed_duration_since(reset.checked_sub_signed(length)?) < Duration::hours(12) {
        return None;
    }
    let hours = |d: Duration| d.num_milliseconds() as f64 / 3_600_000.;
    calculate(
        metric.projected_at_reset(now)? * 100.,
        metric.time_to_limit().map(hours),
        hours(reset - now),
        hours(length),
    )
}

pub fn text(metric: &Metric, now: DateTime<Utc>) -> Option<String> {
    if is_weekly(metric) {
        Some(
            weekly(metric, now)
                .map(|s| format!("SUS {}", s.value()))
                .unwrap_or_else(|| "SUS n/a".into()),
        )
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
        .metrics()
        .iter()
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
            used: 0.,
            resets_at: now() + left,
            pace: Some(Pace::per_hour(projected / (left.num_seconds() as f64 / 3600.))),
            length: Some(Duration::days(7)),
        }
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
            (74.5, None, 10., 74),
            (75.5, None, 10., 76),
        ] {
            assert_eq!(calculate(p, out, reset, 168.).unwrap().value(), expected);
        }
    }
    #[test]
    fn twelve_hour_boundary_and_missing_forecasts() {
        assert_eq!(
            weekly(&metric(Duration::hours(12) - Duration::seconds(1), 0.8), now()),
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
    fn fleet_averages_pools_before_weighting_one_subscription() {
        let a = AccountSnapshot::new(
            AccountId::new("ag"),
            Provider::Gemini,
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
