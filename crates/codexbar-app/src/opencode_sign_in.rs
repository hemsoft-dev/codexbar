//! OpenCode accounts CodexBar signs in through the OpenCode Console's device approval (#102). CodexBar opens the
//! Console's own approval page in the browser, where the user picks the workspace; the resulting sign-in is kept in
//! Windows Credential Manager under the account. No cookie, token or workspace id is copied by hand. An account is bound
//! to the workspace and user it was first signed in to, so signing it in again can't silently switch either; a second
//! workspace is a second account. Signing out deletes CodexBar's copy only; it doesn't revoke access at OpenCode.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use codexbar_providers::UreqClient;
use codexbar_providers::oauth::{Browser as _, SystemBrowser};
use codexbar_providers::opencode_console::{self, ConsoleCredential, CredentialSlot, SignInError, SlotUnavailable};
use codexbar_store::credentials::CredentialStore;
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::settings_hub::SettingsHub;

/// True for an OpenCode account CodexBar signs in through the Console, rather than one on a dashboard cookie.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::OPENCODE_GO) && record.method == AuthMethod::OAuth
}

/// The account's saved sign-in: `Ok(None)` when it isn't signed in (or holds something else, such as an old cookie),
/// an error when Credential Manager couldn't be read.
pub fn credential(
    hub: &SettingsHub,
    record: &AccountRecord,
) -> Result<Option<ConsoleCredential>, codexbar_store::credentials::CredentialError> {
    Ok(
        codexbar_store::credentials::read_long(hub.credentials().as_ref(), &record.id)?
            .and_then(|secret| ConsoleCredential::from_secret(&secret)),
    )
}

/// An account's sign-in in Credential Manager, for the provider to read and to save renewals into.
pub struct Slot {
    pub store: Arc<dyn CredentialStore>,
    pub account: String,
}

impl CredentialSlot for Slot {
    fn load(&self) -> Result<Option<String>, SlotUnavailable> {
        codexbar_store::credentials::read_long(self.store.as_ref(), &self.account).map_err(|_| SlotUnavailable)
    }

    fn save(&self, secret: &str) -> Result<(), SlotUnavailable> {
        codexbar_store::credentials::write_long(self.store.as_ref(), &self.account, secret).map_err(|_| SlotUnavailable)
    }
}

/// What the dialog shows while the user approves: the page and the code it displays.
#[derive(Clone, Debug)]
pub struct Approval {
    pub code: Option<String>,
    pub url: String,
}

/// Runs the device approval. A seam, so UI tests never reach OpenCode or open a browser.
pub trait SignInService: Send + Sync {
    /// Asks the Console for an approval and opens its page, unless `cancel` was set meanwhile.
    fn begin(&self, cancel: &AtomicBool) -> Result<Box<dyn PendingApproval>, SignInError>;
}

/// An approval waiting for the user on the Console page.
pub trait PendingApproval: Send {
    fn approval(&self) -> Approval;
    /// Waits for the approval and confirms the workspace and user it belongs to.
    fn finish(self: Box<Self>, cancel: Arc<AtomicBool>) -> Result<ConsoleCredential, SignInError>;
}

struct Console;

struct ConsolePending {
    http: UreqClient,
    challenge: opencode_console::DeviceChallenge,
}

impl SignInService for Console {
    fn begin(&self, cancel: &AtomicBool) -> Result<Box<dyn PendingApproval>, SignInError> {
        // Redirects are never followed: every exchange must stay on the Console.
        let http = UreqClient::without_redirects();
        let challenge = opencode_console::begin(&http, chrono::Utc::now())?;
        if cancel.load(Ordering::SeqCst) {
            return Err(SignInError::Cancelled);
        }
        // A browser that doesn't open isn't fatal: the dialog has the page.
        let _ = SystemBrowser.open(&challenge.url);
        Ok(Box::new(ConsolePending { http, challenge }))
    }
}

impl PendingApproval for ConsolePending {
    fn approval(&self) -> Approval {
        Approval {
            code: self.challenge.user_code.clone(),
            url: self.challenge.url.clone(),
        }
    }

    fn finish(self: Box<Self>, cancel: Arc<AtomicBool>) -> Result<ConsoleCredential, SignInError> {
        opencode_console::wait_for_approval(
            &self.http,
            &self.challenge,
            &cancel,
            &chrono::Utc::now,
            &opencode_console::cancellable_sleep,
        )
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(Console)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
}

#[derive(Clone)]
enum Stage {
    Starting,
    Waiting(Approval),
    Failed(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs an OpenCode account in, in a dialog that shows the Console's code and can be cancelled. Closing the dialog
/// ends the sign-in, even one OpenCode has already approved: nothing is saved.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let stage = Rc::new(RefCell::new(Stage::Starting));
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = Rc::new(CancelOnDrop(cancel.clone()));
    let label = record.label.clone();
    let shown = stage.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let _keep = &guard;
        let stage = shown.borrow().clone();
        let text = match &stage {
            Stage::Starting => "Asking OpenCode for an approval code…",
            Stage::Waiting(_) => {
                "Approve CodexBar on the OpenCode page that opened in your browser, and choose the workspace to track. \
                 Check that the page shows this code."
            }
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign “{label}” in to OpenCode"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("opencode-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .when_some(
                        match &stage {
                            Stage::Waiting(approval) => Some(approval.clone()),
                            _ => None,
                        },
                        |this, approval| {
                            let url = approval.url.clone();
                            this.child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .when_some(approval.code.clone(), |this, code| {
                                        this.child(
                                            div()
                                                .id("opencode-sign-in-code")
                                                .role(Role::Note)
                                                .test_support()
                                                .aria_label(format!("Approval code {code}"))
                                                .text_2xl()
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .child(code),
                                        )
                                    })
                                    .child(
                                        Button::new("opencode-sign-in-open")
                                            .small()
                                            .outline()
                                            .label("Open OpenCode")
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
                                    .id("opencode-sign-in-error")
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
                cx.background_spawn(async move { service.begin(&cancel) }).await
            };
            let pending = match begun {
                Ok(pending) => pending,
                Err(SignInError::Cancelled) => return,
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    let _ = cx.update(|window, _| window.refresh());
                    return;
                }
            };
            *stage.borrow_mut() = Stage::Waiting(pending.approval());
            let _ = cx.update(|window, _| window.refresh());
            let waiting = cancel.clone();
            let result = cx.background_spawn(async move { pending.finish(waiting) }).await;
            // Closed after OpenCode approved but before this ran: the user cancelled, so nothing is kept.
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let _ = cx.update(|window, cx| match result {
                Ok(credential) => match remember(&record_id, &credential, cx) {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message);
                        window.refresh();
                    }
                },
                Err(SignInError::Cancelled) => {}
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Keeps the sign-in for the account and binds the account to its workspace and user. An account already bound to
/// another workspace or user refuses it, as does a workspace another account already tracks. The sign-in is written
/// before the account, and put back as it was when the account can't be saved, so a failure changes nothing.
fn remember(record_id: &str, credential: &ConsoleCredential, cx: &mut App) -> Result<(), SharedString> {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return Err("The account was removed during the sign-in. Nothing was saved.".into());
    };
    if !is_managed(&record) {
        return Err("The account no longer signs in through OpenCode. Nothing was saved.".into());
    }
    // A user is only ever recorded by a Console sign-in, so an account that has one is bound to it and its workspace.
    if let Some(user) = &record.external_id {
        if *user != credential.user_id {
            return Err(
                "This account was signed in by a different OpenCode user. Add another account to track this one."
                    .into(),
            );
        }
        if let Some(workspace) = record.workspace_id.as_deref().filter(|w| *w != credential.workspace_id) {
            return Err(format!(
                "This account tracks workspace {workspace}. To track {}, add another OpenCode account.",
                credential.workspace_id
            )
            .into());
        }
    }
    if let Some(other) = hub.settings().accounts().iter().find(|other| {
        // Any OpenCode Go account, a cookie one included: two accounts for one workspace would show it twice.
        other.id != record.id
            && other.provider.eq_ignore_ascii_case(names::OPENCODE_GO)
            && other.workspace_id.as_deref() == Some(&credential.workspace_id)
    }) {
        return Err(format!(
            "Workspace {} is already added as “{}”.",
            credential.workspace_id, other.label
        )
        .into());
    }
    let store = hub.credentials();
    let previous = codexbar_store::credentials::read_long(store.as_ref(), &record.id)
        .map_err(|err| SharedString::from(format!("The saved sign-in couldn't be read: {err}")))?;
    codexbar_store::credentials::write_long(store.as_ref(), &record.id, &credential.to_secret())
        .map_err(|err| SharedString::from(format!("The sign-in couldn't be saved: {err}")))?;
    let id = record.id.clone();
    let result = SettingsHub::update(cx, |settings| {
        settings.upsert(AccountRecord {
            external_id: Some(credential.user_id.clone()),
            workspace_id: Some(credential.workspace_id.clone()),
            ..record
        })
    });
    if let Err(err) = result {
        let _ = match previous {
            Some(previous) => codexbar_store::credentials::write_long(store.as_ref(), &id, &previous),
            None => codexbar_store::credentials::delete_long(store.as_ref(), &id),
        };
        return Err(format!("The sign-in couldn't be saved: {err}").into());
    }
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Signs an account out: CodexBar's saved sign-in is deleted. The account, its workspace and its history stay, for
/// signing in again. Access isn't revoked at OpenCode.
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

/// Deletes what CodexBar kept for an account that was removed or left this sign-in.
pub fn forget(record: AccountRecord, cx: &mut App) {
    if let Err(err) =
        codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), &record.id)
    {
        SettingsHub::set_error(
            cx,
            Some(format!("The account's OpenCode sign-in couldn't be deleted: {err}").into()),
        );
    }
}

/// Which workspace the account row says it tracks, and whether it is signed in.
pub fn describe(hub: &SettingsHub, record: &AccountRecord) -> String {
    match (credential(hub, record), &record.workspace_id) {
        (Err(_), _) => "Its sign-in couldn't be read from Windows Credential Manager".to_owned(),
        (Ok(Some(credential)), _) => format!("Workspace {}", credential.workspace_id),
        (Ok(None), Some(workspace)) if record.external_id.is_some() => format!("Workspace {workspace} · Not signed in"),
        (Ok(None), _) => "Not signed in".to_owned(),
    }
}
