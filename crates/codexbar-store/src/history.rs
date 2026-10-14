use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use codexbar_core::AccountSnapshot;
use serde::{Deserialize, Serialize};

use crate::summary::Point;

/// Bumped when the line format changes. A file with another version is set aside, never misread.
const SCHEMA_VERSION: u32 = 1;
/// A value this close to the previous sample is not a change.
const DEDUP_EPSILON: f64 = 0.0005;
/// Unchanged values still get a sample this often, so flat periods are visible as flat rather than missing.
const HEARTBEAT_MINUTES: i64 = 15;

#[derive(Serialize, Deserialize)]
struct Header {
    codexbar_history: u32,
}

/// One stored reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    t: i64,
    /// Account id.
    a: String,
    /// Metric key.
    m: String,
    /// Fraction used for limits, dollars for balances.
    v: f64,
    /// Unix seconds of the window's reset, when it has one. Samples sharing it belong to the same window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r: Option<i64>,
}

impl Sample {
    pub fn at(&self) -> DateTime<Utc> {
        Utc.timestamp_opt(self.t, 0).single().unwrap_or_default()
    }

    pub fn value(&self) -> f64 {
        self.v
    }

    pub fn resets_at(&self) -> Option<DateTime<Utc>> {
        self.r.and_then(|r| Utc.timestamp_opt(r, 0).single())
    }
}

/// `~/.codexbar/history.jsonl`, next to the existing settings file.
pub fn default_history_path() -> PathBuf {
    codexbar_core::home_dir()
        .unwrap_or_default()
        .join(".codexbar")
        .join("history.jsonl")
}

/// Usage history, loaded in memory and appended to disk as refreshes succeed.
pub struct HistoryStore {
    path: PathBuf,
    /// False for demo history, which lives in memory and never touches the history file.
    persist: bool,
    retention: Duration,
    samples: Vec<Sample>,
    last: HashMap<(String, String), usize>,
}

impl HistoryStore {
    /// Loads history, dropping samples past retention. Never fails: an unreadable or foreign file is renamed aside
    /// and history starts empty, so a bad file can't block startup.
    pub fn open(path: impl Into<PathBuf>, retention: Duration, now: DateTime<Utc>) -> Self {
        let path = path.into();
        let mut store = Self {
            path,
            persist: true,
            retention,
            samples: Vec::new(),
            last: HashMap::new(),
        };
        match store.load(now) {
            Ok(needs_compaction) => {
                if needs_compaction {
                    let _ = store.rewrite();
                }
            }
            Err(_) => {
                store.set_aside(now);
                store.samples.clear();
            }
        }
        store.reindex();
        store
    }

    /// History that lives only in memory (the demo dashboard). Recording and pruning work as usual; nothing is written.
    pub fn in_memory(retention: Duration) -> Self {
        Self {
            path: PathBuf::new(),
            persist: false,
            retention,
            samples: Vec::new(),
            last: HashMap::new(),
        }
    }

    /// Adds readings directly, for seeding in-memory demo history. Keeps samples sorted; skips deduplication so a
    /// generated series is stored exactly as given.
    pub fn insert_points(&mut self, account: &str, metric: &str, points: &[Point]) {
        self.samples.extend(points.iter().map(|point| Sample {
            t: point.at.timestamp(),
            a: account.to_owned(),
            m: metric.to_owned(),
            v: point.value,
            r: None,
        }));
        self.samples.sort_by_key(|sample| sample.t);
        self.reindex();
    }

    /// Readings for one metric from `since` through `until`, oldest first. The upper bound keeps out samples dated
    /// in the future, as after the system clock is set back.
    pub fn points(&self, account: &str, metric: &str, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Point> {
        let (since, until) = (since.timestamp(), until.timestamp());
        let mut points: Vec<Point> = self
            .series(account, metric)
            .filter(|sample| (since..=until).contains(&sample.t))
            .map(|sample| Point::new(sample.at(), sample.v))
            .collect();
        // Recording appends, so after the clock is set back samples can be out of time order; summaries and charts
        // need them oldest first. The sort is stable, so equal times keep their recorded order.
        points.sort_by_key(|point| point.at);
        points
    }

    /// Reads the file. Ok(true) means lines were skipped or expired and the file should be rewritten.
    fn load(&mut self, now: DateTime<Utc>) -> io::Result<bool> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };
        let mut lines = BufReader::new(file).lines();
        let header: Header = match lines.next() {
            None => return Ok(false),
            Some(line) => serde_json::from_str(&line?).map_err(|_| invalid("unreadable header"))?,
        };
        if header.codexbar_history != SCHEMA_VERSION {
            return Err(invalid("incompatible history version"));
        }
        let cutoff = (now - self.retention).timestamp();
        let mut dirty = false;
        for line in lines {
            let Ok(line) = line else {
                dirty = true;
                continue;
            };
            match serde_json::from_str::<Sample>(&line) {
                Ok(sample) if sample.t >= cutoff => self.samples.push(sample),
                // Expired or a torn/corrupt line: drop it and compact.
                _ => dirty = true,
            }
        }
        self.samples.sort_by_key(|sample| sample.t);
        Ok(dirty)
    }

    fn set_aside(&self, now: DateTime<Utc>) {
        let aside = self
            .path
            .with_extension(format!("jsonl.unreadable-{}", now.timestamp()));
        let _ = fs::rename(&self.path, aside);
    }

    fn reindex(&mut self) {
        self.last.clear();
        for (ix, sample) in self.samples.iter().enumerate() {
            self.last.insert((sample.a.clone(), sample.m.clone()), ix);
        }
    }

    /// Appends a sample for each metric that changed (or hit the heartbeat) and prunes expired samples.
    /// Returns how many samples were written.
    pub fn record(&mut self, accounts: &[AccountSnapshot], now: DateTime<Utc>) -> io::Result<usize> {
        let mut fresh = Vec::new();
        for account in accounts {
            for metric in account.metrics() {
                let Some(value) = metric.history_value() else { continue };
                let sample = Sample {
                    t: now.timestamp(),
                    a: account.id().as_str().to_owned(),
                    m: metric.key(),
                    v: value,
                    r: metric.resets_at().map(|at| at.timestamp()),
                };
                if !self.is_duplicate(&sample) {
                    fresh.push(sample);
                }
            }
        }

        let cutoff = (now - self.retention).timestamp();
        let expired = self.samples.first().is_some_and(|sample| sample.t < cutoff);
        let written = fresh.len();
        self.samples.extend(fresh.iter().cloned());
        if expired {
            self.samples.retain(|sample| sample.t >= cutoff);
            self.reindex();
            self.rewrite()?;
        } else {
            self.reindex();
            self.append(&fresh)?;
        }
        Ok(written)
    }

    fn is_duplicate(&self, sample: &Sample) -> bool {
        let Some(&ix) = self.last.get(&(sample.a.clone(), sample.m.clone())) else {
            return false;
        };
        let last = &self.samples[ix];
        (last.v - sample.v).abs() < DEDUP_EPSILON
            && last.r == sample.r
            && sample.t - last.t < Duration::minutes(HEARTBEAT_MINUTES).num_seconds()
    }

    /// Deletes every sample of an account.
    /// Deletes every sample of `account`. If the file can't be rewritten, the samples are kept in memory too, so a
    /// retry rewrites the file rather than finding nothing left to delete.
    pub fn remove_account(&mut self, account: &str) -> io::Result<()> {
        if !self.samples.iter().any(|sample| sample.a == account) {
            return Ok(());
        }
        let kept = self.samples.clone();
        self.samples.retain(|sample| sample.a != account);
        self.reindex();
        if let Err(err) = self.rewrite() {
            self.samples = kept;
            self.reindex();
            return Err(err);
        }
        Ok(())
    }

    /// Moves every sample of account `from` to `to`, keeping any `to` already has. For accounts that change id,
    /// such as a provider's single legacy account gaining its configured id. Returns how many samples moved.
    pub fn rename_account(&mut self, from: &str, to: &str) -> io::Result<usize> {
        let mut moved = 0;
        for sample in self.samples.iter_mut().filter(|sample| sample.a == from) {
            sample.a = to.to_owned();
            moved += 1;
        }
        if moved > 0 {
            self.reindex();
            self.rewrite()?;
        }
        Ok(moved)
    }

    /// Samples for one metric, oldest first.
    pub fn series<'a>(&'a self, account: &'a str, metric: &'a str) -> impl Iterator<Item = &'a Sample> + 'a {
        self.samples
            .iter()
            .filter(move |sample| sample.a == account && sample.m == metric)
    }

    /// The highest value per calendar day in `tz`, for the last `days` days ending today. Days without samples are
    /// `None`, so callers can tell missing data from zero.
    pub fn daily_max<Tz: TimeZone>(
        &self,
        account: &str,
        metric: &str,
        days: usize,
        tz: &Tz,
        now: DateTime<Utc>,
    ) -> Vec<Option<f64>> {
        let today = now.with_timezone(tz).date_naive();
        let mut out = vec![None; days];
        for sample in self.series(account, metric) {
            let day = sample.at().with_timezone(tz).date_naive();
            let Ok(age) = usize::try_from((today - day).num_days()) else {
                continue;
            };
            if age < days {
                let slot = &mut out[days - 1 - age];
                *slot = Some(slot.map_or(sample.v, |max: f64| max.max(sample.v)));
            }
        }
        out
    }

    /// The last sample of each of the last `days` local days, oldest first: what a running-down balance stood at when
    /// each day ended (#150), where `daily_max` would keep the day's starting balance.
    pub fn daily_last<Tz: TimeZone>(
        &self,
        account: &str,
        metric: &str,
        days: usize,
        tz: &Tz,
        now: DateTime<Utc>,
    ) -> Vec<Option<f64>> {
        let today = now.with_timezone(tz).date_naive();
        let mut last: Vec<Option<(DateTime<Utc>, f64)>> = vec![None; days];
        for sample in self.series(account, metric) {
            let day = sample.at().with_timezone(tz).date_naive();
            let Ok(age) = usize::try_from((today - day).num_days()) else {
                continue;
            };
            if age < days {
                let slot = &mut last[days - 1 - age];
                if slot.is_none_or(|(at, _)| sample.at() >= at) {
                    *slot = Some((sample.at(), sample.v));
                }
            }
        }
        last.into_iter().map(|slot| slot.map(|(_, value)| value)).collect()
    }

    fn append(&self, samples: &[Sample]) -> io::Result<()> {
        if samples.is_empty() || !self.persist {
            return Ok(());
        }
        let is_new = !self.path.exists();
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut buf = String::new();
        if is_new {
            buf.push_str(&header_line());
        }
        for sample in samples {
            buf.push_str(&serde_json::to_string(sample).map_err(|_| invalid("unserializable sample"))?);
            buf.push('\n');
        }
        file.write_all(buf.as_bytes())
    }

    /// Writes the whole file to a temp file and swaps it in, so a crash mid-write never leaves a torn history.
    fn rewrite(&self) -> io::Result<()> {
        if !self.persist {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut buf = header_line();
        for sample in &self.samples {
            buf.push_str(&serde_json::to_string(sample).map_err(|_| invalid("unserializable sample"))?);
            buf.push('\n');
        }
        fs::write(&tmp, buf)?;
        fs::rename(&tmp, &self.path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

fn header_line() -> String {
    format!("{}\n", serde_json::json!({ "codexbar_history": SCHEMA_VERSION }))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use chrono::FixedOffset;
    use codexbar_core::{AccountId, Metric, Money, Provider};

    use super::*;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("codexbar-store-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("history.jsonl")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn account(id: &str, used: f64, resets_in_hours: i64) -> AccountSnapshot {
        AccountSnapshot::new(
            AccountId::new(id),
            Provider::Codex,
            vec![
                Metric::Window {
                    label: "5-hour window".into(),
                    used,
                    resets_at: now() + Duration::hours(resets_in_hours),
                    pace: None,
                },
                Metric::Balance {
                    label: "Credits".into(),
                    remaining: Money::from_cents(1842),
                    burn_per_day: None,
                },
            ],
            now(),
        )
    }

    fn retention() -> Duration {
        Duration::days(30)
    }

    #[test]
    fn record_then_reopen_restores_samples() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        assert_eq!(store.record(&[account("codex", 0.4, 3)], now()).unwrap(), 2);
        let reopened = HistoryStore::open(dir.file(), retention(), now());
        assert_eq!(reopened.len(), 2);
        let series: Vec<f64> = reopened.series("codex", "5-hour-window").map(Sample::value).collect();
        assert_eq!(series, [0.4]);
    }

    #[test]
    fn record_unchanged_value_within_heartbeat_is_deduplicated() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store.record(&[account("codex", 0.4, 3)], now()).unwrap();
        let later = now() + Duration::minutes(2);
        assert_eq!(store.record(&[account("codex", 0.4, 3)], later).unwrap(), 0);
        let after_heartbeat = now() + Duration::minutes(16);
        assert_eq!(store.record(&[account("codex", 0.4, 3)], after_heartbeat).unwrap(), 2);
        assert_eq!(store.record(&[account("codex", 0.45, 3)], after_heartbeat).unwrap(), 1);
    }

    #[test]
    fn open_prunes_samples_past_retention() {
        let dir = TempDir::new();
        let old = now() - Duration::days(40);
        let mut store = HistoryStore::open(dir.file(), retention(), old);
        store.record(&[account("codex", 0.1, 3)], old).unwrap();
        let reopened = HistoryStore::open(dir.file(), retention(), now());
        assert!(reopened.is_empty());
        // The compaction rewrote the file down to its header.
        assert_eq!(fs::read_to_string(dir.file()).unwrap().lines().count(), 1);
    }

    #[test]
    fn open_corrupt_lines_are_skipped_and_compacted() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store.record(&[account("codex", 0.4, 3)], now()).unwrap();
        let mut file = OpenOptions::new().append(true).open(dir.file()).unwrap();
        writeln!(file, "{{not json").unwrap();
        writeln!(file, r#"{{"t":"oops"}}"#).unwrap();
        let reopened = HistoryStore::open(dir.file(), retention(), now());
        assert_eq!(reopened.len(), 2);
        assert!(!fs::read_to_string(dir.file()).unwrap().contains("not json"));
    }

    #[test]
    fn open_unreadable_header_sets_file_aside_and_starts_empty() {
        let dir = TempDir::new();
        fs::write(dir.file(), "garbage\n{}\n").unwrap();
        let store = HistoryStore::open(dir.file(), retention(), now());
        assert!(store.is_empty());
        assert!(!dir.file().exists());
        let aside = fs::read_dir(&dir.0).unwrap().filter_map(Result::ok).count();
        assert_eq!(aside, 1);
    }

    #[test]
    fn open_incompatible_version_is_set_aside() {
        let dir = TempDir::new();
        fs::write(dir.file(), "{\"codexbar_history\":99}\n").unwrap();
        assert!(HistoryStore::open(dir.file(), retention(), now()).is_empty());
        assert!(!dir.file().exists());
    }

    #[test]
    fn a_failed_removal_keeps_the_samples_for_a_retry() {
        let dir = TempDir::new();
        // The history file's folder is a file, so the rewrite fails.
        let blocker = dir.file().with_extension("blocker");
        std::fs::write(&blocker, "not a folder").unwrap();
        let mut store = HistoryStore::open(blocker.join("history.jsonl"), retention(), now());
        store.insert_points("a", "5-hour-window", &[Point::new(now(), 0.4)]);
        assert!(store.remove_account("a").is_err());
        assert_eq!(
            store.series("a", "5-hour-window").count(),
            1,
            "still there to delete on the retry"
        );
    }

    #[test]
    fn remove_account_deletes_only_its_samples() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store
            .record(&[account("a", 0.1, 3), account("b", 0.2, 3)], now())
            .unwrap();
        store.remove_account("a").unwrap();
        let reopened = HistoryStore::open(dir.file(), retention(), now());
        assert_eq!(reopened.series("a", "5-hour-window").count(), 0);
        assert_eq!(reopened.series("b", "5-hour-window").count(), 1);
    }

    #[test]
    fn points_exclude_samples_dated_after_the_range_end() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store
            .record(&[account("a", 0.1, 3)], now() - Duration::hours(1))
            .unwrap();
        store
            .record(&[account("a", 0.9, 3)], now() + Duration::hours(2))
            .unwrap();
        let points = store.points("a", "5-hour-window", now() - Duration::days(1), now());
        let values: Vec<f64> = points.iter().map(|point| point.value).collect();
        assert_eq!(values, vec![0.1]);
    }

    #[test]
    fn points_come_back_in_time_order_after_the_clock_is_set_back() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store.record(&[account("a", 0.5, 3)], now()).unwrap();
        // The clock moves back two hours; the next readings are recorded after the 12:00 one.
        store
            .record(&[account("a", 0.2, 3)], now() - Duration::hours(2))
            .unwrap();
        store
            .record(&[account("a", 0.3, 3)], now() - Duration::hours(1))
            .unwrap();
        let points = store.points("a", "5-hour-window", now() - Duration::days(1), now());
        let values: Vec<f64> = points.iter().map(|point| point.value).collect();
        assert_eq!(values, vec![0.2, 0.3, 0.5]);
    }

    #[test]
    fn rename_account_moves_samples_and_persists() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store
            .record(&[account("openrouter", 0.1, 3)], now() - Duration::hours(1))
            .unwrap();
        store.record(&[account("a1b2", 0.2, 3)], now()).unwrap();
        assert_eq!(store.rename_account("openrouter", "a1b2").unwrap(), 2);
        assert_eq!(
            store.rename_account("openrouter", "a1b2").unwrap(),
            0,
            "nothing left to move"
        );

        let reopened = HistoryStore::open(dir.file(), retention(), now());
        assert_eq!(reopened.series("openrouter", "5-hour-window").count(), 0);
        let merged: Vec<f64> = reopened.series("a1b2", "5-hour-window").map(Sample::value).collect();
        assert_eq!(merged, vec![0.1, 0.2], "old and new samples merge in time order");
    }

    #[test]
    fn daily_max_buckets_by_local_day_and_marks_missing_days() {
        let dir = TempDir::new();
        let tz = FixedOffset::west_opt(4 * 3600).unwrap();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store
            .record(&[account("codex", 0.3, 3)], now() - Duration::days(2))
            .unwrap();
        store
            .record(
                &[account("codex", 0.5, 3)],
                now() - Duration::days(2) + Duration::hours(1),
            )
            .unwrap();
        store.record(&[account("codex", 0.2, 3)], now()).unwrap();
        let days = store.daily_max("codex", "5-hour-window", 4, &tz, now());
        assert_eq!(days, [None, Some(0.5), None, Some(0.2)]);
    }

    #[test]
    fn history_file_contains_only_ids_keys_numbers_and_times() {
        let dir = TempDir::new();
        let mut store = HistoryStore::open(dir.file(), retention(), now());
        store.record(&[account("codex-chatgpt", 0.4, 3)], now()).unwrap();
        for line in fs::read_to_string(dir.file()).unwrap().lines().skip(1) {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
            assert!(
                keys.iter().all(|key| ["t", "a", "m", "v", "r"].contains(&key.as_str())),
                "{keys:?}"
            );
        }
    }
}
