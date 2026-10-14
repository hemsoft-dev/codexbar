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
use gpui_kit::{AnyWindowHandle, AppContext as _, HeadlessAppContext, px, size};

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
    let handle = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            dashboard = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let handle: AnyWindowHandle = handle.into();
    let dashboard = dashboard.unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage");

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
    let _ = std::fs::remove_dir_all(&settings);
}
