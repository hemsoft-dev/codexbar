//! Copilot accounts CodexBar signs in itself (#79). The GitHub CLI runs its own browser sign-in in a private config
//! folder (`gh_login`), so the user's `gh` accounts and keyring are left alone; CodexBar keeps the resulting token in
//! Windows Credential Manager under the account. GitHub CLI tokens don't expire; signing out deletes CodexBar's copy.
//! Accounts using the GitHub CLI's own sign-in (Automatic or Command line) are the import and fallback path.
//!
//! GitHub Billing accounts (#104) sign in the same way, with their own token kept under their own account, so a billing
//! sign-in never reads, widens or replaces a Copilot one. Before it is saved, the token must read the chosen owner's
//! billing: the signed-in user's own, which needs GitHub's `user` scope, or an organization's.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codexbar_providers::gh_login::{DeviceCode, GhAccount, GhLoginError, PendingLogin};
use codexbar_providers::oauth::{Browser as _, SystemBrowser};
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::settings_hub::SettingsHub;

/// How long the device code stays good on GitHub's side, roughly; the sign-in is given up after it.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// True for a Copilot account CodexBar signs in, rather than one using the GitHub CLI's own sign-in, and for every
/// GitHub Billing account (#104), which only signs in this way.
pub fn is_managed(record: &AccountRecord) -> bool {
    (record.provider.eq_ignore_ascii_case(names::COPILOT) && record.method == AuthMethod::OAuth) || is_billing(record)
}

/// True for a GitHub Billing account (#104).
pub fn is_billing(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::GITHUB_BILLING)
}

/// The organization a billing account follows; `None` follows the signed-in user.
pub fn billing_organization(record: &AccountRecord) -> Option<&str> {
    record
        .billing_organization
        .as_deref()
        .map(str::trim)
        .filter(|org| !org.is_empty())
}

/// The owner whose billing a billing account reads, once known: its organization, or the user it signed in as.
pub fn billing_owner(record: &AccountRecord) -> Option<codexbar_providers::github_billing::Owner> {
    use codexbar_providers::github_billing::Owner;
    match billing_organization(record) {
        Some(org) => Some(Owner::Organization(org.to_owned())),
        None => record
            .external_id
            .as_deref()
            .filter(|user| !user.trim().is_empty())
            .map(|user| Owner::Personal(user.to_owned())),
    }
}

/// The token CodexBar keeps for a managed account: `Ok(None)` when it isn't signed in, an error when Credential
/// Manager couldn't be read (which is not the same as signed out).
pub fn token(
    hub: &SettingsHub,
    record: &AccountRecord,
) -> Result<Option<String>, codexbar_store::credentials::CredentialError> {
    Ok(
        codexbar_store::credentials::read_long(hub.credentials().as_ref(), &record.id)?
            .filter(|token| !token.trim().is_empty()),
    )
}

/// The scope enterprise billing needs beyond the GitHub CLI's own, asked for when the account has org billing set.
const BILLING_SCOPE: &str = "manage_billing:enterprise";

/// The extra scopes a sign-in for `record` asks for. A user's own billing needs `user`; an organization's is read with
/// the GitHub CLI's `repo`, so it asks for nothing more.
fn scopes(record: &AccountRecord) -> Vec<&'static str> {
    if is_billing(record) {
        return match billing_organization(record) {
            Some(_) => Vec::new(),
            None => vec![codexbar_providers::github_billing::PERSONAL_SCOPE],
        };
    }
    let set = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.trim().is_empty());
    if set(&record.copilot_enterprise) && set(&record.copilot_organization) {
        vec![BILLING_SCOPE]
    } else {
        Vec::new()
    }
}

/// Deletes private GitHub CLI folders an earlier run left behind (stopped mid sign-in), so no token stays on disk.
pub fn clean_up(cx: &mut App) {
    if let Err(err) = codexbar_providers::gh_login::clean_up(SettingsHub::global(cx).dir()) {
        SettingsHub::set_error(
            cx,
            Some(format!("A folder from an unfinished GitHub sign-in couldn't be deleted: {err}").into()),
        );
    }
}

/// Signs GitHub accounts in. A seam, so UI tests never start the GitHub CLI or a browser.
pub trait SignInService: Send + Sync {
    /// Starts a sign-in with a private config folder inside `parent`, asking for `scopes` beyond the GitHub CLI's
    /// own, and opens GitHub's device page unless `cancel` was set meanwhile. (`gh --web` doesn't open it itself when
    /// not run in a terminal; it only prints it.)
    fn begin(
        &self,
        parent: &Path,
        scopes: &[&str],
        cancel: &AtomicBool,
    ) -> Result<Box<dyn PendingSignIn>, GhLoginError>;

    /// For a billing account (#104): checks that `token` reads `organization`'s billing, or its own user's when `None`,
    /// and returns that user's login. The message on failure is shown as is.
    fn verify_billing(&self, token: &str, organization: Option<&str>) -> Result<String, String>;
}

/// A sign-in waiting for the user to enter the code on GitHub.
pub trait PendingSignIn: Send {
    fn device(&self) -> DeviceCode;
    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<GhAccount, GhLoginError>;
}

impl PendingSignIn for PendingLogin {
    fn device(&self) -> DeviceCode {
        PendingLogin::device(self).clone()
    }

    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<GhAccount, GhLoginError> {
        PendingLogin::finish(*self, timeout, &cancel)
    }
}

struct GitHubCli;

impl SignInService for GitHubCli {
    fn begin(
        &self,
        parent: &Path,
        scopes: &[&str],
        cancel: &AtomicBool,
    ) -> Result<Box<dyn PendingSignIn>, GhLoginError> {
        let login = PendingLogin::start(parent, scopes)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(GhLoginError::Cancelled);
        }
        // A browser that doesn't open isn't fatal: the dialog has the page and the code.
        let _ = SystemBrowser.open(&login.device().url);
        Ok(Box::new(login))
    }

    fn verify_billing(&self, token: &str, organization: Option<&str>) -> Result<String, String> {
        codexbar_providers::github_billing::verify(
            &codexbar_providers::UreqClient::new(),
            token,
            organization,
            chrono::Utc::now(),
        )
        .map_err(|err| err.to_string())
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(GitHubCli)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
}

#[derive(Clone)]
enum Stage {
    Starting,
    Waiting(DeviceCode),
    Failed(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs a CodexBar-managed Copilot account in, in a dialog that shows GitHub's one-time code and can be cancelled.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let parent = hub.dir().to_owned();
    let scopes = scopes(&record);
    let billing = is_billing(&record).then(|| billing_organization(&record).map(str::to_owned));
    let stage = Rc::new(RefCell::new(Stage::Starting));
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = Rc::new(CancelOnDrop(cancel.clone()));
    let label = record.label.clone();
    let shown = stage.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let _keep = &guard;
        let stage = shown.borrow().clone();
        let text = match &stage {
            Stage::Starting => "Starting the GitHub CLI…",
            Stage::Waiting(_) => {
                "Enter this code on the GitHub page that opened in your browser, then approve the GitHub CLI."
            }
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign in “{label}” to GitHub"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("github-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .when_some(
                        match &stage {
                            Stage::Waiting(device) => Some(device.clone()),
                            _ => None,
                        },
                        |this, device| {
                            let code = device.code.clone();
                            let url = device.url.clone();
                            this.child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        div()
                                            .id("github-sign-in-code")
                                            .role(Role::Note)
                                            .test_support()
                                            .aria_label(format!("One-time code {}", device.code))
                                            .text_2xl()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .child(device.code.clone()),
                                    )
                                    .child(
                                        Button::new("github-sign-in-copy")
                                            .small()
                                            .outline()
                                            .label("Copy code")
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                            }),
                                    )
                                    .child(
                                        Button::new("github-sign-in-open")
                                            .small()
                                            .outline()
                                            .label("Open GitHub")
                                            .on_click(move |_, _, _| {
                                                let _ = SystemBrowser.open(&url);
                                            }),
                                    ),
                            )
                        },
                    )
                    .when_some(
                        match &stage {
                            Stage::Failed(error) => Some(error.clone()),
                            _ => None,
                        },
                        |this, error| {
                            this.child(
                                div()
                                    .id("github-sign-in-error")
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

    let service = cx.global::<Service>().0.clone();
    let record_id = record.id.clone();
    window
        .spawn(cx, async move |cx| {
            let begun = {
                let cancel = cancel.clone();
                cx.background_spawn(async move { service.begin(&parent, &scopes, &cancel) })
                    .await
            };
            let pending = match begun {
                Ok(pending) => pending,
                Err(GhLoginError::Cancelled) => return,
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    let _ = cx.update(|window, _| window.refresh());
                    return;
                }
            };
            *stage.borrow_mut() = Stage::Waiting(pending.device());
            let _ = cx.update(|window, _| window.refresh());
            let waiting = cancel.clone();
            let result = cx
                .background_spawn(async move { pending.finish(SIGN_IN_TIMEOUT, waiting) })
                .await;
            // Closed after GitHub approved but before this ran: the user cancelled, so nothing is kept.
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            // A billing sign-in must read the chosen owner's billing before it is kept (#104).
            let result = match (result, billing) {
                (Ok(account), Some(organization)) => {
                    let service = cx.update(|_, cx| cx.global::<Service>().0.clone());
                    let Ok(service) = service else {
                        return;
                    };
                    let token = account.token.clone();
                    let verified = cx
                        .background_spawn(async move { service.verify_billing(&token, organization.as_deref()) })
                        .await;
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    match verified {
                        Ok(login) => Ok(GhAccount {
                            username: login,
                            token: account.token,
                        }),
                        Err(message) => {
                            *stage.borrow_mut() = Stage::Failed(message.into());
                            let _ = cx.update(|window, _| window.refresh());
                            return;
                        }
                    }
                }
                (result, _) => result,
            };
            let _ = cx.update(|window, cx| match result {
                Ok(account) => match remember(&record_id, &account, cx) {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message);
                        window.refresh();
                    }
                },
                Err(GhLoginError::Cancelled) => {}
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Keeps the token and the GitHub username for the account. Another account already holding that user is refused, so
/// one GitHub account never shows twice. The token is written before the username, so a failed write changes nothing.
fn remember(record_id: &str, account: &GhAccount, cx: &mut App) -> Result<(), SharedString> {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return Err("The account was removed during the sign-in.".into());
    };
    if is_billing(&record) {
        // An account following a user's own billing stays that user's: its history is theirs. Another user's billing
        // is another account.
        if billing_organization(&record).is_none()
            && let Some(user) = record.external_id.as_deref()
            && !user.eq_ignore_ascii_case(&account.username)
        {
            return Err(format!(
                "This account follows {user}'s billing, not {}'s. Add another account for {}.",
                account.username, account.username
            )
            .into());
        }
        // One billing account per owner: the organization, or the user's own billing.
        let owner = billing_organization(&record).unwrap_or(&account.username).to_owned();
        if let Some(other) = hub.settings().accounts().iter().find(|other| {
            other.id != record.id
                && is_billing(other)
                && billing_owner(other).is_some_and(|o| o.login().eq_ignore_ascii_case(&owner))
        }) {
            return Err(format!("{owner}'s billing is already added as “{}”.", other.label).into());
        }
    } else if let Some(other) = hub.settings().accounts().iter().find(|other| {
        other.id != record.id
            && other.provider.eq_ignore_ascii_case(names::COPILOT)
            && other
                .external_id
                .as_deref()
                .is_some_and(|user| user.eq_ignore_ascii_case(&account.username))
    }) {
        return Err(format!("{} is already added as “{}”.", account.username, other.label).into());
    }
    let store = hub.credentials();
    let previous = codexbar_store::credentials::read_long(store.as_ref(), &record.id)
        .map_err(|err| SharedString::from(format!("The saved token couldn't be read: {err}")))?;
    codexbar_store::credentials::write_long(store.as_ref(), &record.id, &account.token)
        .map_err(|err| SharedString::from(format!("The token couldn't be saved: {err}")))?;
    let id = record.id.clone();
    let result = SettingsHub::update(cx, |settings| {
        settings.upsert(AccountRecord {
            external_id: Some(account.username.clone()),
            ..record
        })
    });
    if let Err(err) = result {
        // The account still names the previous user, so it keeps the previous user's token: a token without its
        // username would credit one user's usage to another.
        let _ = match previous {
            Some(previous) => codexbar_store::credentials::write_long(store.as_ref(), &id, &previous),
            None => codexbar_store::credentials::delete_long(store.as_ref(), &id),
        };
        return Err(format!("The sign-in couldn't be saved: {err}").into());
    }
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Signs a managed account out: CodexBar's token is deleted. The account and its history stay, for signing in again.
pub fn sign_out(record_id: &str, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) {
        return;
    }
    if let Err(err) = codexbar_store::credentials::delete_long(hub.credentials().as_ref(), &record.id) {
        SettingsHub::set_error(cx, Some(format!("Couldn't sign out: {err}").into()));
        return;
    }
    SettingsHub::request_refresh(cx);
}

/// Who the account row says the account is signed in as, and for a billing account whose billing it reads.
pub fn describe(hub: &SettingsHub, record: &AccountRecord) -> String {
    let signed_in = describe_user(hub, record);
    match (is_billing(record), billing_organization(record)) {
        (true, Some(org)) => format!("{org} (organization) · {signed_in}"),
        (true, None) => format!("Own billing · {signed_in}"),
        (false, _) => signed_in,
    }
}

fn describe_user(hub: &SettingsHub, record: &AccountRecord) -> String {
    match (&record.external_id, token(hub, record)) {
        (_, Err(_)) => "Its token couldn't be read from Windows Credential Manager".to_owned(),
        (Some(user), Ok(Some(_))) => user.clone(),
        (Some(user), Ok(None)) => format!("{user} · Not signed in"),
        (None, Ok(_)) => "Not signed in".to_owned(),
    }
}
