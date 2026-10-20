//! Turns stored history into the series the dashboard draws: a daily trend and the current-vs-previous window curve.

use chrono::{DateTime, Duration, TimeZone, Utc};
use codexbar_core::format::DateStyle;
use codexbar_core::{AccountSnapshot, Metric, WindowCurve};

use crate::HistoryStore;

/// Days in the trend sparkline.
pub const TREND_DAYS: usize = 14;
/// Points along a window curve.
const CURVE_BUCKETS: usize = 30;

/// Adds the stored trend and window curve to a freshly fetched account. Provider-supplied series are kept. Curve
/// labels are in `style`.
pub fn enrich<Tz: TimeZone>(
    store: &HistoryStore,
    account: AccountSnapshot,
    tz: &Tz,
    style: &dyn DateStyle,
    now: DateTime<Utc>,
) -> AccountSnapshot
where
    Tz::Offset: std::fmt::Display,
{
    let Some(primary) = account.primary().cloned() else {
        return account;
    };
    let id = account.id().as_str().to_owned();
    let key = primary.key();

    let account = if account.trend().len() < 2 {
        // Credits run down, so each day is where the balance ended, not its high (#150).
        let daily = if matches!(primary, Metric::Credits { .. }) {
            store.daily_last(&id, &key, TREND_DAYS, tz, now)
        } else {
            store.daily_max(&id, &key, TREND_DAYS, tz, now)
        };
        match trend(&daily) {
            Some(trend) => account.with_trend(trend),
            None => account,
        }
    } else {
        account
    };

    if account.detail().window_curve().is_some() {
        return account;
    }
    match window_curve(store, &id, &primary, tz, style, now) {
        Some(curve) => {
            let detail = account.detail().clone().with_window_curve(curve);
            account.with_detail(detail)
        }
        None => account,
    }
}

/// Drops leading empty days and carries the last known value across gaps. Needs two points to draw a line.
fn trend(days: &[Option<f64>]) -> Option<Vec<f64>> {
    let first = days.iter().position(Option::is_some)?;
    let mut last = 0.0;
    let values: Vec<f64> = days[first..]
        .iter()
        .map(|day| {
            if let Some(value) = day {
                last = *value;
            }
            last
        })
        .collect();
    (values.len() >= 2).then_some(values)
}

/// Samples of the current window bucketed over the window, next to the previous window at the same offsets.
fn window_curve<Tz: TimeZone>(
    store: &HistoryStore,
    account: &str,
    metric: &Metric,
    tz: &Tz,
    style: &dyn DateStyle,
    now: DateTime<Utc>,
) -> Option<WindowCurve>
where
    Tz::Offset: std::fmt::Display,
{
    let Metric::Window { resets_at, .. } = metric else {
        return None;
    };
    let key = metric.key();
    let samples: Vec<_> = store.series(account, &key).collect();
    let current: Vec<_> = samples.iter().filter(|s| s.resets_at() == Some(*resets_at)).collect();
    let previous_reset = samples
        .iter()
        .filter_map(|s| s.resets_at())
        .filter(|at| at < resets_at)
        .max();

    // With a previous window the spacing of resets is the window length; otherwise start at the first observation.
    let (start, length) = match previous_reset {
        Some(previous) => (previous, *resets_at - previous),
        None => {
            let first = current.first()?.at();
            (first, *resets_at - first)
        }
    };
    if length <= Duration::zero() {
        return None;
    }
    let step = length / CURVE_BUCKETS as i32;
    let bucket_end = |ix: usize| start + step * (ix as i32 + 1);

    let filled = |points: &[&&crate::Sample], origin: DateTime<Utc>, upto: usize| -> Vec<f64> {
        let mut value = 0.0;
        let mut cursor = 0;
        (0..upto)
            .map(|ix| {
                let end = origin + step * (ix as i32 + 1);
                while cursor < points.len() && points[cursor].at() <= end {
                    value = points[cursor].value();
                    cursor += 1;
                }
                value
            })
            .collect()
    };

    let elapsed_buckets = (0..CURVE_BUCKETS)
        .take_while(|ix| *ix == 0 || bucket_end(ix - 1) <= now)
        .count();
    let current_values = filled(&current, start, elapsed_buckets);
    if current_values.len() < 2 {
        return None;
    }
    let previous_values = match previous_reset {
        Some(previous) => {
            let points: Vec<_> = samples.iter().filter(|s| s.resets_at() == Some(previous)).collect();
            if points.is_empty() {
                Vec::new()
            } else {
                filled(&points, previous - length, CURVE_BUCKETS)
            }
        }
        None => Vec::new(),
    };
    let labels = (0..CURVE_BUCKETS)
        .map(|ix| {
            let local = bucket_end(ix).with_timezone(tz).naive_local();
            if length > Duration::days(1) {
                style.weekday_time(local)
            } else {
                style.time(local)
            }
        })
        .collect();
    Some(WindowCurve::new(labels, current_values, previous_values))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::FixedOffset;
    use codexbar_core::format::English;
    use codexbar_core::{AccountId, Provider};

    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("codexbar-enrich-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
    }

    fn codex(used: f64, resets_at: DateTime<Utc>, at: DateTime<Utc>) -> AccountSnapshot {
        AccountSnapshot::new(
            AccountId::new("codex"),
            Provider::Codex,
            vec![Metric::Window {
                label: "5-hour window".into(),
                used,
                resets_at,
                pace: None,
                length: None,
            }],
            at,
        )
    }

    #[test]
    fn trend_trims_leading_gaps_and_carries_values() {
        assert_eq!(trend(&[None, Some(0.2), None, Some(0.5)]), Some(vec![0.2, 0.2, 0.5]));
        assert_eq!(trend(&[None, None, Some(0.2)]), None);
        assert_eq!(trend(&[None, None]), None);
    }

    #[test]
    fn enrich_builds_current_and_previous_window_curves() {
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let path = temp_file("curve");
        let mut store = HistoryStore::open(&path, Duration::days(30), now());
        let previous_reset = now() - Duration::hours(2);
        let current_reset = previous_reset + Duration::hours(5);
        // Previous window: readings across its 5 hours.
        for (hours_before, used) in [(4, 0.1), (3, 0.3), (1, 0.6)] {
            let at = previous_reset - Duration::hours(hours_before);
            store.record(&[codex(used, previous_reset, at)], at).unwrap();
        }
        // Current window: two hours in.
        for (minutes, used) in [(30, 0.2), (90, 0.4), (120, 0.5)] {
            let at = previous_reset + Duration::minutes(minutes);
            store.record(&[codex(used, current_reset, at)], at).unwrap();
        }

        let account = enrich(&store, codex(0.5, current_reset, now()), &tz, &English, now());
        let curve = account.detail().window_curve().expect("curve");
        assert_eq!(curve.labels().len(), 30);
        assert_eq!(curve.previous().len(), 30);
        // Two of five hours elapsed: 12 finished 10-minute buckets plus the one now in progress.
        assert_eq!(curve.current().len(), 13);
        assert_eq!(*curve.current().last().unwrap(), 0.5);
        assert!(curve.previous().iter().all(|value| *value <= 0.6));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_credits_trend_keeps_each_days_last_balance() {
        let tz = FixedOffset::west_opt(0).unwrap();
        let path = temp_file("credits-trend");
        let mut store = HistoryStore::open(&path, Duration::days(30), now());
        let credits = |balance: f64, at| {
            AccountSnapshot::new(
                AccountId::new("codex"),
                Provider::Codex,
                vec![Metric::Credits {
                    label: "Credits".into(),
                    balance: Some(balance),
                    start: None,
                }],
                at,
            )
        };
        // Yesterday the balance ran from 900 down to 600; today from 600 down to 450.
        let yesterday = now() - Duration::days(1);
        for (hours, balance) in [(0, 900.0), (3, 750.0), (6, 600.0)] {
            let at = yesterday + Duration::hours(hours);
            store.record(&[credits(balance, at)], at).unwrap();
        }
        for (minutes, balance) in [(10, 520.0), (40, 450.0)] {
            let at = now() - Duration::hours(1) + Duration::minutes(minutes);
            store.record(&[credits(balance, at)], at).unwrap();
        }
        let account = enrich(&store, credits(450.0, now()), &tz, &English, now());
        assert_eq!(account.trend(), [600.0, 450.0], "where each day ended, not its high");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn enrich_single_sample_adds_nothing() {
        let tz = FixedOffset::west_opt(0).unwrap();
        let path = temp_file("single");
        let mut store = HistoryStore::open(&path, Duration::days(30), now());
        let reset = now() + Duration::hours(3);
        store.record(&[codex(0.3, reset, now())], now()).unwrap();
        let account = enrich(&store, codex(0.3, reset, now()), &tz, &English, now());
        assert!(account.trend().is_empty());
        assert!(account.detail().window_curve().is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
