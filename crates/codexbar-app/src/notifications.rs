//! Delivers alerts (#87) as system notifications (Windows toasts, or the macOS and Linux notification centers through
//! GPUI) and keeps the active-alert set: an alert is marked active only once
//! it was shown, so a blocked or failed notification is tried again on the next refresh.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use codexbar_core::AccountSnapshot;
use codexbar_core::alerts::{Alert, evaluate};
use gpui_kit::{App, BorrowAppContext as _, Global, SharedString};

use crate::prefs_hub::PrefsHub;

/// Whether notifications can be shown right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifierStatus {
    Ready,
    /// Windows won't show them; the reason says where to turn them on.
    Blocked(String),
}

pub trait Notifier: Send + Sync {
    fn status(&self) -> NotifierStatus;
    fn show(&self, alert: &Alert, cx: &App) -> Result<(), String>;
    /// Alert keys whose notification Windows accepted but then failed to raise, since the last call.
    fn take_failed(&self) -> Vec<String> {
        Vec::new()
    }

    /// True when `take_failed` has keys waiting.
    fn has_failed(&self) -> bool {
        false
    }

    /// Why Windows failed to raise the last notification that failed after `Show`, if one did since the last call.
    fn take_failure_reason(&self) -> Option<String> {
        None
    }
}

/// The notifier in use and the last delivery problem.
pub struct Notifications {
    notifier: Arc<dyn Notifier>,
    problem: Option<SharedString>,
    /// Where active alerts are kept: `dashboard.json` for live data, or this in-memory set for the demo, whose
    /// synthetic accounts share ids with real ones and must never mark real alerts as already sent.
    memory: Option<BTreeSet<String>>,
}

impl Global for Notifications {}

impl Notifications {
    /// `persist` keeps active alerts in `dashboard.json`; the demo passes false.
    pub fn init(cx: &mut App, notifier: Arc<dyn Notifier>, persist: bool) {
        cx.set_global(Self {
            notifier,
            problem: None,
            memory: (!persist).then(BTreeSet::new),
        });
    }

    /// Replaces the in-memory active set, for the headless UI tests.
    #[cfg(test)]
    pub fn seed_for_test(cx: &mut App, active: BTreeSet<String>) {
        Self::set_active(cx, active);
    }

    /// The alerts already notified and not yet recovered.
    pub fn active(cx: &App) -> BTreeSet<String> {
        match cx.try_global::<Self>().and_then(|global| global.memory.clone()) {
            Some(memory) => memory,
            None => PrefsHub::active_alerts(cx),
        }
    }

    fn set_active(cx: &mut App, active: BTreeSet<String>) {
        if cx.try_global::<Self>().is_some_and(|global| global.memory.is_some()) {
            cx.update_global(|global: &mut Self, _| global.memory = Some(active));
        } else {
            PrefsHub::set_active_alerts(cx, active);
        }
    }

    pub fn status(cx: &App) -> Option<NotifierStatus> {
        cx.try_global::<Self>().map(|global| global.notifier.status())
    }

    /// The last delivery problem, cleared by the next successful notification.
    pub fn problem(cx: &App) -> Option<SharedString> {
        cx.try_global::<Self>().and_then(|global| global.problem.clone())
    }

    /// Sends a sample notification from Settings, so the user can see where alerts appear and whether Windows
    /// shows them.
    pub fn send_test(cx: &mut App) {
        let alert = Alert {
            key: String::new(),
            kind: codexbar_core::alerts::AlertKind::Usage,
            account: "CodexBar".into(),
            metric: "Test".into(),
            value: String::new(),
            title: "CodexBar alerts are on".into(),
            body: "Usage and balance alerts will appear like this.".into(),
            covers: Vec::new(),
        };
        let Some(notifier) = cx.try_global::<Self>().map(|global| global.notifier.clone()) else {
            return;
        };
        let problem = match notifier.status() {
            NotifierStatus::Blocked(reason) => Some(reason),
            NotifierStatus::Ready => notifier.show(&alert, cx).err(),
        };
        cx.update_global(|global: &mut Self, _| global.problem = problem.map(Into::into));
        cx.refresh_windows();
    }

    /// Forgets which alerts were already shown, so conditions that still hold notify again on the next refresh.
    pub fn reset(cx: &mut App) {
        Self::set_active(cx, Default::default());
        cx.refresh_windows();
    }
}

/// Evaluates the accounts that refreshed successfully and delivers new alerts. Accounts whose provider failed are
/// not passed in, so their alerts are neither cleared nor repeated. Alerts end when their condition recovers or on
/// Reset; an account that disappears keeps its keys, which can't notify (re-added accounts get new ids).
pub fn process(cx: &mut App, refreshed: &[AccountSnapshot], now: DateTime<Utc>) {
    let Some(notifier) = cx.try_global::<Notifications>().map(|global| global.notifier.clone()) else {
        return;
    };
    // Which alerts were sent must be saved, or every restart would send them again. A `dashboard.json` from a newer
    // CodexBar can't be written, so alerts pause until that version (or a fixed file) takes over.
    let persists = cx
        .try_global::<Notifications>()
        .is_some_and(|global| global.memory.is_none());
    if persists && PrefsHub::is_read_only(cx) && PrefsHub::alert_settings(cx).enabled {
        cx.update_global(|global: &mut Notifications, _| {
            global.problem = Some(
                "Alerts are paused: dashboard.json is from a newer CodexBar, so sent alerts can't be remembered."
                    .into(),
            );
        });
        return;
    }
    let settings = PrefsHub::alert_settings(cx);
    // A notification Windows failed to raise after accepting it wasn't delivered: forget it, so it is sent again.
    let failed: Vec<String> = notifier
        .take_failed()
        .into_iter()
        .filter(|key| !key.is_empty())
        .collect();
    let reason = notifier.take_failure_reason();
    let mut active = Notifications::active(cx);
    for key in &failed {
        active.remove(key);
    }
    let evaluation = evaluate(&settings, &active, refreshed, now);
    if evaluation.notify.is_empty()
        && evaluation.recovered.is_empty()
        && evaluation.covered.is_empty()
        && failed.is_empty()
    {
        if let Some(reason) = reason {
            cx.update_global(|global: &mut Notifications, _| global.problem = Some(reason.into()));
        } else if notifier.status() == NotifierStatus::Ready {
            // Notifications were turned back on: an old "blocked" message no longer applies.
            cx.update_global(|global: &mut Notifications, _| global.problem = None);
        }
        // Nothing new; still retry an active-alert save that failed earlier.
        Notifications::set_active(cx, active);
        return;
    }

    let mut next = active;
    for key in &failed {
        next.remove(key);
    }
    for key in &evaluation.recovered {
        next.remove(key);
    }
    next.extend(evaluation.covered.iter().cloned());
    let mut problem = None;
    if !evaluation.notify.is_empty() {
        match notifier.status() {
            NotifierStatus::Blocked(reason) => problem = Some(reason),
            NotifierStatus::Ready => {
                for alert in &evaluation.notify {
                    match notifier.show(alert, cx) {
                        Ok(()) => next.extend(alert.keys().cloned()),
                        Err(err) => problem = Some(err),
                    }
                }
            }
        }
    }
    // Windows' reason stands when nothing could be sent again (the test notification has no alert to resend);
    // when the failed alerts were resent successfully it is out of date.
    let problem = problem.or(if failed.is_empty() { reason } else { None });
    cx.update_global(|global: &mut Notifications, _| global.problem = problem.map(Into::into));
    // If this save fails (lock busy), the change stays pending and is retried on the next refresh; Settings shows
    // the save error meanwhile.
    Notifications::set_active(cx, next);
}

/// True when Windows reported notifications it failed to raise that haven't been handled yet.
pub fn has_failed(cx: &App) -> bool {
    cx.try_global::<Notifications>()
        .is_some_and(|global| global.notifier.has_failed())
}

/// Handles notifications Windows failed to raise, right away rather than at the next refresh (which may never come
/// with automatic refresh off). Called from the dashboard's clock with the accounts of the last refresh when settings
/// haven't changed since, so they are sent again now; otherwise with none, which only forgets them, and the next
/// refresh judges them under the current settings.
pub fn retry_failed(cx: &mut App, accounts: &[AccountSnapshot], now: DateTime<Utc>) {
    let waiting = cx
        .try_global::<Notifications>()
        .is_some_and(|global| global.notifier.has_failed());
    if waiting {
        process(cx, accounts, now);
    }
}

/// Keeps notifications in memory: the demo dashboard (so design work never pops real notifications) and tests.
#[derive(Default)]
pub struct RecordingNotifier {
    pub shown: Mutex<Vec<Alert>>,
    pub blocked: Mutex<Option<String>>,
    /// Keys to report as failed after delivery, as Windows' `Failed` event would.
    pub failed: Mutex<Vec<String>>,
    /// The reason to report with them.
    pub reason: Mutex<Option<String>>,
}

impl Notifier for RecordingNotifier {
    fn status(&self) -> NotifierStatus {
        match self.blocked.lock().unwrap().clone() {
            Some(reason) => NotifierStatus::Blocked(reason),
            None => NotifierStatus::Ready,
        }
    }

    fn show(&self, alert: &Alert, _cx: &App) -> Result<(), String> {
        self.shown.lock().unwrap().push(alert.clone());
        Ok(())
    }

    fn take_failed(&self) -> Vec<String> {
        std::mem::take(&mut *self.failed.lock().unwrap())
    }

    fn has_failed(&self) -> bool {
        !self.failed.lock().unwrap().is_empty()
    }

    fn take_failure_reason(&self) -> Option<String> {
        self.reason.lock().unwrap().take()
    }
}

/// macOS and Linux notifications, through GPUI's notification-center support. GPUI reports nothing about delivery, so
/// the notifier only posts where delivery can work and otherwise reports why, which keeps the alert pending (#161).
#[cfg(not(windows))]
#[derive(Default)]
pub struct SystemNotifier;

/// Why this Mac can't show CodexBar's notifications: macOS delivers them only to an app bundle, and `cargo run` starts a
/// plain program.
#[cfg(target_os = "macos")]
fn unavailable() -> Option<String> {
    let bundled = std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    (!bundled).then(|| {
        "macOS shows notifications only from an app bundle, and this CodexBar runs as a plain program. Alerts stay \
         active here until it runs from CodexBar.app."
            .to_owned()
    })
}

/// Why this desktop can't show notifications: no notification service is running or can be started on the session
/// bus.
#[cfg(not(any(windows, target_os = "macos")))]
fn unavailable() -> Option<String> {
    const SERVICE: &str = "org.freedesktop.Notifications";
    let available = (|| -> zbus::Result<bool> {
        let connection = zbus::blocking::Connection::session()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        let name = zbus::names::BusName::try_from(SERVICE)?;
        Ok(bus.name_has_owner(name)?
            || bus
                .list_activatable_names()?
                .iter()
                .any(|activatable| activatable.as_str() == SERVICE))
    })();
    (!available.unwrap_or(false)).then(|| {
        "No notification service is running on this desktop, so alerts can't pop up. They stay active here until \
         one is."
            .to_owned()
    })
}

#[cfg(not(windows))]
impl Notifier for SystemNotifier {
    fn status(&self) -> NotifierStatus {
        match unavailable() {
            Some(reason) => NotifierStatus::Blocked(reason),
            None => NotifierStatus::Ready,
        }
    }

    fn show(&self, alert: &Alert, cx: &App) -> Result<(), String> {
        cx.show_system_notification(gpui_kit::SystemNotification {
            tag: SharedString::from(if alert.key.is_empty() {
                "test".to_owned()
            } else {
                alert.key.clone()
            }),
            title: alert.title.clone().into(),
            body: alert.body.clone().into(),
            actions: Vec::new(),
        });
        Ok(())
    }
}

/// The platform's notifier.
pub fn system_notifier() -> Arc<dyn Notifier> {
    #[cfg(windows)]
    return Arc::new(WindowsNotifier::new());
    #[cfg(not(windows))]
    Arc::new(SystemNotifier)
}

/// Windows toast notifications. The unpackaged app sends them under a per-user AppUserModelID registered in
/// `HKCU\Software\Classes\AppUserModelId` so they show CodexBar's name; the MSIX package (#93) has its own.
#[cfg(windows)]
pub struct WindowsNotifier {
    /// Why the app id couldn't be registered; without it Windows may drop notifications silently.
    registration: Option<String>,
    /// Keys of notifications Windows reported as failed after `Show` (its `Failed` event).
    failed: Arc<Mutex<Vec<String>>>,
    /// The error Windows gave for the last such failure.
    reason: Arc<Mutex<Option<String>>>,
}

/// The AppUserModelID the unpackaged app sends notifications under, and the app identity on macOS and Linux.
pub const APP_ID: &str = "HemSoft.CodexBar";

/// The AppUserModelID notifications go out under: the package's when CodexBar runs from it.
#[cfg(windows)]
fn app_id() -> windows::core::HSTRING {
    crate::package::installed().map_or_else(
        || windows::core::HSTRING::from(APP_ID),
        |installed| windows::core::HSTRING::from(installed.app_user_model_id()),
    )
}

#[cfg(windows)]
impl WindowsNotifier {
    /// Registers the app id for this user and this process (the package needs none). Registration failures are
    /// reported by `status`.
    pub fn new() -> Self {
        let registered = if crate::package::installed().is_some() {
            Ok(())
        } else {
            register_app_id()
        };
        Self {
            registration: registered.err().map(|err| {
                format!(
                    "CodexBar couldn't register for Windows notifications: {}",
                    err.message()
                )
            }),
            failed: Arc::default(),
            reason: Arc::default(),
        }
    }
}

#[cfg(windows)]
impl Notifier for WindowsNotifier {
    fn status(&self) -> NotifierStatus {
        use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
        if let Some(problem) = &self.registration {
            return NotifierStatus::Blocked(problem.clone());
        }
        let setting =
            ToastNotificationManager::CreateToastNotifierWithId(&app_id()).and_then(|notifier| notifier.Setting());
        match setting {
            Ok(NotificationSetting::Enabled) => NotifierStatus::Ready,
            Ok(NotificationSetting::DisabledForApplication) => NotifierStatus::Blocked(
                "Notifications are off for CodexBar. Turn them on in Windows Settings › System › Notifications.".into(),
            ),
            Ok(NotificationSetting::DisabledForUser) => NotifierStatus::Blocked(
                "Notifications are off in Windows. Turn them on in Windows Settings › System › Notifications.".into(),
            ),
            Ok(NotificationSetting::DisabledByGroupPolicy) => {
                NotifierStatus::Blocked("Notifications are turned off by your organization's policy.".into())
            }
            Ok(_) => NotifierStatus::Blocked("Windows doesn't allow notifications for CodexBar.".into()),
            Err(err) => NotifierStatus::Blocked(format!("Windows notifications are unavailable: {}", err.message())),
        }
    }

    fn show(&self, alert: &Alert, _cx: &App) -> Result<(), String> {
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
        use windows::core::HSTRING;
        let xml = toast_xml(&alert.title, &alert.body);
        let result = (|| {
            let document = XmlDocument::new()?;
            document.LoadXml(&HSTRING::from(xml))?;
            let toast = ToastNotification::CreateToastNotification(&document)?;
            // `Show` can succeed and the toast still fail to appear; Windows reports that through `Failed`.
            let failed = self.failed.clone();
            let reason = self.reason.clone();
            let keys: Vec<String> = alert.keys().cloned().collect();
            toast.Failed(&windows::Foundation::TypedEventHandler::new(
                move |_, args: windows::core::Ref<windows::UI::Notifications::ToastFailedEventArgs>| {
                    let code = args.as_ref().and_then(|args| args.ErrorCode().ok());
                    let message = code.map_or_else(
                        || "Windows didn't say why".to_owned(),
                        |code| windows::core::Error::from_hresult(code).message(),
                    );
                    *reason.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(format!("Windows couldn't show a notification: {message}"));
                    failed
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .extend(keys.iter().cloned());
                    Ok(())
                },
            ))?;
            ToastNotificationManager::CreateToastNotifierWithId(&app_id())?.Show(&toast)
        })();
        result.map_err(|err| format!("Notification not shown: {}", err.message()))
    }

    fn take_failed(&self) -> Vec<String> {
        std::mem::take(&mut *self.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    fn has_failed(&self) -> bool {
        !self
            .failed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    fn take_failure_reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// The toast payload: a title line and a body line, escaped for XML.
#[cfg(windows)]
fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        escape(title),
        escape(body)
    )
}

#[cfg(windows)]
fn escape(text: &str) -> String {
    // XML 1.0 forbids most control characters; one in a provider's label would make the whole toast fail to load.
    let text: String = text
        .chars()
        .filter(|ch| matches!(ch, '\t' | '\n' | '\r') || !ch.is_control())
        .collect();
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Registers the AppUserModelID's display name for this user, and claims it for this process, so Windows shows
/// "CodexBar" on notifications from an unpackaged app.
#[cfg(windows)]
fn register_app_id() -> windows::core::Result<()> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
        RegSetValueExW,
    };
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::{HSTRING, PCWSTR};

    let path = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"));
    let mut key = HKEY::default();
    // SAFETY: valid key handle out-pointer and null-terminated strings that outlive the calls.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &path,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let name: Vec<u16> = "CodexBar".encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(name.as_ptr().cast::<u8>(), name.len() * 2);
        let set = RegSetValueExW(key, &HSTRING::from("DisplayName"), None, REG_SZ, Some(bytes)).ok();
        let _ = RegCloseKey(key);
        set?;
        SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;

    #[test]
    fn a_plain_program_reports_notifications_blocked_so_alerts_stay_pending() {
        // A test binary, like `cargo run`, isn't inside an app bundle.
        assert!(matches!(SystemNotifier.status(), NotifierStatus::Blocked(reason) if reason.contains("app bundle")));
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn toast_xml_escapes_markup() {
        let xml = toast_xml("A & B", "<5% left> \"quoted\" it's");
        assert!(xml.contains("<text>A &amp; B</text>"));
        assert!(xml.contains("<text>&lt;5% left&gt; &quot;quoted&quot; it&apos;s</text>"));
    }

    #[test]
    fn toast_xml_drops_characters_xml_cannot_hold() {
        let xml = toast_xml("Team\u{1}\u{8} A", "ok\u{b}");
        assert!(xml.contains("<text>Team A</text>"));
        assert!(xml.contains("<text>ok</text>"));
    }
}
