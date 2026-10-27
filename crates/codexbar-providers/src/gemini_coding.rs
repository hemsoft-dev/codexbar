//! Gemini coding quotas (#107): the Antigravity daily quota summary, read through Google's own Antigravity CLI
//! (`agy -p "/usage" --output-format json`, a documented script interface that runs no model turn). The CLI keeps its
//! own Google sign-in and renews it; CodexBar borrows no OAuth client and stores no token.
//!
//! Five buckets map to stable metrics by id and window, never by display name: `gemini-5h`, `gemini-weekly`, `3p-5h`
//! and `3p-weekly`, plus the independent `gemini-argon-wtu-weekly`. Percent used is `1 - remaining_fraction`, and resets come only from `reset_time`. A missing,
//! duplicate, malformed or expired bucket stays unavailable, and the valid ones still show.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use codexbar_core::Metric;
use serde_json::Value;

use crate::pace::elapsed_pace;
use crate::{CommandError, CommandOptions, CommandOutput, CommandRunner, SystemCommandRunner};

/// The CLI's usage command can take a moment to start; it never runs a model turn.
const TIMEOUT: StdDuration = StdDuration::from_secs(30);

/// One coding quota CodexBar shows: the bucket id and window it comes from, and its metric label.
const BUCKETS: [(&str, &str, &str, i64); 5] = [
    ("gemini-5h", "5h", "Gemini Models five-hour", 5),
    ("gemini-weekly", "weekly", "Gemini Models weekly", 7 * 24),
    ("gemini-argon-wtu-weekly", "weekly", "Argon weekly", 7 * 24),
    ("3p-5h", "5h", "Other models five-hour", 5),
    ("3p-weekly", "weekly", "Other models weekly", 7 * 24),
];

/// Antigravity's own switch for its background self-updater; only `true` turns it off (chief#43). The updater starts
/// without a console once its 15-minute check is due, and its `agy --version` child then gets a new, visible console
/// that no creation flag on the agy CodexBar starts can reach. Agy run in a terminal still updates itself.
pub const NO_AUTO_UPDATE: (&str, &str) = ("AGY_CLI_DISABLE_AUTO_UPDATE", "true");

/// A signed-out agy starts Google sign-in in the default browser unless it believes it runs over SSH; then it prints
/// the sign-in link and waits for a pasted code instead (#282). A background read must never open a browser, so agy
/// is told it runs over a loopback SSH session. The interactive sign-in (`gemini_login`) clears these again.
pub const NO_BROWSER: [(&str, &str); 2] = [
    ("SSH_CONNECTION", "127.0.0.1 50000 127.0.0.1 22"),
    ("SSH_CLIENT", "127.0.0.1 50000 22"),
];

/// What a signed-out agy prints to stderr before it waits for a sign-in nobody here can give (`Authentication
/// required. Please visit the URL to log in:`, or `Error: authentication required.` with no terminal).
const SIGN_IN_PROMPT: &str = "authentication required";

/// Runs the Antigravity CLI without its self-updater or browser sign-in: `SystemCommandRunner` with `NO_AUTO_UPDATE`
/// and `NO_BROWSER` set, any value inherited from this process replaced, and the run stopped as soon as agy asks for
/// sign-in. Its working folder stays this process's.
pub struct CliRunner;

impl CliRunner {
    fn options() -> CommandOptions {
        CommandOptions {
            cwd: None,
            remove_env: vec![NO_AUTO_UPDATE.0, NO_BROWSER[0].0, NO_BROWSER[1].0, "SSH_TTY"],
            env: vec![NO_AUTO_UPDATE, NO_BROWSER[0], NO_BROWSER[1]],
            output_limit: 8 * 1024 * 1024,
            stop_on_stderr: Some(SIGN_IN_PROMPT),
        }
    }
}

impl CommandRunner for CliRunner {
    fn run(&self, program: &str, args: &[&str], timeout: StdDuration) -> Result<CommandOutput, CommandError> {
        SystemCommandRunner.run_with_options(program, args, timeout, &Self::options())
    }
}

/// The Antigravity CLI to run: on PATH, or where its installer puts it.
pub fn cli() -> String {
    // Its installer's folder: `%LOCALAPPDATA%\agy\bin` on Windows; elsewhere it links `agy` into `~/.local/bin`.
    let installed = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .and_then(|base| crate::platform::program_in(&PathBuf::from(base).join("agy").join("bin"), "agy"))
    } else {
        crate::platform::find_program("agy")
    };
    match installed {
        Some(path) => path.to_string_lossy().into_owned(),
        None => "agy".to_owned(),
    }
}

/// Why the coding quotas couldn't be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodingError {
    NotInstalled,
    /// The CLI has no sign-in and asked for one, which only the Settings sign-in can give.
    SignedOut,
    /// The CLI is older than the first version whose `/usage` runs no model turn, or its version can't be read.
    TooOld,
    /// The CLI ran but didn't report usage: usually not signed in.
    NoUsage,
    TimedOut,
    Unknown,
}

impl CodingError {
    /// What the account shows for this failure.
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotInstalled => "Coding quotas need the Antigravity CLI (agy), signed in.",
            Self::SignedOut => "The Antigravity CLI is signed out. Sign in again from Settings > Accounts.",
            Self::TooOld => {
                "Coding quotas need Antigravity CLI 1.1.11 or newer, whose /usage runs no model turn. Update agy."
            }
            Self::NoUsage => "The Antigravity CLI didn't report coding usage. Sign in by running agy once.",
            Self::TimedOut => "The Antigravity CLI took too long to report coding usage.",
            Self::Unknown => "The Antigravity CLI reported coding usage in a shape CodexBar doesn't know.",
        }
    }

    /// Why a `/usage` run that didn't succeed failed: agy asking for sign-in, or anything else.
    fn of_failed_run(stderr: &str) -> Self {
        if stderr.to_ascii_lowercase().contains(SIGN_IN_PROMPT) {
            Self::SignedOut
        } else {
            Self::NoUsage
        }
    }
}

impl std::fmt::Display for CodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
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
    if value.get("status").and_then(Value::as_str) != Some("SUCCESS")
        || value.get("num_turns").and_then(Value::as_u64) != Some(0)
    {
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
                    length: Some(Duration::hours(hours)),
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
        return Err(CodingError::of_failed_run(&output.stderr));
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
    fn weekly_forecasts_and_sus_share_the_twelve_hour_maturity_boundary() {
        let input = include_str!("../fixtures/antigravity-weekly-age.json");
        let start: DateTime<Utc> = "2026-10-23T04:38:00Z".parse().unwrap();
        for age in [
            Duration::hours(12) - Duration::seconds(1),
            Duration::hours(12),
            Duration::hours(15) + Duration::minutes(52),
            Duration::hours(16) + Duration::minutes(48),
            Duration::days(3),
        ] {
            let now = start + age;
            let usage = parse(input, now).unwrap();
            for key in ["gemini-models-weekly", "argon-weekly", "other-models-weekly"] {
                let metric = usage.metrics.iter().find(|m| m.key() == key).unwrap();
                if age < Duration::hours(12) {
                    assert_eq!(metric.projected_at_reset(now), None);
                    assert_eq!(codexbar_core::sus::weekly(metric, now), None);
                } else {
                    let expected = metric.used_fraction().unwrap() * 168. / (age.num_seconds() as f64 / 3600.);
                    assert!((metric.projected_at_reset(now).unwrap() - expected).abs() < 1e-9);
                    assert!(codexbar_core::sus::weekly(metric, now).is_some());
                    if age == Duration::hours(12) && key == "gemini-models-weekly" {
                        assert!((expected - 0.42).abs() < 1e-9);
                        assert_eq!(codexbar_core::sus::weekly(metric, now).unwrap().value(), 42);
                    }
                }
            }
            for metric in usage.metrics.iter().filter(|m| m.label().contains("five-hour")) {
                assert_eq!(codexbar_core::sus::text(metric, now), None);
            }
        }
        let expired = parse(input, start + Duration::days(7)).unwrap();
        assert!(expired.metrics.is_empty());
        for invalid in [
            input.replace("0.97", "1.1"),
            input.replace("gemini-weekly", "missing-weekly"),
            input.replace(
                "\"id\": \"gemini-weekly\"",
                "\"disabled\": true, \"id\": \"gemini-weekly\"",
            ),
        ] {
            let usage = parse(&invalid, start + Duration::hours(12)).unwrap();
            assert!(!usage.metrics.iter().any(|m| m.key() == "gemini-models-weekly"));
        }
        let early_five_hour = parse(input, "2026-10-23T14:48:00Z".parse().unwrap()).unwrap();
        assert_eq!(
            early_five_hour.metrics[0].projected_at_reset(start),
            None,
            "five-hour maturity remains ten percent of its period"
        );
    }

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
        assert_eq!(usage.notes, ["Argon weekly: not reported by Antigravity."]);
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
    fn captured_cli_structure_exposes_gemini_argon_and_3p_as_separate_pools() {
        let text = include_str!("../fixtures/antigravity-usage-20261023.json");
        let now = "2026-10-23T14:00:00Z".parse().unwrap();
        let usage = parse(text, now).unwrap();
        assert_eq!(usage.metrics.len(), 5);
        assert!(usage.notes.is_empty());
        let argon = usage.metrics.iter().find(|m| m.label() == "Argon weekly").unwrap();
        assert!((argon.used_fraction().unwrap() - 0.17).abs() < 1e-9);
        assert_eq!(argon.window_length(), Some(Duration::days(7)));
        let malformed = text.replace("gemini-argon-wtu-weekly", "unknown-model");
        let partial = parse(&malformed, now).unwrap();
        assert_eq!(partial.metrics.len(), 4);
        assert!(
            partial
                .notes
                .iter()
                .any(|n| n == "Argon weekly: not reported by Antigravity.")
        );
        assert_eq!(
            parse(&text.replace("\"num_turns\": 0", "\"num_turns\": 1"), now),
            Err(CodingError::NoUsage)
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
                "Argon weekly: not reported by Antigravity.",
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
                r#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[]}}}"#,
                now()
            ),
            Err(CodingError::Unknown)
        );
    }

    #[test]
    fn codexbar_runs_the_cli_with_its_self_updater_and_browser_sign_in_off() {
        let options = CliRunner::options();
        assert_eq!(
            options.env,
            [
                ("AGY_CLI_DISABLE_AUTO_UPDATE", "true"),
                ("SSH_CONNECTION", "127.0.0.1 50000 127.0.0.1 22"),
                ("SSH_CLIENT", "127.0.0.1 50000 22"),
            ]
        );
        assert_eq!(
            options.remove_env,
            ["AGY_CLI_DISABLE_AUTO_UPDATE", "SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        );
        assert_eq!(options.stop_on_stderr, Some("authentication required"));
        assert_eq!(options.cwd, None);
        assert_eq!(options.output_limit, 8 * 1024 * 1024);
        assert_eq!(
            CliRunner.run(
                "codexbar-missing-agy-fixture",
                &["--version"],
                StdDuration::from_secs(3)
            ),
            Err(CommandError::NotFound)
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

    #[test]
    fn a_cli_that_asks_for_sign_in_is_reported_signed_out() {
        for stderr in [
            "Authentication required. Please visit the URL to log in:\n  https://accounts.google.com/o/oauth2/auth\n",
            "Error: authentication required. Run 'agy' to log in, then retry.\n",
        ] {
            let runner = Runner(
                Ok(CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: stderr.to_owned(),
                }),
                Default::default(),
            );
            assert_eq!(fetch(&runner, now()), Err(CodingError::SignedOut), "{stderr}");
        }
        let other = Runner(
            Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "error: something else".to_owned(),
            }),
            Default::default(),
        );
        assert_eq!(fetch(&other, now()), Err(CodingError::NoUsage));
    }

    #[test]
    fn every_coding_error_has_its_own_message() {
        let all = [
            CodingError::NotInstalled,
            CodingError::SignedOut,
            CodingError::TooOld,
            CodingError::NoUsage,
            CodingError::TimedOut,
            CodingError::Unknown,
        ];
        let messages: std::collections::HashSet<_> = all.iter().map(CodingError::message).collect();
        assert_eq!(messages.len(), all.len());
        for error in all {
            assert_eq!(error.to_string(), error.message());
            assert!(error.message().contains("Antigravity CLI"), "{error}");
        }
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
