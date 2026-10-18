//! Grok accounts CodexBar signs in itself (#108), experimental. Each has its own Grok CLI home in the settings folder
//! (`grok\<account id>`, used as `GROK_HOME`). The official CLI signs it in (`grok login`, in a console window of its
//! own, because it is a terminal flow) and renews it; CodexBar reads its `auth.json`, borrows no OAuth client and
//! stores no token of its own. A new sign-in happens in a temporary home and replaces the account's only once xAI's
//! userinfo confirms who signed in, so a failed or cancelled sign-in leaves the account as it was. An account on the
//! Grok CLI's own sign-in (`~/.grok`, Automatic) is the fallback.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use codexbar_providers::UreqClient;
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::settings_hub::SettingsHub;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PENDING: &str = ".signing-in";
/// The suffix of an account's earlier home while a new sign-in replaces it.
const PREVIOUS: &str = ".previous";

/// True for a Grok account CodexBar signs in, rather than one on the Grok CLI's own sign-in.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::GROK) && record.method == AuthMethod::OAuth
}

fn folders(dir: &Path) -> PathBuf {
    dir.join("grok")
}

/// The Grok CLI home an account reads: its own when CodexBar signs it in, else the Grok CLI's (`GROK_HOME` or
/// `~/.grok`).
pub fn home(dir: &Path, record: &AccountRecord) -> PathBuf {
    if is_managed(record) {
        folders(dir).join(crate::codex_sign_in::folder_name(&record.id))
    } else {
        std::env::var_os("GROK_HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(|| codexbar_core::home_dir().map(|home| home.join(".grok")))
            .unwrap_or_else(|| PathBuf::from(".grok"))
    }
}

fn pending(dir: &Path, record: &AccountRecord) -> PathBuf {
    folders(dir).join(format!("{}{PENDING}", crate::codex_sign_in::folder_name(&record.id)))
}

/// Deletes temporary homes an interrupted sign-in left behind.
pub fn clean_up(cx: &mut App) {
    let parent = folders(SettingsHub::global(cx).dir());
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // A sign-in interrupted while replacing a home: the earlier home goes back if the new one never arrived.
        if let Some(account) = name.strip_suffix(PREVIOUS) {
            let target = parent.join(account);
            if !target.exists() && std::fs::rename(entry.path(), &target).is_ok() {
                continue;
            }
            let _ = std::fs::remove_dir_all(entry.path());
            continue;
        }
        if name.ends_with(PENDING)
            && let Err(err) = std::fs::remove_dir_all(entry.path())
        {
            SettingsHub::set_error(
                cx,
                Some(format!("A folder from an unfinished Grok sign-in couldn't be deleted: {err}").into()),
            );
        }
    }
}

/// Signs a Grok CLI home in. A seam, so UI tests never start the Grok CLI or reach xAI.
pub trait SignInService: Send + Sync {
    /// Runs the CLI's sign-in for `home` until it finishes, `cancel` is set, or it times out.
    fn sign_in(&self, home: &Path, cancel: &AtomicBool) -> Result<(), String>;
    /// The xAI subject the token belongs to, from userinfo.
    fn subject(&self, token: &str) -> Result<String, String>;
}

struct GrokCli;

impl SignInService for GrokCli {
    fn sign_in(&self, home: &Path, cancel: &AtomicBool) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|err| format!("The sign-in folder couldn't be made: {err}"))?;
        let mut window = codexbar_providers::grok::LoginWindow::open(home)
            .map_err(|_| "The Grok CLI (grok) is needed for this sign-in.".to_owned())?;
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            if cancel.load(Ordering::SeqCst) || Instant::now() > deadline {
                window.kill();
                return Err("cancelled".into());
            }
            if window.finished() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn subject(&self, token: &str) -> Result<String, String> {
        codexbar_providers::grok::userinfo_subject(&UreqClient::without_redirects(), token)
            .map_err(|err| err.to_string())
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(GrokCli)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
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

/// Signs a Grok account in, in a dialog that ends the Grok CLI's window when cancelled.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let temporary = pending(hub.dir(), &record);
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
                "Sign in to Grok in the Grok CLI window that opened (it opens xAI's sign-in page in your browser), \
                 then close that window."
            }
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign “{label}” in to Grok"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("grok-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "Experimental: Grok's weekly usage comes from the Grok CLI's own billing service, not a \
                         published API.",
                    ))
                    .when_some(
                        match &stage {
                            Stage::Failed(error) => Some(error.clone()),
                            _ => None,
                        },
                        |this, error| {
                            this.child(
                                div()
                                    .id("grok-sign-in-error")
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
            let running = cancel.clone();
            let folder = temporary.clone();
            let result = cx
                .background_spawn(async move {
                    // A fresh temporary home for each attempt.
                    let _ = std::fs::remove_dir_all(&folder);
                    service.sign_in(&folder, &running)?;
                    let auth = codexbar_providers::grok::read_auth(&folder)
                        .ok_or("The Grok CLI didn't complete the sign-in.")?;
                    let subject = service.subject(&auth.token)?;
                    if subject != auth.subject {
                        return Err(
                            "xAI's account check didn't match the Grok CLI's sign-in. Nothing was kept.".into(),
                        );
                    }
                    Ok::<_, String>(subject)
                })
                .await;
            if cancel.load(Ordering::SeqCst) {
                let _ = std::fs::remove_dir_all(&temporary);
                return;
            }
            let _ = cx.update(|window, cx| {
                let outcome = result.and_then(|subject| remember(&record_id, &temporary, &subject, cx));
                let _ = std::fs::remove_dir_all(&temporary);
                match outcome {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message.into());
                        window.refresh();
                    }
                }
            });
        })
        .detach();
}

/// Moves the verified temporary home into the account's place and binds the account to its xAI subject. An account
/// bound to another subject refuses it, so signing in again can't silently switch who it is.
fn remember(record_id: &str, temporary: &Path, subject: &str, cx: &mut App) -> Result<(), String> {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return Err("The account was removed during the sign-in. Nothing was kept.".into());
    };
    if !is_managed(&record) {
        return Err("The account no longer signs in to Grok here. Nothing was kept.".into());
    }
    if record.external_id.as_deref().is_some_and(|known| known != subject) {
        return Err("This account was signed in to a different xAI account. Add another Grok account for it.".into());
    }
    if let Some(other) = hub
        .settings()
        .accounts()
        .iter()
        .find(|other| other.id != record.id && is_managed(other) && other.external_id.as_deref() == Some(subject))
    {
        return Err(format!("That xAI account is already added as “{}”.", other.label));
    }
    // The account's current home is set aside, not deleted, until the new one is in place and the account is saved; a
    // failure at either step puts it back, so a failed sign-in changes nothing.
    let target = home(hub.dir(), &record);
    let previous = backup_of(&target);
    let _ = std::fs::remove_dir_all(&previous);
    let had_home = target.exists();
    if had_home {
        std::fs::rename(&target, &previous).map_err(|err| format!("The sign-in couldn't be kept: {err}"))?;
    }
    let restore = |target: &Path| {
        let _ = std::fs::remove_dir_all(target);
        if had_home {
            let _ = std::fs::rename(&previous, target);
        }
    };
    if let Err(err) = std::fs::rename(temporary, &target) {
        restore(&target);
        return Err(format!("The sign-in couldn't be kept: {err}"));
    }
    let result = SettingsHub::update(cx, |settings| {
        settings.upsert(AccountRecord {
            external_id: Some(subject.to_owned()),
            ..record
        })
    });
    if let Err(err) = result {
        // Not bound to a subject, the sign-in isn't kept either; the account keeps its earlier one.
        restore(&target);
        return Err(format!("The sign-in couldn't be saved: {err}"));
    }
    let _ = std::fs::remove_dir_all(&previous);
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Where an account's home waits while a new sign-in replaces it.
fn backup_of(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_owned();
    name.push(PREVIOUS);
    target.with_file_name(name)
}

/// Signs a Grok account out here: its CLI home is deleted. xAI isn't signed out.
pub fn sign_out(record_id: &str, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    forget(record, cx);
    SettingsHub::request_refresh(cx);
}

/// Deletes the CLI home CodexBar made for an account; only ever one directly inside `grok\`.
pub fn forget(record: AccountRecord, cx: &mut App) {
    let dir = SettingsHub::global(cx).dir().to_owned();
    let folder = home(&dir, &record);
    if !is_managed(&record) || folder.parent() != Some(folders(&dir).as_path()) {
        return;
    }
    if let Err(err) = std::fs::remove_dir_all(&folder)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        SettingsHub::set_error(
            cx,
            Some(format!("The account's Grok folder couldn't be deleted: {err}").into()),
        );
    }
}

/// Who the account row says the account is signed in as.
pub fn describe(dir: &Path, record: &AccountRecord) -> String {
    match codexbar_providers::grok::read_auth(&home(dir, record)) {
        Some(auth) => auth.email.unwrap_or_else(|| "Signed in".to_owned()),
        None => "Not signed in".to_owned(),
    }
}
