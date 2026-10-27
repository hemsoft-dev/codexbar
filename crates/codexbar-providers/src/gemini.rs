//! One Gemini account (#106, #107): its Gemini Apps limits from a website session, and its coding quotas from the
//! Antigravity CLI. Each connection is set up on its own and fails on its own; a missing one says so, never zero.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Provider};

use crate::gemini_apps::{self, AppsSession};
use crate::gemini_coding::{self, CodingError};
use crate::{AccountOutcome, CommandRunner, HttpClient, ProviderError, UsageProvider};

const SETUP_HINT: &str =
    "Connect Antigravity in Settings > Accounts. New Gemini Apps website sign-in is unavailable here.";

pub struct GeminiProvider<H: HttpClient> {
    http: H,
    apps: Option<AppsSession>,
    /// Runs the Antigravity CLI when the account shows coding quotas.
    coding: Option<Arc<dyn CommandRunner>>,
    account: String,
    label: String,
    unreadable: bool,
}

fn coding_error(error: &CodingError) -> ProviderError {
    let reason = match error {
        // Shown as signed out, never as a failed read, so the row points at the Settings sign-in (#282).
        CodingError::SignedOut => {
            return ProviderError::NotSignedIn {
                hint: "Sign in to the Antigravity CLI from Settings > Accounts.",
            };
        }
        CodingError::NotInstalled => "Coding quotas need the Antigravity CLI (agy), signed in.",
        CodingError::TooOld => {
            "Coding quotas need Antigravity CLI 1.1.11 or newer, whose /usage runs no model turn. Update agy."
        }
        CodingError::NoUsage => "The Antigravity CLI didn't report coding usage. Sign in by running agy once.",
        CodingError::TimedOut => "The Antigravity CLI took too long to report coding usage.",
        CodingError::Unknown => "The Antigravity CLI reported coding usage in a shape CodexBar doesn't know.",
    };
    ProviderError::Refused { reason }
}

impl<H: HttpClient> GeminiProvider<H> {
    /// `apps` is the account's saved Gemini Apps session, if it has one.
    pub fn new(http: H, apps: Option<AppsSession>, account: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            http,
            apps,
            coding: None,
            account: account.into(),
            label: label.into(),
            unreadable: false,
        }
    }

    /// The account's saved session couldn't be read: reported as such, not taken for signed out, so nobody signs in
    /// again over a session that is still there.
    pub fn with_unreadable_session(mut self) -> Self {
        self.unreadable = true;
        self
    }

    /// Also shows the coding quotas of the Antigravity CLI's sign-in, run through `runner`.
    pub fn with_coding(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.coding = Some(runner);
        self
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let apps = if self.unreadable {
            Some(Err(ProviderError::Unexpected {
                detail: concat!(
                    "its saved Gemini Apps session couldn't be read from ",
                    codexbar_core::secret_store_name!()
                ),
            }))
        } else {
            self.apps
                .as_ref()
                .map(|session| gemini_apps::fetch(&self.http, session, now))
        };
        let coding = self
            .coding
            .as_ref()
            .map(|runner| gemini_coding::fetch(runner.as_ref(), now));
        let mut metrics = Vec::new();
        let mut messages = Vec::new();
        let mut first_error = None;
        match &apps {
            Some(Ok(usage)) => {
                metrics.extend(gemini_apps::metrics(usage, now));
                // A window Gemini didn't return stays missing, and says so.
                for (missing, name) in [
                    (usage.five_hour.is_none(), "five-hour"),
                    (usage.weekly.is_none(), "weekly"),
                ] {
                    if missing {
                        messages.push(format!("Gemini didn't return the Gemini Apps {name} limit."));
                    }
                }
            }
            Some(Err(error)) => {
                messages.push(format!("Gemini Apps: {error}"));
                first_error = Some(error.clone());
            }
            None if self.coding.is_some() => messages.push(
                "Gemini Apps website usage is separate and not connected; new website sign-in is unavailable here."
                    .to_owned(),
            ),
            None => {}
        }
        match &coding {
            Some(Ok(usage)) => {
                metrics.extend(usage.metrics.iter().cloned());
                messages.extend(usage.notes.iter().cloned());
                // Every bucket disabled, expired or malformed: with nothing else to show, the account fails for that
                // reason, not with the Gemini Apps sign-in hint.
                if usage.metrics.is_empty() {
                    first_error.get_or_insert(ProviderError::Refused {
                        reason: "The Antigravity CLI reported no coding quota CodexBar can show.",
                    });
                }
            }
            Some(Err(error)) => {
                messages.push(format!("Coding quotas: {error}"));
                first_error.get_or_insert_with(|| coding_error(error));
            }
            None => {}
        }
        if metrics.is_empty() {
            return Err(first_error.unwrap_or(ProviderError::NotSignedIn { hint: SETUP_HINT }));
        }
        let mut account = AccountSnapshot::new(AccountId::new(self.account.clone()), Provider::Gemini, metrics, now);
        if self.coding.is_some() {
            account = account.with_subscription_price(codexbar_core::sus::Price::fixed(1999));
        }
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        Ok(messages.into_iter().fold(account, AccountSnapshot::with_message))
    }
}

impl<H: HttpClient> UsageProvider for GeminiProvider<H> {
    fn name(&self) -> &'static str {
        "Gemini"
    }

    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError> {
        self.snapshot(now).map(|account| vec![account])
    }

    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        Ok(vec![match self.snapshot(now) {
            Ok(account) => AccountOutcome::Fresh(account),
            Err(error) => AccountOutcome::Failed {
                account: AccountId::new(self.account.clone()),
                label: Some(self.label.clone()).filter(|l| !l.trim().is_empty()),
                error,
            },
        }])
    }

    fn account_id(&self) -> Option<&str> {
        Some(&self.account)
    }

    fn account_label(&self) -> Option<&str> {
        Some(&self.label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Offline;

    impl HttpClient for Offline {
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<crate::HttpResponse, ProviderError> {
            Err(ProviderError::Network)
        }
    }

    #[test]
    fn without_a_session_the_account_needs_setup() {
        let provider = GeminiProvider::new(Offline, None, "gm-1", "Personal");
        let outcome = provider
            .fetch_outcomes("2026-10-12T00:00:00Z".parse().unwrap())
            .unwrap();
        assert!(matches!(
            &outcome[0],
            AccountOutcome::Failed { account, error: ProviderError::NotSignedIn { .. }, .. } if account.as_str() == "gm-1"
        ));
    }

    struct Agy(Result<crate::CommandOutput, crate::CommandError>);

    impl CommandRunner for Agy {
        fn run(
            &self,
            _: &str,
            args: &[&str],
            _: std::time::Duration,
        ) -> Result<crate::CommandOutput, crate::CommandError> {
            if args == ["--version"] {
                return self.0.clone().map(|_| crate::CommandOutput {
                    success: true,
                    stdout: "1.3.3".to_owned(),
                    stderr: String::new(),
                });
            }
            self.0.clone()
        }
    }

    fn agy(stdout: &str) -> Arc<dyn CommandRunner> {
        Arc::new(Agy(Ok(crate::CommandOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })))
    }

    const USAGE: &str = r#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[{"buckets":[
        {"id":"gemini-5h","window":"5h","remaining_fraction":0.5,"reset_time":"2026-10-12T03:00:00Z"},
        {"id":"gemini-weekly","window":"weekly","remaining_fraction":0.9,"reset_time":"2026-10-15T00:00:00Z"},
        {"id":"3p-5h","window":"5h","remaining_fraction":1,"reset_time":"2026-10-12T03:00:00Z"},
        {"id":"3p-weekly","window":"weekly","remaining_fraction":0.8,"reset_time":"2026-10-15T00:00:00Z"}]}]}}}"#;

    fn now() -> DateTime<Utc> {
        "2026-10-12T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_signed_out_cli_shows_the_account_as_not_signed_in() {
        let runner = Arc::new(Agy(Ok(crate::CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: "Authentication required. Please visit the URL to log in:\n".to_owned(),
        })));
        let outcome = GeminiProvider::new(Offline, None, "gm-1", "Personal")
            .with_coding(runner)
            .fetch_outcomes(now())
            .unwrap();
        let AccountOutcome::Failed { error, .. } = &outcome[0] else {
            panic!("a signed-out CLI has no usage to show");
        };
        assert!(error.needs_sign_in());
        assert_eq!(
            error.to_string(),
            "Not signed in. Sign in to the Antigravity CLI from Settings > Accounts."
        );
    }

    #[test]
    fn partial_or_late_cli_results_never_use_argon_for_subscription_status() {
        let at: DateTime<Utc> = "2026-10-23T16:38:00Z".parse().unwrap();
        let original: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/antigravity-weekly-age.json")).unwrap();
        for mode in ["missing", "present", "disabled", "invalid", "expired", "present"] {
            let mut input = original.clone();
            let buckets = input
                .pointer_mut("/command/data/groups/0/buckets")
                .unwrap()
                .as_array_mut()
                .unwrap();
            if mode == "missing" {
                buckets.retain(|b| b["id"] == "gemini-argon-wtu-weekly");
            } else {
                let model = buckets.iter_mut().find(|b| b["id"] == "gemini-weekly").unwrap();
                match mode {
                    "disabled" => model["disabled"] = true.into(),
                    "invalid" => model["remaining_fraction"] = 1.2.into(),
                    "expired" => model["reset_time"] = "2026-10-23T16:38:00Z".into(),
                    _ => {}
                }
            }
            let accounts = GeminiProvider::new(Offline, None, "gemini", "Test")
                .with_coding(agy(&input.to_string()))
                .fetch(at)
                .unwrap();
            let account = &accounts[0];
            assert!(account.metrics().iter().any(|m| m.key() == "argon-weekly"));
            assert_eq!(account.status_metrics().count(), usize::from(mode == "present"));
            assert_eq!(account.assess(at).severity(), codexbar_core::Severity::Normal);
            assert_eq!(
                codexbar_core::sus::subscription_score(account, at).map(|s| s.value()),
                (mode == "present").then_some(42)
            );
        }
    }

    #[test]
    fn an_unreadable_session_is_reported_not_taken_for_signed_out() {
        let provider = GeminiProvider::new(Offline, None, "gm-1", "Personal").with_unreadable_session();
        let outcome = provider.fetch_outcomes(now()).unwrap();
        assert!(matches!(
            &outcome[0],
            AccountOutcome::Failed { error: ProviderError::Unexpected { detail }, .. } if detail.contains("couldn't be read")
        ));
        // Beside coding quotas it is the Gemini Apps part's note, and the quotas still show.
        let provider = GeminiProvider::new(Offline, None, "gm-1", "Personal")
            .with_unreadable_session()
            .with_coding(agy(USAGE));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 4);
        assert!(
            account.messages()[0].starts_with("Gemini Apps:") && account.messages()[0].contains("couldn't be read")
        );
    }

    #[test]
    fn coding_quotas_show_without_a_gemini_apps_session() {
        let provider = GeminiProvider::new(Offline, None, "gm-1", "Personal").with_coding(agy(USAGE));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 4);
        assert_eq!(
            account.messages(),
            [
                "Gemini Apps website usage is separate and not connected; new website sign-in is unavailable here.",
                "Argon weekly: not reported by Antigravity."
            ]
        );
    }

    #[test]
    fn coding_quotas_with_no_usable_bucket_fail_for_that_reason() {
        let all_disabled = USAGE.replace(r#""remaining_fraction""#, r#""disabled":true,"remaining_fraction""#);
        let provider = GeminiProvider::new(Offline, None, "gm-1", "Personal").with_coding(agy(&all_disabled));
        assert_eq!(
            provider.fetch(now()).unwrap_err(),
            ProviderError::Refused {
                reason: "The Antigravity CLI reported no coding quota CodexBar can show."
            },
            "never the Gemini Apps sign-in hint"
        );
    }

    #[test]
    fn one_source_failing_never_hides_the_other() {
        let session = AppsSession {
            psid: "g.abc".into(),
            psidts: None,
        };
        // Gemini Apps can't be reached; coding still shows.
        let provider = GeminiProvider::new(Offline, Some(session), "gm-1", "Personal").with_coding(agy(USAGE));
        let account = provider.fetch(now()).unwrap().remove(0);
        assert_eq!(account.metrics().len(), 4);
        assert!(account.messages()[0].starts_with("Gemini Apps: Couldn't reach"));
        // Coding can't be read and there is nothing else: the account fails with the coding reason.
        let broken = GeminiProvider::new(Offline, None, "gm-1", "Personal")
            .with_coding(Arc::new(Agy(Err(crate::CommandError::NotFound))));
        assert_eq!(
            broken.fetch(now()).unwrap_err(),
            ProviderError::Refused {
                reason: "Coding quotas need the Antigravity CLI (agy), signed in."
            }
        );
    }
}
