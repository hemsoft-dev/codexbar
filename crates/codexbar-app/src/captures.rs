//! Screenshots of the demo dashboard for pull requests, rendered offscreen by GPUI's Linux headless renderer (wgpu), so
//! no window ever appears on a desktop. Windows has no headless renderer in GPUI, and the macOS one needs the Mac
//! platform's text system, which can only be created on the main thread, never a test's.
//!
//! `CODEXBAR_CAPTURE_DIR=<folder> cargo test -p codexbar-app captures -- --ignored` writes one PNG per view.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use codexbar_store::credentials::MemoryCredentialStore;
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, HeadlessAppContext, InputEvent as _, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, point, px, size,
};

use crate::dashboard::{Dashboard, DashboardView, DataSource};
use crate::notifications::RecordingNotifier;
use crate::settings_hub::SettingsHub;
use crate::{theme, zoom};

/// Two accounts with saved keys, so Settings › Accounts has rows to show.
const SETTINGS: &str = r#"{ "accountConfigurationVersion": 1, "accounts": [
    { "id": "or-1", "providerId": "OpenRouter", "displayLabel": "Work", "enabled": true,
      "authenticationMethod": "ApiKey" },
    { "id": "ms-1", "providerId": "Moonshot", "displayLabel": "Kimi", "enabled": true,
      "authenticationMethod": "ApiKey" } ] }"#;

/// The Work account's adapter when OpenRouter rejects its key (#188): the failure row offers Open settings.
struct ExpiredKey;

impl codexbar_providers::UsageProvider for ExpiredKey {
    fn name(&self) -> &'static str {
        codexbar_core::Provider::OpenRouter.display_name()
    }

    fn account_id(&self) -> Option<&str> {
        Some("or-1")
    }

    fn account_label(&self) -> Option<&str> {
        Some("Work")
    }

    fn fetch(
        &self,
        _: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<codexbar_core::AccountSnapshot>, codexbar_providers::ProviderError> {
        Err(codexbar_providers::ProviderError::Expired {
            hint: "Enter a new key in CodexBar settings.",
        })
    }
}

fn save(cx: &mut HeadlessAppContext, handle: AnyWindowHandle, dir: &Path, name: &str) {
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
    let image = cx
        .capture_screenshot(handle)
        .expect("the headless renderer draws the window");
    image.save(dir.join(format!("{name}.png"))).unwrap();
}

#[test]
#[ignore = "writes screenshots; run with CODEXBAR_CAPTURE_DIR set"]
fn captures() {
    let Some(out) = std::env::var_os("CODEXBAR_CAPTURE_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let settings = std::env::temp_dir().join(format!("codexbar-captures-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&settings);
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(settings.join("settings.json"), SETTINGS).unwrap();

    let text_system = gpui_kit::platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(text_system, Arc::new(crate::assets::Assets), || {
        gpui_kit::platform::current_headless_renderer()
    });
    let store = MemoryCredentialStore::default();
    use codexbar_store::credentials::CredentialStore as _;
    store.write("or-1", "sk-or-capture").unwrap();
    store.write("ms-1", "sk-ms-capture").unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        theme::init(cx);
        SettingsHub::init_with(cx, &settings, Arc::new(store));
        crate::prefs_hub::PrefsHub::init_in_memory(cx);
        zoom::init(cx);
        crate::notifications::Notifications::init(cx, Arc::new(RecordingNotifier::default()), false);
        crate::package::Updates::init(cx, false);
        crate::package::StartupSetting::init(cx);
    });
    let mut dashboard = None;
    // Tall enough for the demo's rows and the focus cards under them (#174); 960 cut the cards off.
    let handle = cx
        .open_window(size(px(1440.), px(1400.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            dashboard = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let handle: AnyWindowHandle = handle.into();
    let dashboard = dashboard.unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage");

    // #178: a row dragged to a new place. Manual order first; then the first row is pressed, carried to the lower
    // half of the third row (frames along the way show the preview and the gap line), and released.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("order-mode", 1usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "drag-0-manual");
    let (from, to) = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let from = window.find(("row", 0usize)).bounds();
            let to = window.find(("row", 2usize)).bounds();
            (from.center(), point(to.center().x, to.origin.y + to.size.height * 0.75))
        })
        .unwrap();
    let modifiers = Modifiers::default();
    let mouse = |cx: &mut HeadlessAppContext, event: gpui_kit::PlatformInput| {
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_event(event, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let along = |fraction: f32| -> Point<Pixels> {
        point(from.x + (to.x - from.x) * fraction, from.y + (to.y - from.y) * fraction)
    };
    mouse(
        &mut cx,
        MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
    );
    for step in 1..=4 {
        mouse(
            &mut cx,
            MouseMoveEvent {
                position: along(step as f32 / 4.),
                pressed_button: Some(MouseButton::Left),
                modifiers,
            }
            .to_platform_input(),
        );
        save(&mut cx, handle, &out, &format!("drag-{step}-moving"));
    }
    mouse(
        &mut cx,
        MouseUpEvent {
            button: MouseButton::Left,
            position: to,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
    );
    save(&mut cx, handle, &out, "drag-5-dropped");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "drag-5-dropped-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.run_until_parked();

    // #172: the X Subscription card, with its two weekly limits and the credits.
    let request = crate::handoff::FocusRequest {
        account: "x-subscription".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-x-subscription");
    // #180: the Usage by day line, on a share headline (above, percent axis) and on the team account's credits
    // headline (count axis), in both appearances.
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-x-subscription-dark");
    let request = crate::handoff::FocusRequest {
        account: "codex-team".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-codex-team-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-codex-team");

    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::History, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "history");

    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    cx.run_until_parked();
    // Settings pages in sidebar order; Widgets exists only on Windows.
    let pages = ["general", "accounts", "alerts", "groups", "appearance", "about"];
    for (ix, page) in pages.iter().enumerate() {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.within("settings-sidebar").click(format!("0-{ix}"), cx)
        })
        .unwrap();
        cx.run_until_parked();
        save(&mut cx, handle, &out, &format!("settings-{page}"));
    }
    // The X Subscription and Grok entries on the Accounts page.
    let x_subscription = crate::catalog::PROVIDERS
        .iter()
        .position(|info| info.id == codexbar_store::settings::names::X_SUBSCRIPTION)
        .unwrap();
    for entry in ["0-1".to_owned(), format!("0-1-{x_subscription}")] {
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.within("settings-sidebar").click(entry, cx)
        })
        .unwrap();
        cx.run_until_parked();
    }
    save(&mut cx, handle, &out, "settings-accounts-x-subscription");

    // The Accounts page and the OpenRouter dialog with OPENROUTER_API_KEY set (#186): the saved key still leads the
    // Work row, and the dialog says the variable is in use until a key is saved.
    cx.update(|cx| {
        SettingsHub::use_environment(cx, |name| {
            (name == "OPENROUTER_API_KEY").then(|| "sk-or-env".to_owned())
        })
    });
    // The page is still scrolled to the X Subscription group; its OpenRouter entry scrolls to the OpenRouter group.
    let openrouter = crate::catalog::PROVIDERS
        .iter()
        .position(|info| info.id == codexbar_store::settings::names::OPENROUTER)
        .unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("settings-sidebar").click(format!("0-1-{openrouter}"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "settings-accounts-env");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("add-OpenRouter", cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "settings-add-openrouter-env");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "settings-add-openrouter-env-dark");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "settings-accounts-env-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));

    // #188: a live dashboard whose Work key OpenRouter rejects. Its failure row offers Open settings, which lands on
    // the Work account's dialog in Settings › Accounts.
    let live = cx
        .open_window(size(px(1440.), px(720.)), |window, cx| {
            let source = DataSource::Live {
                history: Arc::new(std::sync::Mutex::new(codexbar_store::HistoryStore::in_memory(
                    chrono::Duration::days(30),
                ))),
                providers: Arc::new(|_| vec![Arc::new(ExpiredKey) as Arc<dyn codexbar_providers::UsageProvider>]),
            };
            let view = cx.new(|cx| Dashboard::new(source, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let live: AnyWindowHandle = live.into();
    cx.run_until_parked();
    save(&mut cx, live, &out, "usage-failure-open-settings");
    cx.update_window(live, |_, window, cx| {
        window.render_frame(cx);
        window.click("fix-OpenRouter-or-1", cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, live, &out, "settings-after-open-settings");

    // #192: Greptile is opt-in. With GREPTILE_API_KEY in the environment (a dummy, through the hub's lookup) and no
    // Greptile account, the real provider factory builds no Greptile adapter: Usage shows no row and no banner, and
    // Settings shows the provider off. An account added and switched on then reads the key from the variable.
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark);
        SettingsHub::use_environment(cx, |name| {
            (name == "GREPTILE_API_KEY").then(|| "grp-capture-dummy".to_owned())
        });
    });
    let greptile_only: crate::dashboard::ProviderFactory = Arc::new(|hub: &SettingsHub| {
        crate::providers::enabled(hub)
            .into_iter()
            .filter(|provider| provider.name() == "Greptile")
            .collect()
    });
    let mut greptile_dashboard = None;
    let greptile = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let source = DataSource::Live {
                history: Arc::new(std::sync::Mutex::new(codexbar_store::HistoryStore::in_memory(
                    chrono::Duration::days(30),
                ))),
                providers: greptile_only,
            };
            let view = cx.new(|cx| Dashboard::new(source, window, cx));
            greptile_dashboard = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let greptile: AnyWindowHandle = greptile.into();
    let greptile_view = greptile_dashboard.unwrap();
    cx.run_until_parked();
    save(&mut cx, greptile, &out, "greptile-unconfigured-usage-dark");
    cx.update(|cx| greptile_view.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    let greptile_group = crate::catalog::PROVIDERS
        .iter()
        .position(|info| info.id == codexbar_store::settings::names::GREPTILE)
        .unwrap();
    for entry in ["0-1".to_owned(), format!("0-1-{greptile_group}")] {
        cx.update_window(greptile, |_, window, cx| {
            window.render_frame(cx);
            window.within("settings-sidebar").click(entry, cx)
        })
        .unwrap();
        cx.run_until_parked();
    }
    save(&mut cx, greptile, &out, "greptile-unconfigured-settings-dark");
    cx.update(|cx| {
        SettingsHub::update(cx, |settings| {
            settings.upsert(codexbar_store::settings::AccountRecord::new(
                codexbar_store::settings::names::GREPTILE,
                "Acme",
                codexbar_store::settings::AuthMethod::ApiKey,
            ))
        })
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, greptile, &out, "greptile-env-account-settings-dark");
    cx.update(|cx| {
        SettingsHub::use_environment(cx, |_| None);
        crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System);
    });
    let _ = std::fs::remove_dir_all(&settings);
}
