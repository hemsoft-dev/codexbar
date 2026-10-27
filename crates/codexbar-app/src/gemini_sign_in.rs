//! Connect Antigravity through its own CLI and native-browser authentication. Previously saved Gemini Apps
//! sessions remain readable, but Google sign-in never runs through a controlled browser or cookie extraction.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codexbar_providers::gemini_apps::AppsSession;
use codexbar_store::settings::{AccountRecord, names};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Global, InteractiveElement as _, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const APPS_LIMITATION: &str = "Gemini Apps website usage is separate. Saved sessions still work, but new Google website sign-in is unavailable here: the controlled-browser login was reported blocked, and Antigravity OAuth cannot supply website cookies.";
const SAME_ACCOUNT: &str = "Connect the same Google account whose usage belongs here. Antigravity has one shared local sign-in; CodexBar cannot verify its Google identity. Use a different CodexBar account for different history.";

pub fn is_managed(record: &AccountRecord) -> bool {
    record.provider.eq_ignore_ascii_case(names::GEMINI)
}
fn apps_key(record: &AccountRecord) -> String {
    format!("{}:gemini-apps", record.id)
}
pub fn apps_session(
    hub: &SettingsHub,
    record: &AccountRecord,
) -> Result<Option<AppsSession>, codexbar_store::credentials::CredentialError> {
    Ok(
        codexbar_store::credentials::read_long(hub.credentials().as_ref(), &apps_key(record))?
            .and_then(|secret| AppsSession::from_secret(&secret)),
    )
}
use crate::settings_hub::SettingsHub;

/// Clean only temporary profiles left by the retired Google-cookie sign-in. Never touch a user's browser profile.
pub fn clean_up(cx: &mut App) {
    if let Err(err) = codexbar_providers::browser_session::clean_up(&SettingsHub::global(cx).dir().join("gemini")) {
        SettingsHub::set_error(
            cx,
            Some(format!("An old Gemini sign-in folder couldn't be deleted: {err}").into()),
        );
    }
}

/// The owned CLI handle, never the user's browser or shared vendor credentials.
pub trait PendingSignIn: Send {
    fn finished(&mut self) -> bool;
}
impl PendingSignIn for codexbar_providers::gemini_login::LoginWindow {
    fn finished(&mut self) -> bool {
        self.finished()
    }
}
pub trait SignInService: Send + Sync {
    fn check(&self) -> Result<(), String>;
    fn open(&self, dir: &Path) -> Result<Box<dyn PendingSignIn>, String>;
}
struct Antigravity;
impl SignInService for Antigravity {
    fn check(&self) -> Result<(), String> {
        codexbar_providers::gemini_coding::fetch(&codexbar_providers::gemini_coding::CliRunner, chrono::Utc::now())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    fn open(&self, dir: &Path) -> Result<Box<dyn PendingSignIn>, String> {
        codexbar_providers::gemini_login::LoginWindow::open(dir)
            .map(|window| Box::new(window) as Box<dyn PendingSignIn>)
    }
}
pub struct Service(pub Arc<dyn SignInService>);
impl Global for Service {}
pub(crate) struct Running(pub Arc<AtomicBool>);
impl Global for Running {}
pub fn init(cx: &mut App) {
    cx.set_global(Service(Arc::new(Antigravity)));
}
#[cfg(test)]
pub fn init_with(cx: &mut App, service: Arc<dyn SignInService>) {
    cx.set_global(Service(service));
}

#[derive(Clone)]
enum Stage {
    Checking,
    Waiting,
    Failed(SharedString),
}
type Pending = Rc<RefCell<Option<Box<dyn PendingSignIn>>>>;
struct CancelOnDrop {
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    pending: Pending,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        self.pending.borrow_mut().take();
    }
}

pub fn sign_in(record_id: &str, window: &mut Window, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return;
    };
    if !is_managed(&record) || hub.is_read_only() {
        return;
    }
    if cx
        .try_global::<Running>()
        .is_some_and(|running| running.0.load(Ordering::SeqCst))
    {
        SettingsHub::set_error(
            cx,
            Some("An Antigravity connection is already open. Finish or cancel it first.".into()),
        );
        return;
    }
    let stage = Rc::new(RefCell::new(Stage::Checking));
    let cancel = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let pending: Pending = Rc::default();
    let guard = Rc::new(CancelOnDrop {
        cancel: cancel.clone(),
        active: active.clone(),
        pending: pending.clone(),
    });
    let dir = hub.dir().to_owned();
    cx.set_global(Running(active.clone()));
    let shown = stage.clone();
    let cancel_button = cancel.clone();
    let cli = pending.clone();
    let id = record.id.clone();
    let service = cx.global::<Service>().0.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let _keep = &guard;
        let phase = shown.borrow().clone();
        let checking = matches!(phase, Stage::Checking);
        let stopped = cancel_button.load(Ordering::SeqCst);
        let text = match phase {
            Stage::Checking => "Checking Antigravity's existing local sign-in through its usage command…",
            Stage::Waiting => "The Antigravity CLI is open. Complete its sign-in in your default system browser, then choose Check connection. No model prompt is submitted.",
            Stage::Failed(_) => "The Antigravity connection has not been verified. Follow the message below, or open the CLI to sign in.",
        };
        let open_stage = shown.clone(); let open_cli = cli.clone(); let open_service = service.clone(); let open_dir = dir.clone();
        let check_stage = shown.clone(); let check_cancel = cancel_button.clone(); let check_id = id.clone(); let check_pending = cli.clone();
        dialog.title(format!("Connect “{}” to Antigravity", record.label)).w(crate::zoom::scaled(560., cx))
            .child(v_flex().gap_3()
                .child(div().id("gemini-sign-in-status").role(Role::Status).test_support().aria_label(text).text_sm().child(text))
                .child(div().id("gemini-sign-in-same-account").test_support().aria_label(SAME_ACCOUNT).text_xs().text_color(cx.theme().muted_foreground).child(SAME_ACCOUNT))
                .child(div().id("gemini-apps-limitation").test_support().aria_label(APPS_LIMITATION).text_xs().text_color(cx.theme().muted_foreground).child(APPS_LIMITATION))
                .when_some(match &phase { Stage::Failed(error) => Some(error.clone()), _ => None }, |this, error| {
                    this.child(div().id("gemini-sign-in-error").role(Role::Alert).test_support().aria_label(error.clone()).text_sm().text_color(cx.theme().danger).child(error))
                }))
            .footer(DialogFooter::new()
                .child(Button::new("antigravity-open-cli").outline().label("Open Antigravity sign-in").disabled(checking || stopped || cli.borrow().is_some())
                    .on_click(move |_, window, _| {
                        if open_cli.borrow().is_some() { return; }
                        match open_service.open(&open_dir) {
                            Ok(process) => { *open_cli.borrow_mut() = Some(process); *open_stage.borrow_mut() = Stage::Waiting; }
                            Err(error) => *open_stage.borrow_mut() = Stage::Failed(error.into()),
                        }
                        window.refresh();
                    }))
                .child(Button::new("antigravity-check").primary().label("Check connection").disabled(checking || stopped)
                    .on_click(move |_, window, cx| {
                        if !matches!(*check_stage.borrow(), Stage::Checking) {
                            check(check_id.clone(), check_stage.clone(), check_cancel.clone(), check_pending.clone(), window, cx);
                        }
                    }))
                .child(DialogClose::new().trigger(|button| button.outline().label(if stopped { "Close" } else { "Cancel" }))))
    });
    check(record.id, stage.clone(), cancel.clone(), pending.clone(), window, cx);
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(SIGN_IN_TIMEOUT).await;
            if !active.load(Ordering::SeqCst) {
                return;
            }
            cancel.store(true, Ordering::SeqCst);
            pending.borrow_mut().take();
            *stage.borrow_mut() = Stage::Failed(
                "The connection timed out. Open it again to retry. Any sign-in saved by Antigravity is kept.".into(),
            );
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
}

fn check(
    record_id: String,
    stage: Rc<RefCell<Stage>>,
    cancel: Arc<AtomicBool>,
    pending: Pending,
    window: &mut Window,
    cx: &mut App,
) {
    if cancel.load(Ordering::SeqCst) {
        return;
    }
    *stage.borrow_mut() = Stage::Checking;
    window.refresh();
    let service = cx.global::<Service>().0.clone();
    window
        .spawn(cx, async move |cx| {
            let result = cx.background_spawn(async move { service.check() }).await;
            if cancel.load(Ordering::SeqCst) {
                return;
            }
            let _ = cx.update(|window, cx| {
                let result = result.and_then(|()| connect(&record_id, cx).map_err(|error| error.to_string()));
                match result {
                    Ok(()) => window.close_dialog(cx),
                    Err(error) => {
                        // A closed CLI must not keep the retry button disabled. Keep an active consent process alone.
                        let finished = pending.borrow_mut().as_mut().is_some_and(|cli| cli.finished());
                        if finished {
                            pending.borrow_mut().take();
                        }
                        *stage.borrow_mut() = Stage::Failed(error.into());
                        window.refresh();
                    }
                }
            });
        })
        .detach();
}

fn connect(record_id: &str, cx: &mut App) -> Result<(), SharedString> {
    let hub = SettingsHub::global(cx);
    let Some(mut record) = hub.settings().accounts().iter().find(|a| a.id == record_id).cloned() else {
        return Err("The account was removed. Nothing was connected.".into());
    };
    if !is_managed(&record) {
        return Err("The account is no longer a Gemini account. Nothing was connected.".into());
    }
    if let Some(other) = hub
        .settings()
        .accounts()
        .iter()
        .find(|a| a.id != record_id && a.gemini_coding)
    {
        return Err(format!(
            "“{}” already shows Antigravity's shared sign-in. Disconnect its coding quotas first.",
            other.label
        )
        .into());
    }
    record.gemini_coding = true;
    SettingsHub::update(cx, |settings| settings.upsert(record))
        .map_err(|error| SharedString::from(error.to_string()))?;
    SettingsHub::request_refresh(cx);
    Ok(())
}

/// Disconnect only CodexBar's coding feed. The shared Antigravity sign-in and saved Apps session are preserved.
pub fn sign_out(record_id: &str, cx: &mut App) {
    let Some(mut record) = SettingsHub::global(cx)
        .settings()
        .accounts()
        .iter()
        .find(|a| a.id == record_id)
        .cloned()
    else {
        return;
    };
    record.gemini_coding = false;
    let _ = SettingsHub::update(cx, |settings| settings.upsert(record));
    SettingsHub::request_refresh(cx);
}
pub fn forget_apps(record_id: &str, cx: &mut App) {
    let hub = SettingsHub::global(cx);
    let Some(record) = hub.settings().accounts().iter().find(|a| a.id == record_id) else {
        return;
    };
    if let Err(err) = codexbar_store::credentials::delete_long(hub.credentials().as_ref(), &apps_key(record)) {
        SettingsHub::set_error(
            cx,
            Some(format!("Couldn't forget the Gemini Apps session: {err}").into()),
        );
    }
    SettingsHub::request_refresh(cx);
}
pub fn forget(record: AccountRecord, cx: &mut App) {
    if let Err(err) =
        codexbar_store::credentials::delete_long(SettingsHub::global(cx).credentials().as_ref(), &apps_key(&record))
    {
        SettingsHub::set_error(
            cx,
            Some(format!("The account's saved Gemini Apps session couldn't be deleted: {err}").into()),
        );
    }
}
pub fn describe(hub: &SettingsHub, record: &AccountRecord) -> String {
    let coding = if record.gemini_coding {
        "Antigravity CLI coding quotas enabled"
    } else {
        "Antigravity not connected"
    };
    match apps_session(hub, record) {
        Ok(Some(_)) => format!("{coding} · saved Gemini Apps session"),
        Err(_) => format!("{coding} · saved Gemini Apps session unreadable"),
        Ok(None) => coding.to_owned(),
    }
}
