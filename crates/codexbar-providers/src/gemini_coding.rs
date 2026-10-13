//! Gemini coding quotas (#107): the Antigravity daily quota summary, read through Google's own Antigravity CLI
//! (`agy -p "/usage" --output-format json`, a documented script interface that runs no model turn). The CLI keeps its
//! own Google sign-in and renews it; CodexBar borrows no OAuth client and stores no token.
//!
//! Four buckets map to stable metrics by id and window, never by display name: `gemini-5h`, `gemini-weekly`, `3p-5h`
//! and `3p-weekly`. Percent used is `1 - remaining_fraction`, and resets come only from `reset_time`. A missing,
//! duplicate, malformed or expired bucket stays unavailable, and the valid ones still show.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::Metric;
use serde_json::Value;

use crate::pace::elapsed_pace;
use crate::{CommandError, CommandRunner};

/// The CLI's usage command can take a moment to start; it never runs a model turn.
const TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// One coding quota CodexBar shows: the bucket id and window it comes from, and its metric label.
const BUCKETS: [(&str, &str, &str, i64); 4] = [
    ("gemini-5h", "5h", "Gemini Models five-hour", 5),
    ("gemini-weekly", "weekly", "Gemini Models weekly", 7 * 24),
    ("3p-5h", "5h", "Other models five-hour", 5),
    ("3p-weekly", "weekly", "Other models weekly", 7 * 24),
];

/// The Antigravity CLI to run: on PATH, or where its installer puts it.
pub fn cli() -> String {
    let installed = std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join(r"agy\bin\agy.exe"))
        .filter(|path| path.is_file());
    match installed {
        Some(path) => path.to_string_lossy().into_owned(),
        None => "agy".to_owned(),
    }
}

/// Why the coding quotas couldn't be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodingError {
    NotInstalled,
    /// The CLI is older than the first version whose `/usage` runs no model turn, or its version can't be read.
    TooOld,
    /// The CLI ran but didn't report usage: usually not signed in.
    NoUsage,
    TimedOut,
    Unknown,
}

impl std::fmt::Display for CodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInstalled => "Coding quotas need the Antigravity CLI (agy), signed in.",
            Self::TooOld => {
                "Coding quotas need Antigravity CLI 1.1.11 or newer, whose /usage runs no model turn. Update agy."
            }
            Self::NoUsage => "The Antigravity CLI didn't report coding usage. Sign in by running agy once.",
            Self::TimedOut => "The Antigravity CLI took too long to report coding usage.",
            Self::Unknown => "The Antigravity CLI reported coding usage in a shape CodexBar doesn't know.",
        })
    }
}

/// The coding quotas: the metrics that read, and a note for each bucket that didn't.
#[derive(Debug, Default, PartialEq)]
pub struct CodingUsage {
    pub metrics: Vec<Metric>,
    pub notes: Vec<String>,
}

fn reset(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?.as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Parses the CLI's `/usage` JSON at `now`.
pub fn parse(stdout: &str, now: DateTime<Utc>) -> Result<CodingUsage, CodingError> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| CodingError::Unknown)?;
    if value.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return Err(CodingError::NoUsage);
    }
    let command = value.get("command").ok_or(CodingError::NoUsage)?;
    if command.get("name").and_then(Value::as_str) != Some("usage") {
        return Err(CodingError::NoUsage);
    }
    let buckets: Vec<&Value> = command
        .pointer("/data/groups")
        .and_then(Value::as_array)
        .ok_or(CodingError::Unknown)?
        .iter()
        .filter_map(|group| group.get("buckets")?.as_array())
        .flatten()
        .collect();
    let mut usage = CodingUsage::default();
    for (id, window, label, hours) in BUCKETS {
        let matching: Vec<&&Value> = buckets
            .iter()
            .filter(|bucket| bucket.get("id").and_then(Value::as_str) == Some(id))
            .collect();
        let bucket = match matching.as_slice() {
            [only] => *only,
            [] => {
                usage.notes.push(format!("{label}: not reported by Antigravity."));
                continue;
            }
            _ => {
                usage
                    .notes
                    .push(format!("{label}: Antigravity reported it twice, so it isn't shown."));
                continue;
            }
        };
        let remaining = bucket
            .get("remaining_fraction")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite() && (0.0..=1.0).contains(f));
        let resets_at = reset(bucket.get("reset_time"));
        let disabled = bucket.get("disabled").and_then(Value::as_bool) == Some(true);
        match (
            bucket.get("window").and_then(Value::as_str) == Some(window),
            remaining,
            resets_at,
        ) {
            _ if disabled => usage.notes.push(format!("{label}: disabled by Antigravity.")),
            (true, Some(remaining), Some(resets_at)) if resets_at > now => {
                let used = 1.0 - remaining;
                usage.metrics.push(Metric::Window {
                    label: label.to_owned(),
                    used,
                    resets_at,
                    pace: elapsed_pace(used, resets_at - Duration::hours(hours), resets_at, now),
                });
            }
            (true, Some(_), Some(_)) => usage.notes.push(format!("{label}: its reset time has passed.")),
            _ => usage
                .notes
                .push(format!("{label}: reported in a shape CodexBar doesn't know.")),
        }
    }
    if usage.metrics.is_empty() && usage.notes.len() == BUCKETS.len() && buckets.is_empty() {
        return Err(CodingError::Unknown);
    }
    Ok(usage)
}

/// Runs the CLI and reads its coding quotas.
/// The first Antigravity CLI whose non-interactive `/usage` answers without starting an agent turn (its changelog,
/// 1.1.11). An older one could send `/usage` to the model as a prompt, using quota and starting a conversation.
const FIRST_SAFE_VERSION: (u32, u32, u32) = (1, 1, 11);

/// The `major.minor.patch` the CLI's `--version` prints (`1.3.3`, possibly after a name or before a suffix).
fn version(text: &str) -> Option<(u32, u32, u32)> {
    text.split_whitespace().find_map(|word| {
        let core = word.trim_start_matches('v').split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|part| part.parse::<u32>().ok());
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    })
}

pub fn fetch(runner: &dyn CommandRunner, now: DateTime<Utc>) -> Result<CodingUsage, CodingError> {
    // `/usage` is only run by a CLI known to answer it without a model turn; an unreadable version counts as too old.
    let installed = runner
        .run(&cli(), &["--version"], StdDuration::from_secs(10))
        .map_err(|err| match err {
            CommandError::NotFound => CodingError::NotInstalled,
            CommandError::TimedOut => CodingError::TimedOut,
            CommandError::Failed => CodingError::TooOld,
        })?;
    if !installed.success || version(&installed.stdout).is_none_or(|found| found < FIRST_SAFE_VERSION) {
        return Err(CodingError::TooOld);
    }
    let output = runner
        .run(
            &cli(),
            &["-p", "/usage", "--output-format", "json", "--print-timeout", "25s"],
            TIMEOUT,
        )
        .map_err(|err| match err {
            CommandError::NotFound => CodingError::NotInstalled,
            CommandError::TimedOut => CodingError::TimedOut,
            CommandError::Failed => CodingError::NoUsage,
        })?;
    if !output.success {
        return Err(CodingError::NoUsage);
    }
    parse(&output.stdout, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandOutput;

    fn now() -> DateTime<Utc> {
        "2026-10-13T02:00:00Z".parse().unwrap()
    }

    const USAGE: &str = r#"{"conversation_id":"","status":"SUCCESS","num_turns":0,
        "command":{"name":"usage","data":{"groups":[
          {"name":"Gemini Models","buckets":[
            {"id":"gemini-weekly","window":"weekly","remaining_fraction":0.9992612600326538,"reset_time":"2026-10-17T00:13:09Z"},
            {"id":"gemini-5h","window":"5h","remaining_fraction":1,"reset_time":"2026-10-13T06:55:08Z"}]},
          {"name":"Claude and GPT models","buckets":[
            {"id":"3p-weekly","window":"weekly","remaining_fraction":0.8480448126792908,"reset_time":"2026-10-17T12:37:38Z"},
            {"id":"3p-5h","window":"5h","remaining_fraction":0.69,"reset_time":"2026-10-13T06:54:48Z"}]}]}}}"#;

    #[test]
    fn buckets_map_by_id_and_window_with_used_as_one_minus_remaining() {
        let usage = parse(USAGE, now()).unwrap();
        let labels: Vec<&str> = usage.metrics.iter().map(Metric::label).collect();
        assert_eq!(
            labels,
            [
                "Gemini Models five-hour",
                "Gemini Models weekly",
                "Other models five-hour",
                "Other models weekly"
            ]
        );
        assert_eq!(usage.metrics[0].used_fraction(), Some(0.0), "1 remaining is 0% used");
        assert!(
            (usage.metrics[2].used_fraction().unwrap() - 0.31).abs() < 1e-9,
            "0.69 remaining is 31% used"
        );
        assert_eq!(usage.metrics[3].resets_at(), "2026-10-17T12:37:38Z".parse().ok());
        assert!(usage.notes.is_empty());
        let keys: Vec<String> = usage.metrics.iter().map(Metric::key).collect();
        assert_eq!(
            keys,
            [
                "gemini-models-five-hour",
                "gemini-models-weekly",
                "other-models-five-hour",
                "other-models-weekly"
            ]
        );
    }

    #[test]
    fn bad_buckets_stay_unavailable_and_the_rest_still_show() {
        let broken = USAGE
            .replace(r#""remaining_fraction":0.69"#, r#""remaining_fraction":1.7"#)
            .replace(
                r#""id":"gemini-weekly","window":"weekly""#,
                r#""id":"gemini-weekly","window":"daily""#,
            )
            .replace(
                r#""reset_time":"2026-10-17T12:37:38Z""#,
                r#""reset_time":"2026-10-01T00:00:00Z""#,
            );
        let usage = parse(&broken, now()).unwrap();
        let labels: Vec<&str> = usage.metrics.iter().map(Metric::label).collect();
        assert_eq!(labels, ["Gemini Models five-hour"]);
        assert_eq!(
            usage.notes,
            [
                "Gemini Models weekly: reported in a shape CodexBar doesn't know.",
                "Other models five-hour: reported in a shape CodexBar doesn't know.",
                "Other models weekly: its reset time has passed.",
            ]
        );
        let duplicated = USAGE.replace(r#""id":"3p-5h""#, r#""id":"3p-weekly""#);
        let usage = parse(&duplicated, now()).unwrap();
        assert!(
            usage
                .notes
                .contains(&"Other models weekly: Antigravity reported it twice, so it isn't shown.".to_owned())
        );
        assert!(
            usage
                .notes
                .contains(&"Other models five-hour: not reported by Antigravity.".to_owned())
        );
        let disabled = USAGE.replace(r#""id":"gemini-5h","#, r#""id":"gemini-5h","disabled":true,"#);
        assert!(
            parse(&disabled, now())
                .unwrap()
                .notes
                .contains(&"Gemini Models five-hour: disabled by Antigravity.".to_owned())
        );
    }

    #[test]
    fn a_cli_that_isnt_signed_in_or_answers_otherwise_reports_no_usage() {
        assert_eq!(
            parse(r#"{"status":"ERROR","response":"Please sign in"}"#, now()),
            Err(CodingError::NoUsage)
        );
        assert_eq!(
            parse(r#"{"status":"SUCCESS","response":"hello","num_turns":1}"#, now()),
            Err(CodingError::NoUsage),
            "an answer that isn't the usage command is never read as usage"
        );
        assert_eq!(parse("not json", now()), Err(CodingError::Unknown));
        assert_eq!(
            parse(
                r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[]}}}"#,
                now()
            ),
            Err(CodingError::Unknown)
        );
    }

    struct Runner(Result<CommandOutput, CommandError>, std::sync::Mutex<Vec<Vec<String>>>);

    impl CommandRunner for Runner {
        fn run(&self, _: &str, args: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
            self.1
                .lock()
                .unwrap()
                .push(args.iter().map(|a| (*a).to_owned()).collect());
            if args == ["--version"] {
                return self.0.clone().map(|_| CommandOutput {
                    success: true,
                    stdout: "1.3.3\n".to_owned(),
                    stderr: String::new(),
                });
            }
            self.0.clone()
        }
    }

    #[test]
    fn the_cli_is_asked_for_its_version_then_the_usage_command_only() {
        let runner = Runner(
            Ok(CommandOutput {
                success: true,
                stdout: USAGE.to_owned(),
                stderr: String::new(),
            }),
            Default::default(),
        );
        assert_eq!(fetch(&runner, now()).unwrap().metrics.len(), 4);
        let asked = runner.1.lock().unwrap().clone();
        assert_eq!(
            asked,
            [
                vec!["--version"],
                vec!["-p", "/usage", "--output-format", "json", "--print-timeout", "25s"]
            ]
        );
        let missing = Runner(Err(CommandError::NotFound), Default::default());
        assert_eq!(fetch(&missing, now()), Err(CodingError::NotInstalled));
    }

    /// Answers `--version` with `version`, and records whether `/usage` was ever asked.
    struct Versioned(&'static str, std::sync::atomic::AtomicBool);

    impl CommandRunner for Versioned {
        fn run(&self, _: &str, args: &[&str], _: StdDuration) -> Result<CommandOutput, CommandError> {
            if args != ["--version"] {
                self.1.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(CommandOutput {
                success: true,
                stdout: if args == ["--version"] { self.0 } else { USAGE }.to_owned(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn a_cli_older_than_1_1_11_or_of_unknown_version_is_never_asked_for_usage() {
        assert_eq!(version("1.3.3\n"), Some((1, 3, 3)));
        assert_eq!(version("agy v1.1.11-beta"), Some((1, 1, 11)));
        assert_eq!(version("1.3"), None);
        assert_eq!(version("1.3.3.4"), None);
        for (installed, safe) in [
            ("1.1.11", true),
            ("1.3.3", true),
            ("2.0.0", true),
            ("1.1.10", false),
            ("1.1.8", false),
            ("0.9.99", false),
            ("unknown", false),
            ("", false),
        ] {
            let runner = Versioned(installed, Default::default());
            let result = fetch(&runner, now());
            let asked = runner.1.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(asked, safe, "{installed}");
            if !safe {
                assert_eq!(result, Err(CodingError::TooOld), "{installed}");
            }
        }
    }
}
