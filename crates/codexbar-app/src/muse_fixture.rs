//! Synthetic-only acquisition seam used by tests and the native demo capture.
use chrono::{DateTime, Duration, Utc};
use codexbar_providers::{
    CommandError, CommandOptions, CommandOutput, CommandRunner, UsageProvider, muse::MuseProvider,
};
use std::time::Duration as Timeout;

pub(crate) fn base() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1793037245064).unwrap()
}
pub(crate) fn now(state: &str) -> DateTime<Utc> {
    if state == "reset-after" {
        base() + Duration::minutes(3)
    } else if state == "stale" {
        base() + Duration::minutes(8)
    } else {
        base() + Duration::minutes(1)
    }
}
struct Fixture {
    text: String,
    failure: Option<CommandError>,
    success: bool,
}
impl CommandRunner for Fixture {
    fn run(&self, _: &str, _: &[&str], _: Timeout) -> Result<CommandOutput, CommandError> {
        panic!("unscoped command")
    }
    fn run_with_options(
        &self,
        program: &str,
        args: &[&str],
        timeout: Timeout,
        options: &CommandOptions,
    ) -> Result<CommandOutput, CommandError> {
        assert_eq!(program, "muse");
        assert_eq!(args, ["usage", "--json"]);
        assert_eq!(timeout, Timeout::from_secs(15));
        assert_eq!(options.output_limit, 4 * 1024 * 1024);
        assert!(options.remove_env.contains(&"META_API_KEY"));
        if let Some(error) = self.failure {
            return Err(error);
        }
        Ok(CommandOutput {
            success: self.success,
            stdout: self.text.clone(),
            stderr: "synthetic Fixture User@example.com".into(),
        })
    }
}
pub(crate) fn provider(state: &str) -> impl UsageProvider + use<> {
    let text = match state {
        "zero" => include_str!("../../codexbar-providers/fixtures/muse/zero.json"),
        "exhausted" => include_str!("../../codexbar-providers/fixtures/muse/exhausted.json"),
        "partial" => include_str!("../../codexbar-providers/fixtures/muse/partial.json"),
        "malformed" => include_str!("../../codexbar-providers/fixtures/muse/malformed-values.json"),
        "unsupported" => include_str!("../../codexbar-providers/fixtures/muse/unsupported-schema.json"),
        "null" | "payg-unknown" | "expired-entitlement-unknown" => {
            include_str!("../../codexbar-providers/fixtures/muse/null-allowance.json")
        }
        "token-activity-only" => {
            r#"{"schemaVersion":1,"allowance":{"asOfMs":1793037245064},"run":{"usage":{"tokens":987654}},"model":"wonder-full"}"#
        }
        _ => include_str!("../../codexbar-providers/fixtures/muse/measured.json"),
    };
    let text = if state == "reset-before" {
        let mut payload: serde_json::Value = serde_json::from_str(text).unwrap();
        payload["allowance"]["window"]["resetsAtMs"] =
            serde_json::json!((base() + Duration::minutes(2)).timestamp_millis());
        payload["allowance"]["window"]["usedPercent"] = serde_json::json!(95);
        payload.to_string()
    } else {
        text.to_owned()
    };
    let failure = match state {
        "timeout" => Some(CommandError::TimedOut),
        "transport" => Some(CommandError::Failed),
        "missing-cli" => Some(CommandError::NotFound),
        _ => None,
    };
    MuseProvider::new(
        Fixture {
            text,
            failure,
            success: state != "rate-command-failure",
        },
        std::env::temp_dir(),
        "muse-demo".into(),
        "Demo".into(),
    )
}
