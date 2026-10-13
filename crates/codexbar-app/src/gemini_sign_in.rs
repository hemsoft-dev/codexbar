//! Gemini accounts' Gemini Apps sign-in (#106), experimental. CodexBar opens Gemini in Microsoft Edge or Google Chrome
//! on a fresh, temporary profile; the user signs in to Google there. CodexBar reads only the browser's cookies, keeps the
//! two Google session cookies Gemini Apps usage needs once both limits read with them, saves them in Windows Credential
//! Manager under the account, and closes the browser and deletes its profile. No password, page or script is read, and
//! no other browser's cookies are touched. Signing out deletes CodexBar's copy; it doesn't sign out of Google.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use codexbar_providers::UreqClient;
use codexbar_providers::gemini_apps::{self, AppsSession};
use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::settings_hub::SettingsHub;

/// How long the browser waits for a sign-in.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const USAGE_PAGE: &str = "https://gemini.google.com/usage";

/// True for a Gemini account whose Gemini Apps connection CodexBar signs in.
pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::GEMINI) && record.method == AuthMethod::BrowserSession
}

/// Where the Gemini Apps session is kept: beside the account's own entry, which a coding connection (#107) can use.
fn apps_key(record: &AccountRecord) -> String {
    format!("{}:gemini-apps", record.id)
}

/// The account's Gemini Apps session: `Ok(None)` when it isn't signed in, an error when Credential Manager couldn't be
/// read.
pub fn apps_session(
    hub: &SettingsHub,
    record: &AccountRecord,
) -> Result<Option<AppsSession>, codexbar_store::credentials::CredentialError> {
    Ok(
        codexbar_store::credentials::read_long(hub.credentials().as_ref(), &apps_key(record))?
            .and_then(|secret| AppsSession::from_secret(&secret)),
    )
}

/// The folder temporary browser profiles go in.
fn profiles(dir: &Path) -> PathBuf {
    dir.join("gemini")
}

/// Deletes temporary browser profiles an interrupted sign-in left behind.
pub fn clean_up(cx: &mut App) {
    if let Err(err) = codexbar_providers::browser_session::clean_up(&profiles(SettingsHub::global(cx).dir())) {
        SettingsHub::set_error(
            cx,
            Some(format!("A folder from an unfinished Gemini sign-in couldn't be deleted: {err}").into()),
        );
    }
}

/// Signs Gemini Apps in. A seam, so UI tests never start a browser or reach Google.
pub trait SignInService: Send + Sync {
    /// Opens the sign-in browser on a temporary profile in `parent` and waits until its cookies make a session whose
    /// usage reads both Gemini Apps limits, or `cancel` is set, or it times out. The browser and profile are gone when
    /// this returns. `waiting` is called once the browser is up.
    fn sign_in(&self, parent: &Path, cancel: &AtomicBool, waiting: &dyn Fn()) -> Result<AppsSession, String>;
}

struct Browser;

impl SignInService for Browser {
    fn sign_in(&self, parent: &Path, cancel: &AtomicBool, waiting: &dyn Fn()) -> Result<AppsSession, String> {
        use codexbar_providers::browser_session::{DisposableBrowser, find_browser};
        let browser = find_browser().ok_or("Microsoft Edge or Google Chrome is needed for this sign-in.")?;
        std::fs::create_dir_all(parent).map_err(|err| format!("The sign-in folder couldn't be made: {err}"))?;
        let mut session = DisposableBrowser::open(&browser, parent, USAGE_PAGE).map_err(|err| err.to_string())?;
        waiting();
        let http = UreqClient::without_redirects();
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        // A session that didn't read usage isn't tried again until its cookies change.
        let mut tried: Option<AppsSession> = None;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            if Instant::now() > deadline {
                return Err("The sign-in took too long. Try again.".into());
            }
            if !session.running() {
                return Err("The sign-in window was closed before Gemini could be read.".into());
            }
            let cookies = gemini_apps::browser_cookies(&session.cookies().map_err(|err| err.to_string())?);
            match gemini_apps::session_from(&cookies, chrono::Utc::now()) {
                Err(gemini_apps::CookieProblem::Conflicting) => {
                    return Err("The browser holds two different Google sessions. Try again.".into());
                }
                Ok(candidate) if tried.as_ref() != Some(&candidate) => {
                    // Kept in memory only, until both limits read with it.
                    match gemini_apps::fetch(&http, &candidate, chrono::Utc::now()) {
                        Ok(usage) if usage.five_hour.is_some() && usage.weekly.is_some() => return Ok(candidate),
                        _ => tried = Some(candidate),
                    }
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

pub struct Service(pub Arc<dyn SignInService>);

impl Global for Service {}

pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(Browser)));
}

#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
}

#[derive(Clone)]
enum Stage {
    Starting,
    Waiting,
    Failed(SharedString),
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Signs a Gemini account's Gemini Apps connection in, in a dialog that can be cancelled. Closing the dialog closes the
/// browser and keeps nothing.
pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    let parent = profiles(hub.dir());
    let stage = Rc::new(RefCell::new(Stage::Starting));
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = Rc::new(CancelOnDrop(cancel.clone()));
    let label = record.label.clone();
    let shown = stage.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let _keep = &guard;
        let stage = shown.borrow().clone();
        let text = match &stage {
            Stage::Starting => "Opening a temporary browser window…",
            Stage::Waiting => {
                "Sign in to Google in the browser window that opened, with the account whose Gemini usage to show. \
                 CodexBar closes the window and deletes its data once Gemini's usage reads."
            }
            Stage::Failed(_) => "The sign-in didn't complete.",
        };
        dialog
            .title(format!("Sign “{label}” in to Gemini Apps"))
            .w(crate::zoom::scaled(480., cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("gemini-sign-in-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(text)
                            .text_sm()
                            .child(text),
                    )
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "Experimental: Gemini Apps usage comes from Gemini's website, not a published API. CodexBar \
                         keeps only two Google session cookies, which can reach more of your Google account; they go \
                         only to gemini.google.com.",
                    ))
                    .when_some(
                        match &stage {
                            Stage::Failed(error) => Some(error.clone()),
                            _ => None,
                        },
                        |this, error| {
                            this.child(
                                div()
                                    .id("gemini-sign-in-error")
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
            let task = cx.background_spawn(async move { service.sign_in(&parent, &running, &|| {}) });
            // Until the result, the dialog says to sign in in the browser.
            *stage.borrow_mut() = Stage::Waiting;
            let _ = cx.update(|window, _| window.refresh());
            let result = task.await;
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let _ = cx.update(|window, cx| match result {
                Ok(session) => match remember(&record_id, &session, cx) {
                    Ok(()) => window.close_dialog(cx),
                    Err(message) => {
                        *stage.borrow_mut() = Stage::Failed(message);
                        window.refresh();
                    }
                },
                Err(message) => {
                    *stage.borrow_mut() = Stage::Failed(message.into());
                    window.refresh();
                }
            });
        })
        .detach();
}

/// Keeps the session for the account, replacing any earlier one in one write.
fn remember(record_id: &str, session: &AppsSession, cx: &mut App) -> Result<(), SharedString> {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return Err("The account was removed during the sign-in. Nothing was saved.".into());
    };
    if !is_managed(&record) {
        return Err("The account no longer signs in to Gemini Apps here. Nothing was saved.".into());
    }
    codexbar_store::credentials::write_long(hub.credentials().as_ref(), &apps_key(&record), &session.to_secret())
        .map_err(|err| SharedString::from(format!("The sign-in couldn't be saved: {err}")))?;
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Signs an account's Gemini Apps connection out: CodexBar's copy of the session is deleted. Google isn't signed out.
pub fn sign_out(record_id: &str, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if let Err(err) = codexbar_store::credentials::delete_long(hub.credentials().as_ref(), &apps_key(&record)) {
        SettingsHub::set_error(cx, Some(format!("Couldn't sign out: {err}").into()));
        return;
    }
    SettingsHub::request_refresh(cx);
}

/// Deletes what CodexBar kept for an account that was removed or left this sign-in.
pub fn forget(record: AccountRecord, cx: &mut App) {
    if let Err(err) =
        codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), &apps_key(&record))
    {
        SettingsHub::set_error(
            cx,
            Some(format!("The account's Gemini sign-in couldn't be deleted: {err}").into()),
        );
    }
}

/// Whether the account row's Gemini Apps connection is signed in.
pub fn describe(hub: &SettingsHub, record: &AccountRecord) -> String {
    match apps_session(hub, record) {
        Err(_) => "Its sign-in couldn't be read from Windows Credential Manager".to_owned(),
        Ok(Some(_)) => "Gemini Apps signed in".to_owned(),
        Ok(None) => "Gemini Apps not signed in".to_owned(),
    }
}
