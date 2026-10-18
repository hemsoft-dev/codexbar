//! ChatGPT accounts CodexBar signs in itself (#78). Each one gets its own Codex home in the settings folder
//! (`codex\<account id>`). The Codex CLI's app-server signs that home in through the browser, signs it out and renews it,
//! and writes its `auth.json`, so CodexBar needs no client registration of its own (docs/OAUTH.md). An account using
//! the Codex CLI's own sign-in (`~/.codex`) is the import and fallback path and is left to `codex` itself.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codexbar_providers::codex_app_server::{AppServer, AppServerError, SignIn};
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

/// How long a browser sign-in may take before it is given up.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Keeps Codex writing `auth.json` in a CodexBar home rather than the Windows credential store, so the usage fetch can
/// read the sign-in.
const HOME_CONFIG: &str =
    "# Written by CodexBar: this Codex home's sign-in is kept in auth.json.\ncli_auth_credentials_store = \"file\"\n";

/// True for a Codex account CodexBar signs in, rather than one using the Codex CLI's own sign-in.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::CODEX) && record.method == AuthMethod::OAuth
}

/// The Codex home an account reads its sign-in from: its own folder under `dir` when CodexBar signs it in, else the
/// Codex CLI's (`CODEX_HOME`, or `~/.codex`).
pub fn home(dir: &Path, record: &AccountRecord) -> PathBuf {
    if is_managed(record) {
        dir.join("codex").join(folder_name(&record.id))
    } else {
        let auth = codexbar_providers::codex::default_auth_path();
        auth.parent().map_or_else(|| PathBuf::from(".codex"), Path::to_path_buf)
    }
}

/// The folder an account's home is in: its id when that is a plain name (CodexBar's ids are 32 hex digits), else a
/// hash of it, so an edited settings file can't place a home, or its deletion, outside `codex\`.
pub(crate) fn folder_name(id: &str) -> String {
    let plain = !id.is_empty()
        && id.len() <= 64
        // Lowercase only: Windows folders ignore case, so "Work" and "work" must not share one.
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_');
    if plain {
        return id.to_owned();
    }
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(id.as_bytes());
    let hex: String = digest.iter().take(16).map(|byte| format!("{byte:02x}")).collect();
    format!("id-{hex}")
}

/// The sign-in file of an account's Codex home.
pub fn auth_path(dir: &Path, record: &AccountRecord) -> PathBuf {
    home(dir, record).join("auth.json")
}

/// Creates a CodexBar home and its config, keeping a config that is already there.
fn prepare(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let config = home.join("config.toml");
    if !config.exists() {
        std::fs::write(config, HOME_CONFIG)?;
    }
    Ok(())
}

/// Deletes a removed account's CodexBar home. Only a managed home is ever deleted, never the Codex CLI's own.
pub fn delete_home(dir: &Path, record: &AccountRecord) -> std::io::Result<()> {
    let home = home(dir, record);
    // Only ever a folder directly inside `codex\`.
    if !is_managed(record) || home.parent() != Some(dir.join("codex").as_path()) {
        return Ok(());
    }
    match std::fs::remove_dir_all(home) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Signs Codex homes in and out. A seam, so UI tests never start Codex or a browser.
pub trait SignInService: Send + Sync {
    /// Starts a browser sign-in for `home` and opens the sign-in page.
    /// Unless `cancel` was set while Codex was starting, in which case no browser opens.
    fn begin(&self, home: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, AppServerError>;
    fn sign_out(&self, home: &Path) -> Result<(), AppServerError>;
}

/// A browser sign-in that has started.
pub trait PendingSignIn: Send {
    /// The sign-in page, shown in case the browser didn't open.
    fn url(&self) -> String;
    /// Waits for the sign-in to complete. `cancel` is set when the user closes the dialog.
    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), AppServerError>;
}

impl PendingSignIn for SignIn {
    fn url(&self) -> String {
        SignIn::url(self).to_owned()
    }

    fn finish(self: Box<Self>, timeout: Duration, cancel: Arc<AtomicBool>) -> Result<(), AppServerError> {
        SignIn::finish(*self, timeout, &cancel)
    }
}

/// The Codex CLI's app-server, and the user's browser.
struct CodexCli;

impl SignInService for CodexCli {
    fn begin(&self, home: &Path, cancel: &AtomicBool) -> Result<Box<dyn PendingSignIn>, AppServerError> {
        let sign_in = AppServer::start(home)?.begin_sign_in()?;
        // Closed while Codex was starting: no browser opens, and Codex drops the sign-in (one that somehow completed
        // stands, and the dialog is gone either way).
        if cancel.load(Ordering::SeqCst) {
            sign_in.finish(Duration::ZERO, cancel)?;
            return Err(AppServerError::Cancelled);
        }
        // A browser that doesn't open isn't fatal: the dialog shows the page to open by hand.
        let _ = SystemBrowser.open(sign_in.url());
        Ok(Box::new(sign_in))
    }

    fn sign_out(&self, home: &Path) -> Result<(), AppServerError> {
        AppServer::start(home)?.sign_out()
    }
}

/// The sign-in service the app uses.
pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(CodexCli)));
}

/// Tests install a fake.
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

/// Cancels the sign-in when the dialog goes away, however it is closed.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs a CodexBar-managed account in through the browser, in a dialog that shows progress and can be cancelled.
/// On success the account remembers which ChatGPT identity it holds, and the dashboard refreshes.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let dir = hub.dir().to_owned();
    let home = home(&dir, &record);
    if let Err(err) = prepare(&home) {
        SettingsHub::set_error(
            cx,
            Some(format!("Couldn't create the account's Codex folder: {err}").into()),
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
        let (text, url, error) = match &stage {
            Stage::Starting => ("Starting the Codex CLI…".to_owned(), None, None),
            Stage::Waiting(url) => (
                "Finish signing in to ChatGPT in your browser. If it didn't open, copy the page address below."
                    .to_owned(),
                Some(url.clone()),
                None,
            ),
            Stage::Failed(error) => ("The sign-in didn't complete.".to_owned(), None, Some(error.clone())),
        };
        dialog
            .title(format!("Sign in “{label}”"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text.clone())
                            .text_sm()
                            .child(text),
                    )
                    .when_some(url, |this, url| {
                        let copy = url.clone();
                        this.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(url),
                                )
                                .child(
                                    Button::new("sign-in-copy")
                                        .small()
                                        .outline()
                                        .label("Copy address")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()));
                                        }),
                                ),
                        )
                    })
                    .when_some(error, |this, error| {
                        this.child(
                            div()
                                .id("sign-in-error")
                                .role(Role::Alert)
                                .test_support()
                                .aria_label(error.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    }),
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
            let begin_home = home.clone();
            let begun = {
                let service = service.clone();
                let cancel = cancel.clone();
                cx.background_spawn(async move { service.begin(&begin_home, &cancel) })
                    .await
            };
            let pending = match begun {
                Ok(pending) => pending,
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    let _ = cx.update(|window, _| window.refresh());
                    return;
                }
            };
            *stage.borrow_mut() = Stage::Waiting(pending.url().into());
            let _ = cx.update(|window, _| window.refresh());
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
                // Closed by the user: nothing to show.
                Err(AppServerError::Cancelled) => {}
                Err(err) => {
                    *stage.borrow_mut() = Stage::Failed(err.to_string().into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Stores the ChatGPT identity the account's home is signed in to, as its dashboard account. Signing it in to another
/// identity replaces the one before, whose stored usage is then removed with it, so identities never mix.
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
    let identity =
        codexbar_providers::codex::signed_in_account(&auth_path(dir, &record)).map(|id| id.as_str().to_owned());
    if identity.is_some() && identity != record.external_id {
        SettingsHub::update(cx, |settings| {
            settings.upsert(AccountRecord {
                external_id: identity,
                ..record
            })
        })
        .map_err(|err| SharedString::from(format!("The sign-in couldn't be saved: {err}")))?;
    }
    // The dashboard fetches the new sign-in now rather than at the next scheduled refresh.
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Signs a CodexBar-managed account out. Its usage history stays with it, for when it is signed in again.
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

/// Signs a removed managed account out and deletes its home, off the UI thread. Failures are reported, not retried.
pub fn forget(record: AccountRecord, cx: &mut App) {
    if !is_managed(&record) {
        return;
    }
    let dir = SettingsHub::global(cx).dir().to_owned();
    let service = service(cx);
    cx.spawn(async move |cx| {
        let result = cx
            .background_spawn(async move {
                let home = home(&dir, &record);
                // Signing out first revokes nothing CodexBar can't recreate; a home that was never signed in has
                // nothing to sign out.
                if home.join("auth.json").exists() {
                    let _ = service.sign_out(&home);
                }
                delete_home(&dir, &record)
            })
            .await;
        if let Err(err) = result {
            cx.update(|cx| {
                SettingsHub::set_error(
                    cx,
                    Some(format!("The account was removed, but its Codex folder couldn't be deleted: {err}").into()),
                );
            });
        }
    })
    .detach();
}

/// What the account row says about the sign-in: its email, or that it isn't signed in.
pub fn describe(dir: &Path, record: &AccountRecord) -> String {
    let path = auth_path(dir, record);
    match (codexbar_providers::codex::signed_in_email(&path), path.exists()) {
        (Some(email), _) => email,
        (None, true) => "Signed in".to_owned(),
        (None, false) => "Not signed in".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(id: &str) -> AccountRecord {
        AccountRecord {
            id: id.to_owned(),
            ..AccountRecord::new(names::CODEX, "Work", AuthMethod::OAuth)
        }
    }

    #[test]
    fn a_home_is_always_a_folder_directly_inside_codex() {
        let dir = std::env::temp_dir().join(format!("codexbar-homes-{}", std::process::id()));
        let codex = dir.join("codex");
        assert_eq!(home(&dir, &managed("0123abcd")), codex.join("0123abcd"));
        // An edited settings file can't point a home elsewhere: relative, absolute or odd ids are hashed.
        for id in [
            "../../Documents",
            r"C:\Users\Someone",
            "..",
            "a/b",
            "",
            &"x".repeat(65),
            "Work",
        ] {
            let home = home(&dir, &managed(id));
            assert_eq!(home.parent(), Some(codex.as_path()), "{id}");
            let name = home.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("id-") && name.len() == 35, "{id} -> {name}");
        }
        assert_ne!(home(&dir, &managed("../a")), home(&dir, &managed("../b")));
        // Ids differing only in case get different folders.
        assert_ne!(home(&dir, &managed("Work")), home(&dir, &managed("work")));
    }

    #[test]
    fn deleting_a_home_never_touches_anything_else() {
        let dir = std::env::temp_dir().join(format!("codexbar-delete-{}", std::process::id()));
        let outside = dir.join("Documents");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        delete_home(&dir.join("settings"), &managed("../Documents")).unwrap();
        assert!(outside.join("keep.txt").exists());
        // An account on the Codex CLI's own sign-in never deletes that home.
        let automatic = AccountRecord::new(names::CODEX, "CLI", AuthMethod::Automatic);
        assert_eq!(delete_home(&dir, &automatic).ok(), Some(()));
        let managed_home = home(&dir, &managed("cx-1"));
        std::fs::create_dir_all(&managed_home).unwrap();
        delete_home(&dir, &managed("cx-1")).unwrap();
        assert!(!managed_home.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
