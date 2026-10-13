//! The app-wide settings state: the shared settings file, the credential store, and the last save error.

use std::sync::Arc;

use codexbar_store::credentials::{CredentialStore, SecretSource, WindowsCredentialStore, resolve_secret};
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
}

impl Global for SettingsHub {}

impl SettingsHub {
    pub fn init(cx: &mut App) {
        Self::init_with(cx, &Settings::default_dir(), Arc::new(WindowsCredentialStore::new()));
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
        });
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
        }
    }

    /// The secret an account will use and where it comes from. The legacy plaintext key belongs to the provider's
    /// first account only, as in the WPF app.
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

    pub fn secret_for(&self, account: &AccountRecord) -> (Option<String>, SecretSource) {
        let info = catalog::info(&account.provider);
        let Some(spec) = &info.secret else {
            return (None, SecretSource::Missing);
        };
        if spec.saved_first
            && let Ok(Some(saved)) = codexbar_store::credentials::read_long(self.credentials.as_ref(), &account.id)
            && !saved.trim().is_empty()
        {
            return (Some(saved), SecretSource::CredentialManager);
        }
        let first = self.settings.accounts_for(info.id).next().map(|a| a.id.as_str());
        let is_first = first.is_none_or(|id| id == account.id);
        let legacy = is_first.then(|| self.settings.api_key(info.id)).flatten();
        let resolved = resolve_secret(Some(spec.env), self.credentials.as_ref(), &account.id, legacy);
        // A key moved from the file before the provider had accounts (#74) is kept under its implicit account; the
        // provider's first account goes on using it until a key is saved for that account.
        let implicit = codexbar_store::settings::legacy_id(info.id, "");
        if matches!(resolved.1, SecretSource::Missing)
            && is_first
            && account.id != implicit
            && let Ok(Some(secret)) = self.credentials.read(&implicit)
            && !secret.trim().is_empty()
        {
            return (Some(secret), SecretSource::CredentialManager);
        }
        resolved
    }
}

/// How the Settings screen describes a secret's source. Never includes the secret.
pub fn describe_source(source: &SecretSource) -> SharedString {
    match source {
        SecretSource::Environment(name) => format!("From the {name} environment variable").into(),
        SecretSource::CredentialManager => "Saved in Windows Credential Manager".into(),
        SecretSource::SettingsFile => "In the settings file; moves to Credential Manager at the next start".into(),
        SecretSource::Missing => "Not set".into(),
        SecretSource::StoreUnavailable(err) => format!("Credential Manager unavailable: {err}").into(),
    }
}
