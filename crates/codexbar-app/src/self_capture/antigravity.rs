//! Synthetic CLI responses for native-window connection-flow evidence. Never used outside --demo captures.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use codexbar_store::settings::{AccountRecord, AuthMethod, names};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};

use crate::dashboard::{Dashboard, DashboardView};
use crate::gemini_sign_in::{PendingSignIn, Service, SignInService};
use crate::settings_hub::SettingsHub;

#[derive(Default)]
struct Fixture {
    mode: AtomicUsize,
    checks: AtomicUsize,
    opens: AtomicUsize,
    finished: Arc<AtomicBool>,
}
struct Pending(Arc<AtomicBool>);
impl PendingSignIn for Pending {
    fn finished(&mut self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
impl SignInService for Fixture {
    fn check(&self) -> Result<(), String> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        match self.mode.load(Ordering::SeqCst) {
            1 => Ok(()),
            2 => Err("Install the Antigravity CLI (agy), then try again.".into()),
            _ => Err("The Antigravity CLI didn't report coding usage. Open its sign-in, then check again.".into()),
        }
    }
    fn open(&self, _: &Path) -> Result<Box<dyn PendingSignIn>, String> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.finished.store(false, Ordering::SeqCst);
        Ok(Box::new(Pending(self.finished.clone())))
    }
}
fn click(cx: &mut AsyncApp, handle: AnyWindowHandle, id: &'static str) {
    cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
    })
    .expect("capture click");
}
fn connected(cx: &mut AsyncApp) -> bool {
    cx.update(|cx| {
        SettingsHub::global(cx)
            .settings()
            .accounts()
            .iter()
            .find(|account| account.id == "antigravity-fixture")
            .unwrap()
            .gemini_coding
    })
}
struct Recording {
    started: Instant,
    timestamps: Vec<f64>,
}
impl Recording {
    async fn frames(&mut self, cx: &mut AsyncApp, handle: AnyWindowHandle, dir: &Path) {
        for _ in 0..8 {
            let name = format!("antigravity-frame-{:04}", self.timestamps.len());
            assert!(super::save(cx, handle, dir, &name));
            self.timestamps.push(self.started.elapsed().as_secs_f64());
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    }
}

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    assert!(crate::is_demo());
    let fixture = Arc::new(Fixture::default());
    cx.update(|cx| {
        assert!(
            SettingsHub::global(cx).dir().starts_with(dir),
            "capture must use isolated settings under its evidence directory"
        );
        assert!(
            SettingsHub::global(cx).settings().accounts().is_empty(),
            "never include real configured accounts"
        );
        cx.set_global(Service(fixture.clone()));
        let mut record = AccountRecord::new(names::GEMINI, "Demo account", AuthMethod::Automatic);
        record.id = "antigravity-fixture".into();
        SettingsHub::update(cx, |settings| settings.upsert(record)).unwrap();
        crate::prefs_hub::PrefsHub::set_appearance(cx, crate::theme::Appearance::Light);
        dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx));
    });
    super::settle(cx).await;
    cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            window.within("settings-sidebar").click("0-1", cx);
            window.render_frame(cx);
            window.within("settings-sidebar").click("0-1-10", cx);
        })
    })
    .expect("Gemini settings page");
    super::settle(cx).await;
    let mut saved = Vec::new();
    let mut recording = Recording {
        started: Instant::now(),
        timestamps: Vec::new(),
    };
    macro_rules! capture {
        ($name:literal) => {{
            recording.frames(cx, handle, dir).await;
            assert!(super::save(cx, handle, dir, $name));
            saved.push($name.to_owned());
        }};
    }
    assert!(!connected(cx));
    capture!("antigravity-entry");
    click(cx, handle, "sign-in-antigravity-fixture");
    super::settle(cx).await;
    assert!(!connected(cx));
    assert_eq!(fixture.opens.load(Ordering::SeqCst), 0);
    capture!("antigravity-needs-sign-in");
    click(cx, handle, "antigravity-open-cli");
    assert_eq!(fixture.opens.load(Ordering::SeqCst), 1);
    capture!("antigravity-handoff");
    fixture.finished.store(true, Ordering::SeqCst);
    click(cx, handle, "antigravity-check");
    super::settle(cx).await;
    assert!(!connected(cx));
    capture!("antigravity-retry");
    click(cx, handle, "antigravity-open-cli");
    assert_eq!(fixture.opens.load(Ordering::SeqCst), 2);
    cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.close_dialog(cx);
        })
    })
    .expect("cancel connection");
    super::settle(cx).await;
    assert!(!connected(cx));
    capture!("antigravity-cancelled");
    click(cx, handle, "sign-in-antigravity-fixture");
    super::settle(cx).await;
    click(cx, handle, "antigravity-open-cli");
    fixture.mode.store(1, Ordering::SeqCst);
    click(cx, handle, "antigravity-check");
    super::settle(cx).await;
    assert!(connected(cx));
    capture!("antigravity-connected");
    click(cx, handle, "sign-out-antigravity-fixture");
    super::settle(cx).await;
    let opened = fixture.opens.load(Ordering::SeqCst);
    click(cx, handle, "sign-in-antigravity-fixture");
    super::settle(cx).await;
    assert!(connected(cx));
    assert_eq!(fixture.opens.load(Ordering::SeqCst), opened);
    capture!("antigravity-reused");
    click(cx, handle, "sign-out-antigravity-fixture");
    fixture.mode.store(2, Ordering::SeqCst);
    super::settle(cx).await;
    click(cx, handle, "sign-in-antigravity-fixture");
    super::settle(cx).await;
    assert!(!connected(cx));
    capture!("antigravity-missing-cli");
    let evidence = serde_json::json!({"synthetic":true, "realGoogleConsent":false, "timestamps":recording.timestamps,
        "checks":fixture.checks.load(Ordering::SeqCst), "cliHandoffs":fixture.opens.load(Ordering::SeqCst)});
    std::fs::write(dir.join("antigravity-recording.json"), evidence.to_string()).unwrap();
    cx.update(|cx| {
        cx.update_window(handle, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.close_dialog(cx);
        })
    })
    .expect("close fixture dialog");
    saved
}
