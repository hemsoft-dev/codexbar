//! What a dashboard failure row offers when an account isn't signed in (#337), beside Open settings (#188) and Retry.
//! An account CodexBar signs in itself gets that sign-in right on the row. An account on a CLI's own sign-in gets the
//! CLI's sign-in, run in a console window of its own, and, where the provider has one, a way to move the account to
//! CodexBar's own sign-in. Nothing starts without a click.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use codexbar_providers::cli_sign_in::{Cli, CliSignInError, SignInWindow};
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::{App, Global};

use crate::catalog::ProviderInfo;
use crate::managed::Managed;

/// How long a CLI's sign-in window may stay open before CodexBar stops waiting for it.
const CLI_SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What the row offers.
#[derive(Clone, Debug, PartialEq)]
pub enum Help {
    /// CodexBar signs this account in itself: the same sign-in as the Settings row's button.
    Managed { kind: Managed, record: String },
    /// The account reads a CLI's own sign-in: run it. `switch` is the account, when CodexBar can sign it in instead.
    Cli { cli: Cli, switch: Option<AccountRecord> },
    /// The account reads a sign-in CodexBar can't start (the Cursor app's), but CodexBar can sign it in instead.
    Switch(AccountRecord),
}

/// The help for a failed `info` account, `record` when one is configured. `None` for accounts whose key or cookie is
/// entered in Settings, which Open settings already covers.
pub fn help(info: &ProviderInfo, record: Option<&AccountRecord>) -> Option<Help> {
    if let Some(record) = record
        && let Some(kind) = Managed::of(record)
    {
        return Some(Help::Managed {
            kind,
            record: record.id.clone(),
        });
    }
    let method = record.map_or(info.default_method, |record| record.method);
    // CodexBar's own sign-in for this provider, when it has one; offered only for a configured account, whose
    // dialog changes the method.
    let switch = record.filter(|_| has_own_sign_in(info.id)).cloned();
    let cli = match (info.id, method) {
        (names::COPILOT, AuthMethod::Automatic | AuthMethod::CommandLine) => Some(Cli::GitHub),
        (names::CODEX, AuthMethod::Automatic) => Some(Cli::Codex),
        (names::CLAUDE, AuthMethod::Automatic | AuthMethod::OAuth) => Some(Cli::Claude),
        (names::GROK, AuthMethod::Automatic) => Some(Cli::Grok),
        _ => None,
    };
    match (cli, switch) {
        (Some(cli), switch) => Some(Help::Cli { cli, switch }),
        (None, Some(record)) => Some(Help::Switch(record)),
        (None, None) => None,
    }
}

/// Providers CodexBar can sign in itself, besides their CLI's or app's own sign-in.
fn has_own_sign_in(provider: &str) -> bool {
    [names::CODEX, names::COPILOT, names::CLAUDE, names::CURSOR, names::GROK].contains(&provider)
}

/// The button that starts a CodexBar sign-in.
pub fn managed_label(kind: Managed) -> &'static str {
    if kind == Managed::Gemini {
        "Connect Antigravity…"
    } else {
        "Sign in…"
    }
}

/// Runs a CLI's sign-in until it finishes. A seam, so UI tests never start a CLI.
pub trait CliSignIn: Send + Sync {
    fn sign_in(&self, cli: Cli) -> Result<(), CliSignInError>;
}

struct ConsoleSignIn;

impl CliSignIn for ConsoleSignIn {
    fn sign_in(&self, cli: Cli) -> Result<(), CliSignInError> {
        SignInWindow::open(cli)?.finish(CLI_SIGN_IN_TIMEOUT, &AtomicBool::new(false))
    }
}

pub struct Service(pub Arc<dyn CliSignIn>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(ConsoleSignIn)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn CliSignIn>) {
    cx.set_global(Service(service));
}

/// The service that runs CLI sign-ins; the console one unless a test set another.
pub fn service(cx: &App) -> Arc<dyn CliSignIn> {
    cx.try_global::<Service>().map_or_else(
        || Arc::new(ConsoleSignIn) as Arc<dyn CliSignIn>,
        |service| service.0.clone(),
    )
}

/// What the row says after a CLI's sign-in didn't work.
pub fn cli_failure(cli: Cli, error: &CliSignInError) -> String {
    let name = cli.name();
    let mut name = name.to_owned();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    match error {
        CliSignInError::NotInstalled => format!("{name} isn't installed."),
        CliSignInError::Failed => format!(
            "{name} didn't finish signing in. Try again, or run `{}` in a terminal.",
            cli.command_line()
        ),
        CliSignInError::Start(reason) => format!("{name} couldn't start: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(provider: &str, method: AuthMethod) -> AccountRecord {
        AccountRecord::new(provider, "Work", method)
    }

    #[test]
    fn an_account_codexbar_signs_in_gets_that_sign_in() {
        let codex = record(names::CODEX, AuthMethod::OAuth);
        assert_eq!(
            help(crate::catalog::info(names::CODEX), Some(&codex)),
            Some(Help::Managed {
                kind: Managed::Codex,
                record: codex.id.clone(),
            })
        );
        let gemini = record(names::GEMINI, AuthMethod::Automatic);
        let Some(Help::Managed { kind, .. }) = help(crate::catalog::info(names::GEMINI), Some(&gemini)) else {
            panic!("Gemini connects Antigravity");
        };
        assert_eq!(managed_label(kind), "Connect Antigravity…");
        assert_eq!(managed_label(Managed::GitHub), "Sign in…");
    }

    #[test]
    fn a_copilot_account_on_the_github_cli_gets_its_sign_in_and_codexbar_s() {
        let copilot = record(names::COPILOT, AuthMethod::CommandLine);
        assert_eq!(
            help(crate::catalog::info(names::COPILOT), Some(&copilot)),
            Some(Help::Cli {
                cli: Cli::GitHub,
                switch: Some(copilot),
            })
        );
    }

    #[test]
    fn a_provider_with_no_account_record_gets_its_cli_but_no_switch() {
        assert_eq!(
            help(crate::catalog::info(names::COPILOT), None),
            Some(Help::Cli {
                cli: Cli::GitHub,
                switch: None,
            })
        );
        assert_eq!(
            help(crate::catalog::info(names::CODEX), None),
            Some(Help::Cli {
                cli: Cli::Codex,
                switch: None,
            })
        );
    }

    #[test]
    fn each_cli_backed_method_runs_its_own_cli() {
        for (provider, method, cli) in [
            (names::CODEX, AuthMethod::Automatic, Cli::Codex),
            (names::CLAUDE, AuthMethod::Automatic, Cli::Claude),
            (names::CLAUDE, AuthMethod::OAuth, Cli::Claude),
            (names::GROK, AuthMethod::Automatic, Cli::Grok),
            (names::COPILOT, AuthMethod::Automatic, Cli::GitHub),
        ] {
            let account = record(provider, method);
            assert!(
                matches!(help(crate::catalog::info(provider), Some(&account)), Some(Help::Cli { cli: found, switch: Some(_) }) if found == cli),
                "{provider} {method:?}"
            );
        }
    }

    #[test]
    fn a_cursor_account_on_the_app_s_sign_in_can_only_switch() {
        let cursor = record(names::CURSOR, AuthMethod::Automatic);
        assert_eq!(
            help(crate::catalog::info(names::CURSOR), Some(&cursor)),
            Some(Help::Switch(cursor))
        );
    }

    #[test]
    fn a_key_or_cookie_account_gets_nothing_beyond_open_settings() {
        let openrouter = record(names::OPENROUTER, AuthMethod::ApiKey);
        assert_eq!(help(crate::catalog::info(names::OPENROUTER), Some(&openrouter)), None);
    }

    #[test]
    fn a_failed_cli_sign_in_says_what_happened_and_what_to_run() {
        assert_eq!(
            cli_failure(Cli::GitHub, &CliSignInError::NotInstalled),
            "The GitHub CLI isn't installed."
        );
        assert_eq!(
            cli_failure(Cli::Codex, &CliSignInError::Failed),
            "The Codex CLI didn't finish signing in. Try again, or run `codex login` in a terminal."
        );
        assert_eq!(
            cli_failure(Cli::Claude, &CliSignInError::Start("access denied".into())),
            "Claude Code couldn't start: access denied"
        );
    }
}
