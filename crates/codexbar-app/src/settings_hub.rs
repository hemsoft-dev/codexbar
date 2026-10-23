//! The app-wide settings state: the shared settings file, the credential store, and the last save error.

use std::sync::Arc;

use codexbar_store::credentials::{CredentialStore, STORE_NAME, STORE_SHORT_NAME, SecretSource, SystemCredentialStore};
use codexbar_store::settings::{AccountRecord, Settings, SettingsError, legacy_id};
use gpui_kit::{App, BorrowAppContext as _, Global, SharedString};

use crate::catalog;

pub struct SettingsHub {
    settings: Settings,
    /// The folder settings were loaded from; a failed save reloads from here, never from `~/.codexbar` directly.
    dir: std::path::PathBuf,
    /// A schema problem found at load: the screen stays read-only so the file is never overwritten.
    load_error: Option<SettingsError>,
    credentials: Arc<dyn CredentialStore>,
    error: Option<SharedString>,
    /// Bumped on every saved change, so work started under older settings can tell (an in-flight refresh).
    revision: u64,
    /// Bumped when something outside the settings file changed what providers return (a sign-in, #78).
    refresh_requests: u64,
    /// Reads an environment variable; tests supply their own values so they never depend on the host's (#186).
    environment: EnvLookup,
}

/// Reads one environment variable by name.
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

impl Global for SettingsHub {}

impl SettingsHub {
    pub fn init(cx: &mut App) {
        Self::init_with(cx, &Settings::default_dir(), Arc::new(SystemCredentialStore::new()));
    }

    /// Loads settings from `dir` with the given credential store; tests pass a temporary folder and an in-memory
    /// store, so they never touch `~/.codexbar` or Windows Credential Manager.
    pub fn init_with(cx: &mut App, dir: &std::path::Path, credentials: Arc<dyn CredentialStore>) {
        let (mut settings, load_error) = match Settings::load(dir) {
            Ok(settings) => (settings, None),
            Err(err) => (Settings::load_or_default(dir), Some(err)),
        };
        // Plaintext keys an older CodexBar left in the file move to Credential Manager (#74). Only from a file this
        // version can save; a key that couldn't move stays usable and is tried again at the next start.
        let error = load_error
            .is_none()
            .then(|| {
                let providers: Vec<&str> = crate::catalog::PROVIDERS
                    .iter()
                    .filter(|info| info.secret.is_some())
                    .map(|info| info.id)
                    .collect();
                codexbar_store::settings::move_plaintext_keys(&mut settings, credentials.as_ref(), &providers)
            })
            .filter(|outcome| !outcome.failed.is_empty())
            .map(|outcome| {
                let providers: Vec<&str> = outcome.failed.iter().map(|(provider, _)| *provider).collect();
                let reason = &outcome.failed[0].1;
                SharedString::from(format!(
                    "Keys for {} are still in the settings file: {reason} CodexBar tries again at the next start.",
                    providers.join(", ")
                ))
            });
        cx.set_global(Self {
            settings,
            dir: dir.to_owned(),
            load_error,
            credentials,
            error,
            revision: 0,
            refresh_requests: 0,
            environment: Arc::new(|name| std::env::var(name).ok()),
        });
    }

    /// Makes the hub read environment variables through `lookup` instead of the process environment, so a test can
    /// set a provider's variable without touching the host's (which may hold a real key).
    #[cfg(test)]
    pub fn use_environment(cx: &mut App, lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
        cx.update_global(|hub: &mut Self, _| hub.environment = Arc::new(lookup));
        cx.refresh_windows();
    }

    /// Asks the dashboard to fetch again, after a sign-in changed what a provider can read.
    pub fn request_refresh(cx: &mut App) {
        cx.update_global(|hub: &mut Self, _| hub.refresh_requests += 1);
        cx.refresh_windows();
    }

    /// Changes whenever a refresh is requested.
    pub fn refresh_requests(cx: &App) -> u64 {
        Self::global(cx).refresh_requests
    }

    /// Changes whenever settings are saved.
    pub fn revision(cx: &App) -> u64 {
        cx.try_global::<Self>().map_or(0, |hub| hub.revision)
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// The folder settings were loaded from; Rust-only preferences live next to them.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn credentials(&self) -> Arc<dyn CredentialStore> {
        self.credentials.clone()
    }

    pub fn is_read_only(&self) -> bool {
        self.load_error.is_some()
    }

    /// The message to show at the top of Settings: a load problem, or the last failed save.
    pub fn notice(&self) -> Option<SharedString> {
        self.load_error
            .as_ref()
            .map(|err| err.to_string().into())
            .or_else(|| self.error.clone())
    }

    /// Applies an edit and saves under the shared lock. On failure the file is reloaded, so the screen shows what
    /// is really saved, and the error is kept for display.
    pub fn update(
        cx: &mut App,
        edit: impl FnOnce(&mut Settings) -> Result<(), SettingsError>,
    ) -> Result<(), SettingsError> {
        let result = cx.update_global(|hub: &mut Self, _| {
            if let Some(err) = &hub.load_error {
                return Err(match err {
                    SettingsError::NewerVersion => SettingsError::NewerVersion,
                    SettingsError::AmbiguousProviderKeys => SettingsError::AmbiguousProviderKeys,
                    _ => SettingsError::InvalidAccounts,
                });
            }
            let mut draft = hub.settings.clone();
            let result = edit(&mut draft).and_then(|()| draft.save());
            match &result {
                Ok(()) => {
                    hub.settings = draft;
                    hub.error = None;
                    hub.revision += 1;
                }
                Err(err) => {
                    hub.error = Some(err.to_string().into());
                    if let Ok(fresh) = Settings::load(&hub.dir) {
                        hub.settings = fresh;
                    }
                }
            }
            result
        });
        cx.refresh_windows();
        result
    }

    pub fn set_error(cx: &mut App, error: Option<SharedString>) {
        cx.update_global(|hub: &mut Self, _| hub.error = error);
        cx.refresh_windows();
    }

    /// The account record that stands for a provider when it has none yet (legacy single-account behavior).
    pub fn implicit_account(provider: &str) -> AccountRecord {
        let info = catalog::info(provider);
        AccountRecord {
            id: legacy_id(info.id, ""),
            provider: info.id.to_owned(),
            label: info.id.to_owned(),
            enabled: true,
            method: info.default_method,
            external_id: None,
            workspace_id: None,
            legacy_card_key: Some(info.id.to_owned()),
            copilot_enterprise: None,
            copilot_organization: None,
            copilot_pool_total: None,
            billing_organization: None,
            gemini_coding: false,
            sus_price: None,
        }
    }

    /// An account's browser sign-in tokens (#77), from Credential Manager. `Ok(None)` means signed out; an error
    /// means Credential Manager couldn't be read, which is not the same and mustn't prompt a new sign-in.
    #[allow(
        dead_code,
        reason = "used by provider sign-in once a client registration is approved (docs/OAUTH.md)"
    )]
    pub fn tokens_for(
        &self,
        account_id: &str,
    ) -> Result<Option<codexbar_providers::oauth::TokenSet>, codexbar_store::credentials::CredentialError> {
        let secret = codexbar_store::credentials::read_long(self.credentials.as_ref(), account_id)?;
        Ok(secret.and_then(|secret| codexbar_providers::oauth::TokenSet::from_secret(&secret)))
    }

    /// Stores an account's tokens after a sign-in or renewal. Tokens can be longer than one credential, so they are
    /// split across parts; the previous tokens stay readable until the new ones are fully written.
    #[allow(
        dead_code,
        reason = "used by provider sign-in once a client registration is approved (docs/OAUTH.md)"
    )]
    pub fn save_tokens(
        &self,
        account_id: &str,
        tokens: &codexbar_providers::oauth::TokenSet,
    ) -> Result<(), codexbar_store::credentials::CredentialError> {
        codexbar_store::credentials::write_long(self.credentials.as_ref(), account_id, &tokens.to_secret())
    }

    /// The secret an account will use and where it comes from (#186): the key saved for the account, else a key
    /// moved from the settings file before the provider had accounts (kept under the implicit account and belonging
    /// to the provider's first account, #74), else the provider's environment variable, else the legacy plaintext key
    /// in the settings file (the first account's only, as in the WPF app). A variable thus only stands in for an
    /// account with no saved key, and saving one makes it the key in use. When the store can't be read, no inherited
    /// key is guessed at: the variable still works, otherwise the failure is reported with the legacy key, if any.
    pub fn secret_for(&self, account: &AccountRecord) -> (Option<String>, SecretSource) {
        let info = catalog::info(&account.provider);
        let Some(spec) = &info.secret else {
            return (None, SecretSource::Missing);
        };
        let store = self.credentials.as_ref();
        let first = self.settings.accounts_for(info.id).next().map(|a| a.id.as_str());
        let is_first = first.is_none_or(|id| id == account.id);
        let legacy = is_first.then(|| self.settings.api_key(info.id)).flatten();
        let env = (self.environment)(spec.env)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let unavailable = |err| match env.clone() {
            Some(value) => (Some(value), SecretSource::Environment(spec.env)),
            None => (legacy.clone(), SecretSource::StoreUnavailable(err)),
        };
        match codexbar_store::credentials::read_long(store, &account.id) {
            Ok(Some(saved)) if !saved.trim().is_empty() => return (Some(saved), SecretSource::CredentialManager),
            Ok(_) => {}
            Err(err) => return unavailable(err),
        }
        let implicit = codexbar_store::settings::legacy_id(info.id, "");
        if is_first && account.id != implicit {
            match store.read(&implicit) {
                Ok(Some(secret)) if !secret.trim().is_empty() => {
                    return (Some(secret), SecretSource::CredentialManager);
                }
                Ok(_) => {}
                Err(err) => return unavailable(err),
            }
        }
        // Nothing saved for the account (an incomplete chunked secret counts as nothing): the variable, else the
        // legacy plaintext key still in the settings file.
        if let Some(value) = env {
            return (Some(value), SecretSource::Environment(spec.env));
        }
        match legacy {
            Some(secret) => (Some(secret), SecretSource::SettingsFile),
            None => (None, SecretSource::Missing),
        }
    }

    /// Where an account's secret would come from if the dialog saved nothing for it: an existing account's current
    /// source; for a new account, what the provider's first account inherits (#74) or the environment variable.
    /// Never a secret.
    pub fn prospective_secret_source(&self, provider: &str, existing: Option<&AccountRecord>) -> SecretSource {
        let info = catalog::info(provider);
        let prospective = existing
            .cloned()
            .unwrap_or_else(|| AccountRecord::new(info.id, "", info.setup_method));
        self.secret_for(&prospective).1
    }
}

/// How the Settings screen describes a secret's source. Never includes the secret.
pub fn describe_source(source: &SecretSource) -> SharedString {
    match source {
        SecretSource::Environment(name) => format!("From the {name} environment variable").into(),
        SecretSource::CredentialManager => format!("Saved in {STORE_NAME}").into(),
        SecretSource::SettingsFile => {
            format!("In the settings file; moves to {STORE_SHORT_NAME} at the next start").into()
        }
        SecretSource::Missing => "Not set".into(),
        SecretSource::StoreUnavailable(err) => {
            let store = if cfg!(windows) {
                STORE_SHORT_NAME
            } else {
                "The keychain"
            };
            format!("{store} unavailable: {err}").into()
        }
    }
}
