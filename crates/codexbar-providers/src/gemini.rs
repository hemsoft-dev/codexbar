//! One Gemini account (#106, #107): its Gemini Apps limits from a website session, and room for the coding quotas a
//! separate connection adds. Each connection is set up on its own; a missing one shows as needing setup, never as zero.

use chrono::{DateTime, Utc};
use codexbar_core::{AccountId, AccountSnapshot, Provider};

use crate::gemini_apps::{self, AppsSession};
use crate::{AccountOutcome, HttpClient, ProviderError, UsageProvider};

const SETUP_HINT: &str = "Sign in to Gemini Apps in CodexBar settings.";

pub struct GeminiProvider<H: HttpClient> {
    http: H,
    apps: Option<AppsSession>,
    account: String,
    label: String,
}

impl<H: HttpClient> GeminiProvider<H> {
    /// `apps` is the account's saved Gemini Apps session, if it has one.
    pub fn new(http: H, apps: Option<AppsSession>, account: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            http,
            apps,
            account: account.into(),
            label: label.into(),
        }
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Result<AccountSnapshot, ProviderError> {
        let session = self
            .apps
            .as_ref()
            .ok_or(ProviderError::NotSignedIn { hint: SETUP_HINT })?;
        let usage = gemini_apps::fetch(&self.http, session, now)?;
        let metrics = gemini_apps::metrics(&usage, now);
        let mut account = AccountSnapshot::new(AccountId::new(self.account.clone()), Provider::Gemini, metrics, now);
        if !self.label.trim().is_empty() {
            account = account.with_label(self.label.trim());
        }
        // A window Gemini didn't return stays missing, and says so.
        for (missing, name) in [
            (usage.five_hour.is_none(), "five-hour"),
            (usage.weekly.is_none(), "weekly"),
        ] {
            if missing {
                account = account.with_message(format!("Gemini didn't return the Gemini Apps {name} limit."));
            }
        }
        Ok(account)
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
}
