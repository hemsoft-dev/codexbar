#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account_table;
mod alert_details;
mod assets;
mod brand;
// Only Linux can render GPUI headlessly from a test.
#[cfg(all(test, target_os = "linux"))]
mod captures;
mod catalog;
mod claude_sign_in;
mod codex_sign_in;
mod cursor_sign_in;
mod dashboard;
mod focus_cards;
mod gauge;
mod gemini_sign_in;
mod github_sign_in;
mod grok_sign_in;
mod groups_page;
mod handoff;
mod history_view;
mod locale;
mod managed;
mod mark;
mod notifications;
mod opencode_sign_in;
mod package;
mod placement;
mod prefs_hub;
mod providers;
mod ring_size;
#[cfg(feature = "capture")]
mod self_capture;
mod settings_hub;
mod settings_view;
mod smooth_plot;
mod spend_view;
mod status;
mod theme;
mod tray;
#[cfg(test)]
mod ui_tests;
mod usage_bar;
#[cfg(windows)]
mod widget_builder;
#[cfg(windows)]
mod widget_feed;
#[cfg(windows)]
mod widgets;
mod zoom;

use std::sync::{Arc, Mutex};

use codexbar_store::{HistoryStore, default_history_path};
use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions};

use crate::dashboard::{Dashboard, DashboardView, DataSource};
use crate::tray::TrayCommand;

/// How long usage history is kept (#85).
const HISTORY_RETENTION: chrono::Duration = chrono::Duration::days(30);

fn is_demo() -> bool {
    std::env::var_os("CODEXBAR_DEMO").is_some_and(|value| value == "1") || std::env::args().any(|arg| arg == "--demo")
}

fn data_source() -> DataSource {
    let demo = is_demo();
    if demo {
        return DataSource::Demo;
    }
    let history = HistoryStore::open(default_history_path(), HISTORY_RETENTION, chrono::Utc::now());
    DataSource::Live {
        history: Arc::new(Mutex::new(history)),
        providers: Arc::new(providers::enabled),
    }
}

/// This instance's name part, per user: `DOMAIN.user`, or `Demo.DOMAIN.user` for the demo. On macOS and Linux the
/// settings folder is already the user's own, so the name only tells the demo apart.
fn instance() -> String {
    #[cfg(not(windows))]
    let user = String::from("user");
    #[cfg(windows)]
    let user: String = format!(
        "{}.{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    )
    .chars()
    .filter(|ch| *ch != '\\')
    .collect();
    if is_demo() { format!("Demo.{user}") } else { user }
}

/// One CodexBar per Windows user, across sessions (two Remote Desktop sessions share one profile), so two instances
/// never send the same alert twice or write history and preferences over each other. The mutex is in the global
/// namespace and named per user, so other users on the machine run their own. The demo has its own name, so design
/// work can run beside the real app. A second launch exits; the first one is already in the notification area.
#[cfg(windows)]
fn already_running() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::HSTRING;
    let name = format!(r"Global\HemSoft.CodexBar.{}", instance());
    // SAFETY: a named mutex with default security; the handle is kept open for the life of the process.
    match unsafe { CreateMutexW(None, false, &HSTRING::from(name)) } {
        // The handle is never closed: Windows releases the mutex when the process exits.
        Ok(_handle) => (unsafe { GetLastError() }) == ERROR_ALREADY_EXISTS,
        // Without the mutex, run anyway rather than refuse to start.
        Err(_) => false,
    }
}

/// Makes every file and folder this process (and the CLIs it starts) creates private to the user, and the settings
/// folder too if an earlier run made it with default permissions.
#[cfg(unix)]
fn private_files() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: umask only sets this process's file mode creation mask; it can't fail.
    unsafe { libc::umask(0o077) };
    let dir = codexbar_store::settings::Settings::default_dir();
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

/// On macOS and Linux the lock is an advisory lock on a file in the settings folder, held until the process exits.
#[cfg(not(windows))]
fn already_running() -> bool {
    use std::sync::OnceLock;
    static HELD: OnceLock<std::fs::File> = OnceLock::new();
    let dir = codexbar_store::settings::Settings::default_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("instance.{}.lock", instance())));
    match file {
        Ok(file) => match file.try_lock() {
            Ok(()) => {
                let _ = HELD.set(file);
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            // Without the lock, run anyway rather than refuse to start.
            Err(std::fs::TryLockError::Error(_)) => false,
        },
        Err(_) => false,
    }
}

fn main() {
    // Settings, history and sign-ins in ~/.codexbar stay readable by this user only (#161).
    #[cfg(unix)]
    private_files();
    // Windows starts CodexBar this way for the Widgets board (#94). The provider runs beside the tray app.
    #[cfg(windows)]
    if std::env::args().any(|arg| arg == widgets::SERVER_ARG) {
        if let Err(err) = widgets::serve() {
            eprintln!("codexbar: widget provider failed: {}", err.message());
            std::process::exit(1);
        }
        return;
    }
    // `--package-status <file> [<trigger>]` reports the MSIX package and exits (#93); it runs beside an open
    // CodexBar. With a trigger file it first waits for that file, the way a running CodexBar meets a later update.
    #[cfg(windows)]
    let mut args = std::env::args().skip_while(|arg| arg != "--package-status");
    #[cfg(windows)]
    if args.next().is_some() {
        let file = args.next().map(std::path::PathBuf::from);
        let trigger = args.next().map(std::path::PathBuf::from);
        let written = file.map(|path| package::write_status(&path, trigger.as_deref()));
        std::process::exit(if matches!(written, Some(Ok(()))) { 0 } else { 1 });
    }
    let focus = handoff::FocusRequest::from_args(std::env::args());
    // The handoff event exists before this instance takes the single-instance lock, so a later launch that finds
    // the lock taken always reaches a live event (one this instance holds open) rather than one it creates alone.
    let settings_dir = codexbar_store::settings::Settings::default_dir();
    let handoff_listener = handoff::listen(&settings_dir, &instance());
    if already_running() {
        // A widget tile (#95) or a second launch: the running CodexBar shows its window, on the tile's account.
        if let Some(request) = &focus {
            let _ = handoff::write_request(&settings_dir, request);
        }
        handoff::signal(&settings_dir, &instance());
        return;
    }
    gpui_kit::application().with_assets(assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        // macOS and Linux name the app on its notifications by this identity; Windows registers its own (notifications).
        #[cfg(not(windows))]
        cx.set_app_identity(notifications::APP_ID, "CodexBar");
        // A menu-bar app on macOS: no Dock icon and no app menu; the menu-bar icon opens the dashboard (#161).
        #[cfg(target_os = "macos")]
        cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
        settings_hub::SettingsHub::init(cx);
        codex_sign_in::init(cx);
        claude_sign_in::init(cx);
        cursor_sign_in::init(cx);
        github_sign_in::init(cx);
        opencode_sign_in::init(cx);
        gemini_sign_in::init(cx);
        gemini_sign_in::clean_up(cx);
        grok_sign_in::init(cx);
        grok_sign_in::clean_up(cx);
        github_sign_in::clean_up(cx);
        let dir = settings_hub::SettingsHub::global(cx).dir().to_owned();
        if is_demo() {
            prefs_hub::PrefsHub::init_in_memory(cx);
        } else {
            prefs_hub::PrefsHub::init(cx, &dir);
        }
        // After the preferences, so the saved appearance applies from the first frame.
        theme::init(cx);
        // The demo never pops real notifications; its alerts are kept in memory.
        let notifier: std::sync::Arc<dyn notifications::Notifier> = if is_demo() {
            std::sync::Arc::new(notifications::RecordingNotifier::default())
        } else {
            notifications::system_notifier()
        };
        notifications::Notifications::init(cx, notifier, !is_demo());
        zoom::init(cx);
        // The demo never checks the real update channel.
        package::Updates::init(cx, !is_demo());
        if !is_demo() {
            package::migrate_startup(&dir);
        }
        package::StartupSetting::init(cx);

        // Where the window was last (#194), or centered on the primary display the first time.
        let saved_placement = prefs_hub::PrefsHub::window_placement(cx);
        let restored = placement::restore_in(saved_placement.as_ref(), cx);
        let bounds = restored.as_ref().map_or_else(
            || WindowBounds::Windowed(Bounds::centered(None, placement::default_size(), cx)),
            |restored| restored.bounds,
        );
        let options = WindowOptions {
            window_bounds: Some(bounds),
            display_id: restored.as_ref().and_then(|restored| restored.display),
            window_min_size: Some(placement::min_size()),
            // Linux desktops find the window's icon and name through the matching HemSoft.CodexBar.desktop (#167).
            app_id: (!cfg!(windows)).then(|| notifications::APP_ID.to_owned()),
            ..TitleBar::window_options()
        };

        let (handle, dashboard) = gpui_kit::open_window(options, cx, |window, cx| {
            // Window managers and the macOS Window menu name the window by its title; Windows keeps the untitled
            // window it has always had.
            #[cfg(not(windows))]
            window.set_window_title("CodexBar");
            // Closing hides: the window lives as long as the app, so the tray reopens the last view instantly.
            window.on_window_should_close(cx, |window, cx| {
                tray::hide(window, cx);
                false
            });
            cx.new(|cx| Dashboard::new(data_source(), window, cx))
        })
        .expect("failed to open the dashboard window");
        // Start with Windows (the package's startup task) goes straight to the notification area.
        // Out of the way at a start with Windows, and when it was minimized or hidden to the tray when it last ran,
        // unless this start was asked to show something: a widget tile's account, or Settings.
        let asked_to_show = focus.is_some() || std::env::args().any(|arg| arg == "--settings");
        let stays_hidden = restored.as_ref().is_some_and(|restored| restored.hidden) && !asked_to_show;
        if package::launched_at_startup() || stays_hidden {
            let _ = cx.update_window(handle, |_, window, cx| tray::hide(window, cx));
        } else {
            cx.activate(true);
        }
        if std::env::args().any(|arg| arg == "--settings") {
            dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx));
        }
        if let Some(request) = focus {
            dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx));
        }
        // Later launches hand over here: show the window, on the requested account when there is one.
        if let Some(listener) = handoff_listener {
            let focus_target = dashboard.clone();
            let request_dir = dir.clone();
            cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    if !listener.signaled() {
                        continue;
                    }
                    let request = handoff::take_request(&request_dir);
                    cx.update(|cx| {
                        if let Some(request) = &request {
                            focus_target.update(cx, |dashboard, cx| dashboard.focus_account(request, cx));
                        }
                        let _ = cx.update_window(handle, |_, window, cx| tray::show(window, cx));
                    });
                }
            })
            .detach();
        }

        let tooltip_source = dashboard.clone();
        let result = tray::init(cx, move |command, cx| match command {
            TrayCommand::Toggle | TrayCommand::Open => {
                let _ = cx.update_window(handle, |_, window, cx| {
                    let hide = command == TrayCommand::Toggle && dashboard.read(cx).should_hide_on_toggle(window);
                    if hide {
                        tray::hide(window, cx)
                    } else {
                        tray::show(window, cx)
                    }
                });
            }
            TrayCommand::Refresh => dashboard.update(cx, |dashboard, cx| dashboard.refresh(cx)),
            TrayCommand::Settings => {
                dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx));
                let _ = cx.update_window(handle, |_, window, cx| tray::show(window, cx));
            }
            TrayCommand::Quit => cx.quit(),
        });
        if let Err(err) = result {
            eprintln!("codexbar: tray icon unavailable: {err}");
            // Without the menu-bar icon a Mac needs the Dock icon to reach the window.
            #[cfg(target_os = "macos")]
            cx.set_activation_policy(gpui_kit::ActivationPolicy::Regular);
            // Without a tray icon the window is the only way in, even after a start with Windows.
            let _ = cx.update_window(handle, |_, window, cx| tray::show(window, cx));
        }
        #[cfg(feature = "capture")]
        self_capture::start(cx, handle, tooltip_source.clone());
        // Restored accounts were shown before the tray icon existed; give it their text now.
        tooltip_source.update(cx, |dashboard, cx| dashboard.publish_tooltip(cx));
    });
}
