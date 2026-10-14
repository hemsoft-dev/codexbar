//! Claude accounts CodexBar signs in itself (#80). Each one has its own Claude Code config folder in the settings
//! folder (`claude\<account id>`). Claude Code signs it in (in a console window of its own, because `claude auth
//! login` is a terminal UI), renews it with its own locked refresh, signs it out, and writes its `.credentials.json`
//! and `.claude.json`; CodexBar only reads them and holds no Anthropic client id (docs/OAUTH.md). An account on
//! Claude Code's own sign-in (`~/.claude`) is the import and fallback path.
//!
//! These accounts use the Browser session method: OAuth already means Claude Code's own sign-in for Claude.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codexbar_providers::claude_cli::{ClaudeCliError, LoginWindow};
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::settings_hub::SettingsHub;

/// How long the Claude Code window may stay open before the sign-in is given up.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// True for a Claude account CodexBar signs in, rather than one on Claude Code's own sign-in.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::CLAUDE) && record.method == AuthMethod::BrowserSession
}

/// The Claude Code config folder an account reads: its own under `dir` when CodexBar signs it in, else Claude Code's.
pub fn config_dir(dir: &Path, record: &AccountRecord) -> PathBuf {
    if is_managed(record) {
        dir.join("claude").join(crate::codex_sign_in::folder_name(&record.id))
    } else {
        codexbar_providers::claude::default_config_dir()
    }
}

/// Where Claude Code names an account's signed-in user.
pub fn profile_path(dir: &Path, record: &AccountRecord) -> PathBuf {
    if is_managed(record) {
        config_dir(dir, record).join(".claude.json")
    } else {
        codexbar_providers::claude::default_profile_path()
    }
}

pub fn credentials_path(dir: &Path, record: &AccountRecord) -> PathBuf {
    codexbar_providers::claude_cli::credentials_path(&config_dir(dir, record))
}

/// Deletes a removed account's config folder; only ever one CodexBar made, directly inside `claude\`.
pub fn delete_folder(dir: &Path, record: &AccountRecord) -> std::io::Result<()> {
    let folder = config_dir(dir, record);
    if !is_managed(record) || folder.parent() != Some(dir.join("claude").as_path()) {
        return Ok(());
    }
    match std::fs::remove_dir_all(folder) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Signs Claude config folders in and out. A seam, so UI tests never start Claude Code.
pub trait SignInService: Send + Sync {
    /// Opens the sign-in for `config`, unless `cancel` was set meanwhile.
    fn begin(&self, config: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, ClaudeCliError>;
    fn sign_out(&self, config: &Path) -> Result<(), ClaudeCliError>;
}

/// A sign-in open in a Claude Code window.
pub trait PendingSignIn: Send {
    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), ClaudeCliError>;
}

impl PendingSignIn for LoginWindow {
    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), ClaudeCliError> {
        LoginWindow::finish(*self, timeout, &cancel)
    }
}

struct ClaudeCode;

impl SignInService for ClaudeCode {
    fn begin(&self, config: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, ClaudeCliError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(ClaudeCliError::Cancelled);
        }
        Ok(Box::new(LoginWindow::open(config)?))
    }

    fn sign_out(&self, config: &Path) -> Result<(), ClaudeCliError> {
        codexbar_providers::claude_cli::sign_out(config)
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(ClaudeCode)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
}

fn service(cx: &App) -> Arc<dyn SignInService> {
    cx.global::<Service>().0.clone()
}

#[derive(Clone)]
enum Stage {
    Waiting,
    Failed(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs a CodexBar-managed Claude account in: opens a Claude Code window for its folder and waits for it, in a dialog
/// that closes that window when cancelled.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let dir = hub.dir().to_owned();
    let config = config_dir(&dir, &record);
    if let Err(err) = std::fs::create_dir_all(&config) {
        SettingsHub::set_error(
            cx,
            Some(format!("Couldn't create the account's Claude folder: {err}").into()),
        );
        return;
    }
    let stage = Rc::new(RefCell::new(Stage::Waiting));
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = Rc::new(CancelOnDrop(cancel.clone()));
    let label = record.label.clone();
    let shown = stage.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let _keep = &guard;
        let stage = shown.borrow().clone();
        let text = match &stage {
            Stage::Waiting => {
                "A Claude Code window opened for this account. Sign in to Claude there; this closes when you're done."
            }
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign in “{label}” to Claude"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("claude-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .when_some(
                        match &stage {
                            Stage::Failed(error) => Some(error.clone()),
                            Stage::Waiting => None,
                        },
                        |this, error| {
                            this.child(
                                div()
                                    .id("claude-sign-in-error")
                                    .role(Role::Alert)
                                    .test_support()
                                    .aria_label(error.clone())
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child(error),
                            )
                        },
                    ),
            )
            .footer(DialogFooter::new().child(DialogClose::new().trigger(|button| {
                button.outline().label(if matches!(stage, Stage::Failed(_)) {
                    "Close"
                } else {
                    "Cancel"
                })
            })))
    });

    let service = service(cx);
    let record_id = record.id.clone();
    window
        .spawn(cx, async move |cx| {
            let begun = {
                let (service, config, cancel) = (service.clone(), config.clone(), cancel.clone());
                cx.background_spawn(async move { service.begin(&config, &cancel) })
                    .await
            };
            let pending = match begun {
                Ok(pending) => pending,
                Err(ClaudeCliError::Cancelled) => return,
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    let _ = cx.update(|window, _| window.refresh());
                    return;
                }
            };
            let waiting = cancel.clone();
            let result = cx
                .background_spawn(async move { pending.finish(SIGN_IN_TIMEOUT, waiting) })
                .await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => match remember_identity(&record_id, &dir, cx) {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message);
                        window.refresh();
                    }
                },
                Err(ClaudeCliError::Cancelled) => {}
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Stores the Claude identity the folder is signed in to, as the account's dashboard account. Signing it in to
/// another identity replaces the one before, whose stored usage is then removed with it, so identities never mix.
fn remember_identity(record_id: &str, dir: &Path, cx: &mut App) -> Result<(), SharedString> {
    let Some(record) = SettingsHub::global(cx)
        .settings()
        .accounts()
        .iter()
        .find(|a| a.id == record_id)
        .cloned()
    else {
        return Err("The account was removed during the sign-in.".into());
    };
    let identity = codexbar_providers::claude::signed_in_identity(&profile_path(dir, &record))
        .map(|(id, _)| id.as_str().to_owned());
    if identity.is_some() && identity != record.external_id {
        SettingsHub::update(cx, |settings| {
            settings.upsert(AccountRecord {
                external_id: identity,
                ..record
            })
        })
        .map_err(|err| SharedString::from(format!("The sign-in couldn't be saved: {err}")))?;
    }
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Signs a managed account out. Its history stays, for when it is signed in again.
pub fn sign_out(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) {
        return;
    }
    let config = config_dir(hub.dir(), &record);
    let service = service(cx);
    window
        .spawn(cx, async move |cx| {
            let result = cx.background_spawn(async move { service.sign_out(&config) }).await;
            let _ = cx.update(|_, cx| match result {
                Ok(()) => SettingsHub::request_refresh(cx),
                Err(err) => SettingsHub::set_error(cx, Some(format!("Couldn't sign out: {err}").into())),
            });
        })
        .detach();
}

/// Signs a removed managed account out and deletes its folder, off the UI thread.
pub fn forget(record: AccountRecord, cx: &mut App) {
    if !is_managed(&record) {
        return;
    }
    let dir = SettingsHub::global(cx).dir().to_owned();
    let service = service(cx);
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move {
                let config = config_dir(&dir, &record);
                if codexbar_providers::claude::has_credentials(&codexbar_providers::claude_cli::credentials_path(
                    &config,
                )) {
                    let _ = service.sign_out(&config);
                }
                delete_folder(&dir, &record)
            })
            .await;
        if let Err(err) = result {
            cx.update(|cx| {
                SettingsHub::set_error(
                    cx,
                    Some(format!("The account was removed, but its Claude folder couldn't be deleted: {err}").into()),
                );
            });
        }
    })
    .detach();
}

/// What the account row says about the sign-in: its email, or that it isn't signed in.
pub fn describe(dir: &Path, record: &AccountRecord) -> String {
    if !codexbar_providers::claude::has_credentials(&credentials_path(dir, record)) {
        return "Not signed in".to_owned();
    }
    codexbar_providers::claude::signed_in_identity(&profile_path(dir, record))
        .and_then(|(_, email)| email)
        .unwrap_or_else(|| "Signed in".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_always_directly_inside_claude() {
        let dir = std::env::temp_dir().join(format!("codexbar-claude-homes-{}", std::process::id()));
        let record = |id: &str| AccountRecord {
            id: id.to_owned(),
            ..AccountRecord::new(names::CLAUDE, "Work", AuthMethod::BrowserSession)
        };
        assert_eq!(config_dir(&dir, &record("abc123")), dir.join("claude").join("abc123"));
        for id in ["../../Documents", r"C:\Users\Someone", "Work"] {
            assert_eq!(
                config_dir(&dir, &record(id)).parent(),
                Some(dir.join("claude").as_path()),
                "{id}"
            );
        }
        // Claude Code's own sign-in (OAuth or Automatic) is never a CodexBar folder, and is never deleted.
        let own = AccountRecord::new(names::CLAUDE, "Mine", AuthMethod::OAuth);
        assert!(!is_managed(&own));
        assert_eq!(delete_folder(&dir, &own).ok(), Some(()));
    }
}
