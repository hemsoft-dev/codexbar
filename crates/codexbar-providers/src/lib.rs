//! Provider adapters. Each reads the credentials its provider's own client already maintains, fetches usage over
//! HTTPS, and maps the response onto `codexbar_core` snapshots. Fetches are blocking; callers run them off the UI thread.

pub mod balance;
pub mod browser_session;
pub mod claude;
pub mod claude_cli;
pub mod codex;
pub mod codex_app_server;
mod command;
mod contained;
pub mod copilot;
pub mod cursor;
pub mod cursor_cli;
mod error;
pub mod gemini;
pub mod gemini_apps;
pub mod gemini_coding;
pub mod gemini_login;
pub mod gh_login;
pub mod github_allowances;
pub mod github_billing;
pub mod greptile;
pub mod grok;
mod http;
mod jwt;
pub mod oauth;
pub mod opencode;
pub mod opencode_console;
mod pace;
pub mod platform;
pub mod x_subscription;

pub use command::{CommandError, CommandOutput, CommandRunner, SystemCommandRunner};
pub use error::ProviderError;
pub use http::{HttpClient, HttpResponse, UreqClient};

use chrono::{DateTime, Utc};
use codexbar_core::{AccountId, AccountSnapshot};

/// The result for one account of a provider (#75). An adapter that serves several accounts reports each one, so a
/// failure for one doesn't hide it among the others' successes.
#[derive(Debug)]
pub enum AccountOutcome {
    Fresh(AccountSnapshot),
    Failed {
        /// The account's stable id, as a successful snapshot of it would carry.
        account: AccountId,
        label: Option<String>,
        error: ProviderError,
    },
}

/// One source of accounts.
pub trait UsageProvider: Send + Sync {
    /// Stable identifier used for logging and error rows.
    fn name(&self) -> &'static str;

    /// Fetches every account this provider can see at `now`.
    fn fetch(&self, now: DateTime<Utc>) -> Result<Vec<AccountSnapshot>, ProviderError>;

    /// Fetches every account with a result per account. `Err` means the provider as a whole failed (no sign-in,
    /// network). Adapters for several accounts override this to report each account's failure; the default wraps
    /// `fetch`.
    fn fetch_outcomes(&self, now: DateTime<Utc>) -> Result<Vec<AccountOutcome>, ProviderError> {
        self.fetch(now)
            .map(|accounts| accounts.into_iter().map(AccountOutcome::Fresh).collect())
    }

    /// The configured account this adapter reports, when it serves exactly one (several OpenRouter or Moonshot
    /// accounts each get their own adapter under the same name). Lets the dashboard tell their results apart.
    fn account_id(&self) -> Option<&str> {
        None
    }

    /// The configured account's label, when this adapter serves one labelled account ("Team").
    fn account_label(&self) -> Option<&str> {
        None
    }

    /// The one account this adapter's sign-in belongs to right now, when it can tell without fetching (Cursor, #81).
    /// Lets saved results of another account be set aside at startup.
    fn signed_in_account(&self) -> Option<AccountId> {
        None
    }
}
