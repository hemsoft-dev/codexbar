//! Synthetic history for the demo dashboard (`--demo`), kept in memory only.
//!
//! Each series ends at the account's current value, so the History view agrees with the table. Between them the
//! demo covers the shapes the charts must handle: sawtooth windows, a steady monthly climb, a spending balance with
//! a top-up, a flat balance, a spike, and a two-day gap in the data.

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{AccountSnapshot, Metric};

use crate::HistoryStore;
#[cfg(test)]
use crate::summary::GAP_THRESHOLD;
use crate::summary::Point;

/// How far back demo history goes; matches the live retention.
const DEMO_DAYS: i64 = 30;
const STEP_MINUTES: i64 = 30;

pub fn demo_history(accounts: &[AccountSnapshot], now: DateTime<Utc>) -> HistoryStore {
    let mut store = HistoryStore::in_memory(Duration::days(DEMO_DAYS));
    for account in accounts {
        let id = account.id().as_str();
        for (ix, metric) in account.metrics().iter().enumerate() {
            let Some(current) = metric.history_value() else {
                continue;
            };
            let seed = hash(id) ^ (ix as u64).wrapping_mul(0x9E37_79B9);
            let mut points = series(metric, current, seed, now);
            // One account has a two-day hole, as if the PC was off, so gaps are visible in the demo.
            if id == "cursor" {
                let (from, to) = (now - Duration::days(12), now - Duration::days(10));
                points.retain(|point| point.at < from || point.at >= to);
            }
            store.insert_points(id, &metric.key(), &points);
        }
    }
    store
}

fn series(metric: &Metric, current: f64, seed: u64, now: DateTime<Utc>) -> Vec<Point> {
    let steps = DEMO_DAYS * 24 * 60 / STEP_MINUTES;
    let start = now - Duration::minutes(steps * STEP_MINUTES);
    let at = |step: i64| start + Duration::minutes(step * STEP_MINUTES);
    let mut noise = Noise(seed | 1);
    match metric {
        Metric::Window { label, resets_at, .. } => {
            let period = if label.to_lowercase().contains("5-hour") {
                Duration::hours(5)
            } else if label.to_lowercase().contains("week") {
                Duration::days(7)
            } else {
                Duration::days(30)
            };
            // Windows follow the account's real reset times: each one climbs from zero and resets, and the
            // current one reaches `current` now.
            let window_start = *resets_at - period;
            let minutes = period.num_minutes() as f64;
            let elapsed_now = ((now - window_start).num_minutes() as f64 / minutes).clamp(0.05, 1.0);
            (0..=steps)
                .map(|step| {
                    if step == steps {
                        return Point::new(now, current);
                    }
                    let into = (at(step) - window_start).num_minutes().rem_euclid(period.num_minutes());
                    let progress = into as f64 / minutes / elapsed_now;
                    let value = current * progress.powf(0.8) + noise.next() * 0.02;
                    Point::new(at(step), value.clamp(0.0, 1.0))
                })
                .collect()
        }
        // Spend and quotas climb steadily through the period, with a short burst three days ago.
        Metric::Quota { .. } | Metric::Spend { .. } => {
            // Quotas are fractions used; spend is money, so its burst and ceiling scale with the amount.
            let (burst, ceiling) = match metric {
                Metric::Spend { .. } => (current * 0.25, f64::MAX),
                _ => (0.25, 1.0),
            };
            let spike = steps - 3 * 24 * 60 / STEP_MINUTES;
            (0..=steps)
                .map(|step| {
                    let progress = step as f64 / steps as f64;
                    let mut value = current * progress + noise.next() * 0.01;
                    // A short burst three days ago, so the chart shows a spike that downsampling must keep.
                    if (spike..spike + 2).contains(&step) {
                        value += burst;
                    }
                    if step == steps {
                        value = current;
                    }
                    Point::new(at(step), value.clamp(0.0, ceiling))
                })
                .collect()
        }
        Metric::Balance { burn_per_day, .. } => {
            let burn = burn_per_day.map_or(0.0, |burn| burn.cents() as f64 / 100.0);
            let top_up_at = steps / 3;
            let top_up = burn * 12.0;
            (0..=steps)
                .map(|step| {
                    let days_left = (steps - step) as f64 * STEP_MINUTES as f64 / (24.0 * 60.0);
                    // Spending runs the balance down toward `current`; an earlier top-up raised it once.
                    let mut value = current + burn * days_left;
                    if burn > 0.0 && step < top_up_at {
                        value -= top_up;
                    }
                    Point::new(at(step), value.max(0.0))
                })
                .collect()
        }
        // Credits run down evenly from a starting pool to `current`.
        Metric::Credits { start, .. } => {
            let from = start.unwrap_or(current).max(current);
            (0..=steps)
                .map(|step| {
                    let progress = step as f64 / steps as f64;
                    Point::new(at(step), from - (from - current) * progress)
                })
                .collect()
        }
    }
}

fn hash(text: &str) -> u64 {
    text.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01B3)
    })
}

/// A tiny deterministic generator, so demo screenshots are stable between runs.
struct Noise(u64);

impl Noise {
    /// A value in -0.5..0.5.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 10_000) as f64 / 10_000.0 - 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::summarize;
    use chrono::{Local, TimeZone};
    use codexbar_core::demo::demo_accounts;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 14, 0, 0).unwrap()
    }

    #[test]
    fn every_series_ends_at_the_accounts_current_value() {
        let accounts = demo_accounts(now(), &Local);
        let store = demo_history(&accounts, now());
        for account in &accounts {
            for metric in account.metrics() {
                let Some(current) = metric.history_value() else {
                    continue;
                };
                let points = store.points(account.id().as_str(), &metric.key(), now() - Duration::days(30), now());
                let last = points.last().expect("every demo metric has history");
                assert_eq!(last.at, now());
                assert_eq!(last.value, current, "{} {}", account.id().as_str(), metric.key());
            }
        }
    }

    #[test]
    fn demo_covers_a_gap_and_a_flat_series() {
        let accounts = demo_accounts(now(), &Local);
        let store = demo_history(&accounts, now());
        let since = now() - Duration::days(30);
        let cursor = summarize(
            &store.points("cursor", "included-usage", since, now()),
            now(),
            GAP_THRESHOLD,
        )
        .unwrap();
        assert!(cursor.longest_gap.is_some_and(|gap| gap >= Duration::days(2)));
        let flat = accounts
            .iter()
            .flat_map(|account| account.metrics().iter().map(move |metric| (account, metric)))
            .filter_map(|(account, metric)| {
                summarize(
                    &store.points(account.id().as_str(), &metric.key(), since, now()),
                    now(),
                    GAP_THRESHOLD,
                )
            })
            .any(|summary| summary.is_flat());
        assert!(flat, "a balance with no burn stays flat");
    }

    #[test]
    fn demo_history_is_never_written_to_disk() {
        let accounts = demo_accounts(now(), &Local);
        let mut store = demo_history(&accounts, now());
        store.record(&accounts, now() + Duration::hours(1)).unwrap();
        assert!(store.path().as_os_str().is_empty());
    }
}
