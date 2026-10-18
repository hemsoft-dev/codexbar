//! Cursor accounts CodexBar signs in itself (#81). Each one has its own folder in the settings folder
//! (`cursor\<account id>`), which `cursor-agent` uses as its `APPDATA`, so its sign-in never mixes with the Cursor
//! app's or another account's. CodexBar opens the sign-in page `cursor-agent` prints, waits for it, and remembers the
//! account's identity and email; signing a signed-in account in again asks first, since it replaces that sign-in.
//! An account on the Cursor app's own sign-in (Automatic) is the fallback.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codexbar_providers::cursor_cli::{CursorCliError, PendingLogin};
use codexbar_providers::oauth::{Browser as _, SystemBrowser};
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::dialog::{DialogButtonProps, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use serde_json::{Value, json};

use crate::settings_hub::SettingsHub;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// What CodexBar keeps beside a folder's sign-in: the account's email, which the token doesn't carry.
const ACCOUNT_FILE: &str = "codexbar-account.json";

/// True for a Cursor account CodexBar signs in, rather than one on the Cursor app's own sign-in.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::CURSOR) && record.method == AuthMethod::OAuth
}

/// The folder `cursor-agent` uses as `APPDATA` for an account CodexBar signs in.
pub fn home(dir: &Path, record: &AccountRecord) -> PathBuf {
    dir.join("cursor").join(crate::codex_sign_in::folder_name(&record.id))
}

/// The sign-in an account reads: its own folder's when CodexBar signs it in, else the Cursor app's.
pub fn auth_path(dir: &Path, record: &AccountRecord) -> PathBuf {
    if is_managed(record) {
        codexbar_providers::cursor_cli::auth_path(&home(dir, record))
    } else {
        codexbar_providers::cursor::default_auth_path()
    }
}

/// The email remembered for a managed account's sign-in.
fn email(dir: &Path, record: &AccountRecord) -> Option<String> {
    let text = std::fs::read_to_string(home(dir, record).join(ACCOUNT_FILE)).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("email")?
        .as_str()
        .map(str::to_owned)
}

/// Deletes a removed account's folder; only ever one CodexBar made, directly inside `cursor\`.
pub fn delete_home(dir: &Path, record: &AccountRecord) -> std::io::Result<()> {
    let folder = home(dir, record);
    if !is_managed(record) || folder.parent() != Some(dir.join("cursor").as_path()) {
        return Ok(());
    }
    match std::fs::remove_dir_all(folder) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Signs Cursor folders in and out. A seam, so UI tests never start `cursor-agent` or a browser.
pub trait SignInService: Send + Sync {
    /// Starts a sign-in for `home` and opens its page, unless `cancel` was set meanwhile.
    fn begin(&self, home: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, CursorCliError>;
    fn sign_out(&self, home: &Path) -> Result<(), CursorCliError>;
    /// The signed-in email, when the CLI says.
    fn email(&self, home: &Path) -> Option<String>;
}

pub trait PendingSignIn: Send {
    fn url(&self) -> String;
    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), CursorCliError>;
}

impl PendingSignIn for PendingLogin {
    fn url(&self) -> String {
        PendingLogin::url(self).to_owned()
    }

    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), CursorCliError> {
        PendingLogin::finish(*self, timeout, &cancel)
    }
}

struct CursorCli;

impl SignInService for CursorCli {
    fn begin(&self, home: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, CursorCliError> {
        let login = PendingLogin::start(home, cancel)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(CursorCliError::Cancelled);
        }
        // A browser that doesn't open isn't fatal: the dialog has the page.
        let _ = SystemBrowser.open(login.url());
        Ok(Box::new(login))
    }

    fn sign_out(&self, home: &Path) -> Result<(), CursorCliError> {
        codexbar_providers::cursor_cli::sign_out(home)
    }

    fn email(&self, home: &Path) -> Option<String> {
        codexbar_providers::cursor_cli::email(home)
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(CursorCli)));
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
    Starting,
    Waiting(SharedString),
    Failed(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs a managed Cursor account in. One that is already signed in asks first: the new sign-in replaces it, and may
/// be another Cursor account.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    if !auth_path(hub.dir(), &record).exists() {
        start(record, window, cx);
        return;
    }
    let who = email(hub.dir(), &record).unwrap_or_else(|| "the account signed in now".to_owned());
    window.open_alert_dialog(cx, move |alert, _, _| {
        let record = record.clone();
        alert
            .title(format!("Replace the sign-in of “{}”?", record.label))
            .description(format!(
                "Signing in again replaces {who}. If you sign in to another Cursor account, it becomes a new account \
                 on the dashboard, and the usage history CodexBar kept for {who} is deleted."
            ))
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Sign in again")
                    .ok_variant(ButtonVariant::Primary)
                    .show_cancel(true),
            )
            .on_ok(move |_, window, cx| {
                let record = record.clone();
                // After this dialog has closed.
                window.defer(cx, move |window, cx| start(record, window, cx));
                true
            })
    });
}

fn start(record: AccountRecord, window: &mut Window, cx: &mut App) {
    let dir = SettingsHub::global(cx).dir().to_owned();
    let home = home(&dir, &record);
    if let Err(err) = std::fs::create_dir_all(&home) {
        SettingsHub::set_error(
            cx,
            Some(format!("Couldn't create the account's Cursor folder: {err}").into()),
        );
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
            Stage::Starting => "Starting the Cursor CLI…",
            Stage::Waiting(_) => "Finish signing in to Cursor in your browser. If it didn't open, open the page below.",
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign in “{label}” to Cursor"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("cursor-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .when_some(
                        match &stage {
                            Stage::Waiting(url) => Some(url.clone()),
                            _ => None,
                        },
                        |this, url| {
                            let copy = url.clone();
                            let open = url.clone();
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("cursor-sign-in-copy")
                                            .small()
                                            .outline()
                                            .label("Copy address")
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
                                            }),
                                    )
                                    .child(
                                        Button::new("cursor-sign-in-open")
                                            .small()
                                            .outline()
                                            .label("Open the page")
                                            .on_click(move |_, _, _| {
                                                let _ = SystemBrowser.open(&open);
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
                                    .id("cursor-sign-in-error")
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
                let (service, home, cancel) = (service.clone(), home.clone(), cancel.clone());
                cx.background_spawn(async move { service.begin(&home, &cancel) }).await
            };
            let pending = match begun {
                Ok(pending) => pending,
                Err(CursorCliError::Cancelled) => return,
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    let _ = cx.update(|window, _| window.refresh());
                    return;
                }
            };
            *stage.borrow_mut() = Stage::Waiting(pending.url().into());
            let _ = cx.update(|window, _| window.refresh());
            let waiting = cancel.clone();
            let result = {
                let (service, home) = (service.clone(), home.clone());
                cx.background_spawn(async move {
                    pending.finish(SIGN_IN_TIMEOUT, waiting)?;
                    Ok::<_, CursorCliError>(service.email(&home))
                })
                .await
            };
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let _ = cx.update(|window, cx| match result {
                Ok(email) => match remember(&record_id, &dir, email, cx) {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message);
                        window.refresh();
                    }
                },
                Err(CursorCliError::Cancelled) => {}
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Stores the Cursor identity the folder is signed in to, as the account's dashboard account, and its email for the
/// row. Another identity replaces the one before, whose stored usage is removed with it, so identities never mix.
fn remember(record_id: &str, dir: &Path, email: Option<String>, cx: &mut App) -> Result<(), SharedString> {
    let Some(record) = SettingsHub::global(cx)
        .settings()
        .accounts()
        .iter()
        .find(|a| a.id == record_id)
        .cloned()
    else {
        return Err("The account was removed during the sign-in.".into());
    };
    let _ = std::fs::write(
        home(dir, &record).join(ACCOUNT_FILE),
        json!({ "email": email }).to_string(),
    );
    let identity =
        codexbar_providers::cursor::signed_in_account(&auth_path(dir, &record)).map(|id| id.as_str().to_owned());
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
    let home = home(hub.dir(), &record);
    let service = service(cx);
    window
        .spawn(cx, async move |cx| {
            let result = cx.background_spawn(async move { service.sign_out(&home) }).await;
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
                let folder = home(&dir, &record);
                if codexbar_providers::cursor_cli::auth_path(&folder).exists() {
                    let _ = service.sign_out(&folder);
                }
                delete_home(&dir, &record)
            })
            .await;
        if let Err(err) = result {
            cx.update(|cx| {
                SettingsHub::set_error(
                    cx,
                    Some(format!("The account was removed, but its Cursor folder couldn't be deleted: {err}").into()),
                );
            });
        }
    })
    .detach();
}

/// What the account row says: the email it is signed in as, or that it isn't signed in.
pub fn describe(dir: &Path, record: &AccountRecord) -> String {
    if !auth_path(dir, record).exists() {
        return "Not signed in".to_owned();
    }
    email(dir, record).unwrap_or_else(|| "Signed in".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_folder_is_always_directly_inside_cursor() {
        let dir = std::env::temp_dir().join(format!("codexbar-cursor-homes-{}", std::process::id()));
        let record = |id: &str| AccountRecord {
            id: id.to_owned(),
            ..AccountRecord::new(names::CURSOR, "Work", AuthMethod::OAuth)
        };
        for id in ["abc123", "../../Documents", r"C:\Users\Someone", "Work"] {
            assert_eq!(
                home(&dir, &record(id)).parent(),
                Some(dir.join("cursor").as_path()),
                "{id}"
            );
        }
        let app = AccountRecord::new(names::CURSOR, "App", AuthMethod::Automatic);
        assert!(!is_managed(&app));
        assert_eq!(delete_home(&dir, &app).ok(), Some(()));
    }
}
