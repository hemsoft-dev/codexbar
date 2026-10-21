//! Screenshots of the demo dashboard for pull requests, rendered offscreen by GPUI's Linux headless renderer (wgpu), so
//! no window ever appears on a desktop. Windows has no headless renderer in GPUI, and the macOS one needs the Mac
//! platform's text system, which can only be created on the main thread, never a test's.
//!
//! `CODEXBAR_CAPTURE_DIR=<folder> cargo test -p codexbar-app captures -- --ignored` writes one PNG per view.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use codexbar_store::credentials::MemoryCredentialStore;
use codexbar_store::focus::{LimitChoice, LimitView};
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

/// Usage runway bars (#234): one window under its limit, the Pi dashboard's 112% overrun, a used-up window, one past
/// the scale's 125%, a tight one and one too young to project.
struct RunwayAccounts;

impl codexbar_providers::UsageProvider for RunwayAccounts {
    fn name(&self) -> &'static str {
        codexbar_core::Provider::Codex.display_name()
    }

    fn fetch(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<codexbar_core::AccountSnapshot>, codexbar_providers::ProviderError> {
        use chrono::Duration;
        use codexbar_core::{AccountId, AccountSnapshot, Metric, Pace, Provider};
        // `used` now and the share at the reset `minutes` from now, as a steady pace.
        let window_in = |label: &str, used: f64, at_reset: f64, minutes: i64| Metric::Window {
            label: label.into(),
            used,
            resets_at: now + Duration::minutes(minutes),
            pace: (at_reset > used).then(|| Pace::per_hour((at_reset - used) * 60.0 / minutes as f64)),
            length: None,
        };
        let window = |label: &str, used: f64, at_reset: f64, hours: i64| window_in(label, used, at_reset, hours * 60);
        let account = |id: &str, label: &str, metrics: Vec<Metric>| {
            AccountSnapshot::new(AccountId::new(id), Provider::Codex, metrics, now).with_label(label)
        };
        Ok(vec![
            account(
                "runway-over",
                "112% at reset",
                vec![
                    window("Weekly", 0.17, 1.12, 142),
                    window("5-hour window", 1.0, 1.0, 2),
                    window("Weekly Opus", 0.40, 0.70, 142),
                ],
            ),
            account(
                "runway-under",
                "under the limit",
                vec![window("Weekly", 0.42, 0.70, 72)],
            ),
            account("runway-out", "used up", vec![window("5-hour window", 1.0, 1.3, 2)]),
            account("runway-clipped", "past 125%", vec![window("Weekly", 0.60, 1.60, 96)]),
            account("runway-tight", "tight", vec![window("5-hour window", 0.80, 0.88, 3)]),
            account("runway-young", "too young", vec![window("Weekly", 0.05, 0.0, 160)]),
            // The Pi dashboard's Codex weekly window at 2026-10-21 12:27 UTC: 21% used, 118% at the reset 5d 18h 5m
            // away, which it shows as "118% runs out in 4d 16h".
            account(
                "runway-pi",
                "Pi dashboard's Codex weekly",
                vec![window_in("Weekly", 0.21, 1.18, (5 * 24 + 18) * 60 + 5)],
            ),
        ])
    }
}

/// GitHub for a Copilot Business organization with no enterprise (#233), answered from recorded fixtures so no request
/// leaves the harness. `owner` owns the organization without a Copilot seat; the report is GitHub's live answer for
/// hemsoft-dev on 2026-10-21 with a metered model added, renamed, and `empty` is a month with no usage yet.
struct BusinessOrgGitHub {
    empty: bool,
}

const BUSINESS_REPORT: &str = r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme-biz","product":"copilot",
    "usageItems":[{"product":"Copilot","sku":"Copilot AI Credits","model":"Code Review model","unitType":"ai-credits",
    "pricePerUnit":0.01,"grossQuantity":101.238535,"grossAmount":1.01238535,"discountQuantity":101.238535,
    "discountAmount":1.01238535,"netQuantity":0.0,"netAmount":0.0},{"product":"Copilot","sku":"Copilot AI Credits",
    "model":"Claude Sonnet 4.5","unitType":"ai-credits","pricePerUnit":0.01,"grossQuantity":2650.0,"grossAmount":26.5,
    "discountQuantity":2650.0,"discountAmount":26.5,"netQuantity":0.0,"netAmount":0.0}]}"#;
const BUSINESS_EMPTY_REPORT: &str =
    r#"{"timePeriod":{"year":2026,"month":10},"organization":"acme-biz","product":"copilot","usageItems":[]}"#;
const BUSINESS_SEATS: &str = r#"{"seat_breakdown":{"total":2,"active_this_cycle":2},"plan_type":"business"}"#;
const BUSINESS_BUDGETS: &str = r#"{"budgets":[{"budget_type":"BundlePricing","budget_product_sku":"ai_credits",
    "budget_scope":"organization","budget_amount":25,"prevent_further_usage":true,"budget_entity_name":"acme-biz",
    "budget_alerting":{"will_alert":true}}],"has_next_page":false}"#;

impl codexbar_providers::HttpClient for BusinessOrgGitHub {
    fn get(
        &self,
        url: &str,
        _: &[(&str, &str)],
    ) -> Result<codexbar_providers::HttpResponse, codexbar_providers::ProviderError> {
        let body = if url.contains("/organizations/acme-biz/settings/billing/ai_credit/usage") {
            if self.empty {
                BUSINESS_EMPTY_REPORT
            } else {
                BUSINESS_REPORT
            }
        } else if url.contains("/orgs/acme-biz/copilot/billing") {
            BUSINESS_SEATS
        } else if url.contains("/organizations/acme-biz/settings/billing/budgets") {
            BUSINESS_BUDGETS
        } else {
            // copilot_internal/user among them: the owner has no Copilot seat.
            return Ok(codexbar_providers::HttpResponse::new(404, ""));
        };
        Ok(codexbar_providers::HttpResponse::new(200, body))
    }
}

/// No GitHub CLI: the capture's account brings its own token.
struct NoCommands;

impl codexbar_providers::CommandRunner for NoCommands {
    fn run(
        &self,
        _: &str,
        _: &[&str],
        _: std::time::Duration,
    ) -> Result<codexbar_providers::CommandOutput, codexbar_providers::CommandError> {
        Err(codexbar_providers::CommandError::NotFound)
    }
}

/// The real Copilot provider over the fixture, billing acme-biz with no enterprise.
fn business_org_provider(empty: bool) -> Arc<dyn codexbar_providers::UsageProvider> {
    use codexbar_providers::copilot::{CopilotProvider, OrgBilling};
    Arc::new(
        CopilotProvider::new(BusinessOrgGitHub { empty }, NoCommands)
            .without_cli()
            .with_login("owner", "gho_capture")
            .with_billing(
                "owner",
                OrgBilling {
                    enterprise: None,
                    organization: "acme-biz".to_owned(),
                    pool_total: None,
                },
            ),
    )
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

    // #200: the focus cards at 150% zoom in a 1440×960 window, light and dark, scrolled to the cards: the charts keep
    // their room (the card grows, the dashboard scrolls) and the ring stays at its cap.
    let zoomed = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let zoomed: AnyWindowHandle = zoomed.into();
    cx.update(|cx| zoom::set_level(1.5, cx));
    cx.run_until_parked();
    for (appearance, name) in [
        (theme::Appearance::System, "focus-cards-zoom150"),
        (theme::Appearance::Dark, "focus-cards-zoom150-dark"),
    ] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, appearance));
        cx.update_window(zoomed, |_, window, cx| {
            window.render_frame(cx);
            window.scroll(
                "main-scroll",
                gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        save(&mut cx, zoomed, &out, name);
    }
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System);
        zoom::set_level(1.0, cx);
    });
    cx.update_window(zoomed, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    // #195: wide enough, the table shows the 5-hour and weekly windows side by side; rows without one leave the
    // cells blank. A second demo window at 2700×1400 (the wide columns need more since #204, and more again for
    // #234's At reset columns), light and dark, closed again before the drag below.
    let wide = cx
        .open_window(size(px(2700.), px(1400.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let wide: AnyWindowHandle = wide.into();
    cx.run_until_parked();
    save(&mut cx, wide, &out, "usage-wide");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, wide, &out, "usage-wide-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.update_window(wide, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

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

    // #217: Copilot HemSoft's monthly reset drops its trend from 97% to 3%; both trend charts draw it inside their
    // plots, light and dark.
    let request = crate::handoff::FocusRequest {
        account: "copilot-hemsoft".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-copilot-reset");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-copilot-reset-dark");
    // #229: the same reset as smooth curves at 150% zoom, scrolled to the cards.
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.update(|cx| zoom::set_level(1.5, cx));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "main-scroll",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "usage-copilot-reset-zoom150");
    cx.update(|cx| zoom::set_level(1.0, cx));
    cx.run_until_parked();
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.run_until_parked();

    // #218: Claude's cards by default, then with both its 5-hour and weekly rings chosen, light and dark, and the
    // gauge card's menu open.
    let request = crate::handoff::FocusRequest {
        account: "claude-personal".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-default");
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude-personal", |choice| {
            choice.toggle_ring("5-hour-window");
            choice.toggle_ring("weekly");
        })
    });
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-two-rings");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-two-rings-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("focus-card-menu", 1usize), cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-card-menu-open");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx)
    })
    .unwrap();
    cx.run_until_parked();

    // #230: Claude with the 5-hour window as a graph on the first card, Weekly as a gauge on the second, and the 5-hour
    // window as a bar on the third, light and dark, with the first card's menu open, and at 150%.
    let claude_choice = cx.update(|cx| crate::prefs_hub::PrefsHub::focus_choice(cx, "claude-personal"));
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude-personal", |choice| {
            choice.rings = vec!["weekly".into()];
            choice.limits = [
                Some(LimitChoice {
                    key: "5-hour-window".into(),
                    view: LimitView::Graph,
                }),
                None,
                Some(LimitChoice {
                    key: "5-hour-window".into(),
                    view: LimitView::Bar,
                }),
            ];
        })
    });
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-limits");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-limits-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("focus-card-menu", 0usize), cx)
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-limits-menu");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| zoom::set_level(1.5, cx));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "main-scroll",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "focus-claude-limits-zoom150");
    cx.update(|cx| zoom::set_level(1.0, cx));
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude-personal", |choice| *choice = claude_choice)
    });
    cx.run_until_parked();

    // #219: the gauge ring of the first account, Codex personal, at its smallest and largest sizes, the largest in
    // dark too, then with its size slider shown by keyboard focus.
    let request = crate::handoff::FocusRequest {
        account: "codex-personal".into(),
        metric: None,
    };
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    for (size, name) in [(0., "ring-size-min"), (100., "ring-size-max")] {
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_ring_size(cx, Some(size)));
        cx.run_until_parked();
        save(&mut cx, handle, &out, name);
    }
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "ring-size-max-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));

    // #223: the gauge card on the 5-hour window shows the window's own times, not a week of dates.
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set_ring_size(cx, None);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "codex-personal", |choice| {
            choice.rings = vec!["5-hour-window".into()];
        });
    });
    cx.run_until_parked();
    save(&mut cx, handle, &out, "period-five-hour");
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "codex-personal", |choice| choice.rings.clear())
    });
    cx.run_until_parked();
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_ring_size(cx, Some(60.)));
    let ring_focus = cx.update(|cx| dashboard.read(cx).ring_focus());
    cx.update_window(handle, |_, window, cx| ring_focus.focus(window, cx))
        .unwrap();
    cx.run_until_parked();
    save(&mut cx, handle, &out, "ring-size-slider");

    // The largest ring at 150% zoom in a 1440×960 window: it shrinks to fit its card.
    let ring_zoomed = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let ring_zoomed: AnyWindowHandle = ring_zoomed.into();
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set_ring_size(cx, Some(100.));
        zoom::set_level(1.5, cx);
    });
    cx.run_until_parked();
    cx.update_window(ring_zoomed, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "main-scroll",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, ring_zoomed, &out, "ring-size-max-zoom150");
    // The pointer over the card's title, far from the ring, still reveals the slider (#222).
    cx.update_window(ring_zoomed, |_, window, cx| {
        let card = window.find("focus-card-weekly-window").bounds();
        window.dispatch_event(
            MouseMoveEvent {
                position: card.origin + point(px(40.), px(30.)),
                pressed_button: None,
                modifiers: gpui_kit::Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, ring_zoomed, &out, "ring-size-hover-title-zoom150");
    cx.update(|cx| {
        zoom::set_level(1.0, cx);
        crate::prefs_hub::PrefsHub::set_ring_size(cx, None);
    });
    cx.update_window(ring_zoomed, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    // The two rings at 150% zoom in a 1440×960 window, scrolled to the cards.
    let rings_zoomed = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            let request = crate::handoff::FocusRequest {
                account: "claude-personal".into(),
                metric: None,
            };
            view.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let rings_zoomed: AnyWindowHandle = rings_zoomed.into();
    cx.update(|cx| zoom::set_level(1.5, cx));
    cx.run_until_parked();
    cx.update_window(rings_zoomed, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "main-scroll",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, rings_zoomed, &out, "focus-claude-two-rings-zoom150");
    // Three rings, the most the card shows (#221), still drawn as rings at 150%.
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude-personal", |choice| {
            choice.toggle_ring("extra-usage")
        })
    });
    cx.run_until_parked();
    save(&mut cx, rings_zoomed, &out, "focus-claude-three-rings-zoom150");
    cx.update(|cx| {
        zoom::set_level(1.0, cx);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "claude-personal", |choice| choice.rings.clear());
    });
    cx.update_window(rings_zoomed, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::History, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "history");

    // #234: the usage runway bars on their 0 to 125% scale with the limit marked at 100%, light and dark: under the
    // limit, the Pi dashboard's 112% overrun with when it runs out, a used-up window, one clipped past 125%, a tight one
    // and one too young to project. Then the 112% account's focus cards as bars, showing the same three states.
    let mut runway_view = None;
    let runway = cx
        .open_window(size(px(1440.), px(1100.)), |window, cx| {
            let source = DataSource::Live {
                history: Arc::new(std::sync::Mutex::new(codexbar_store::HistoryStore::in_memory(
                    chrono::Duration::days(30),
                ))),
                providers: Arc::new(|_| vec![Arc::new(RunwayAccounts) as Arc<dyn codexbar_providers::UsageProvider>]),
            };
            let view = cx.new(|cx| Dashboard::new(source, window, cx));
            runway_view = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let runway: AnyWindowHandle = runway.into();
    let runway_view = runway_view.unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "runway-over", |choice| {
            choice.limits = [
                Some(LimitChoice {
                    key: "weekly".into(),
                    view: LimitView::Bar,
                }),
                Some(LimitChoice {
                    key: "5-hour-window".into(),
                    view: LimitView::Bar,
                }),
                Some(LimitChoice {
                    key: "weekly-opus".into(),
                    view: LimitView::Bar,
                }),
            ];
        })
    });
    let request = crate::handoff::FocusRequest {
        account: "runway-over".into(),
        metric: None,
    };
    cx.update(|cx| runway_view.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
    cx.run_until_parked();
    save(&mut cx, runway, &out, "runway");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, runway, &out, "runway-dark");
    cx.update(|cx| {
        crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System);
        crate::prefs_hub::PrefsHub::update_focus_choice(cx, "runway-over", |choice| choice.limits = [None, None, None]);
    });
    cx.update_window(runway, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    // #215: the Spend view over the demo, light and dark, then at 150% zoom in a 1440×960 window, top and scrolled to
    // the trend.
    cx.update(|cx| dashboard.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Spend, cx)));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "spend");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
    cx.run_until_parked();
    save(&mut cx, handle, &out, "spend-dark");
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
    let mut spend_zoomed_view = None;
    let spend_zoomed = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let view = cx.new(|cx| Dashboard::new(DataSource::Demo, window, cx));
            spend_zoomed_view = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let spend_zoomed: AnyWindowHandle = spend_zoomed.into();
    let spend_zoomed_view = spend_zoomed_view.unwrap();
    cx.update(|cx| {
        spend_zoomed_view.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Spend, cx));
        zoom::set_level(1.5, cx);
    });
    cx.run_until_parked();
    save(&mut cx, spend_zoomed, &out, "spend-zoom150");
    cx.update_window(spend_zoomed, |_, window, cx| {
        window.render_frame(cx);
        window.scroll(
            "main-scroll",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
            cx,
        );
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    save(&mut cx, spend_zoomed, &out, "spend-zoom150-scrolled");
    cx.update(|cx| zoom::set_level(1.0, cx));
    cx.update_window(spend_zoomed, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

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
    // #215: nothing here reports money, so Spend shows its empty state.
    for (tab, name) in [(1usize, Some("spend-empty")), (0usize, None)] {
        cx.update_window(live, |_, window, cx| {
            window.render_frame(cx);
            window.click(("view-tab", tab), cx)
        })
        .unwrap();
        cx.run_until_parked();
        if let Some(name) = name {
            save(&mut cx, live, &out, name);
        }
    }
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
    // Switching an account on fetches it at once, which with the real factory would call Greptile with the dummy key.
    // The Settings row is captured from a window whose factory builds nothing, so no request leaves the harness.
    cx.update_window(greptile, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let mut settings_dashboard = None;
    let configured = cx
        .open_window(size(px(1440.), px(960.)), |window, cx| {
            let source = DataSource::Live {
                history: Arc::new(std::sync::Mutex::new(codexbar_store::HistoryStore::in_memory(
                    chrono::Duration::days(30),
                ))),
                providers: Arc::new(|_| Vec::new()),
            };
            let view = cx.new(|cx| Dashboard::new(source, window, cx));
            settings_dashboard = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    let configured: AnyWindowHandle = configured.into();
    let settings_view = settings_dashboard.unwrap();
    cx.run_until_parked();
    cx.update(|cx| settings_view.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
    for entry in ["0-1".to_owned(), format!("0-1-{greptile_group}")] {
        cx.update_window(configured, |_, window, cx| {
            window.render_frame(cx);
            window.within("settings-sidebar").click(entry, cx)
        })
        .unwrap();
        cx.run_until_parked();
    }
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
    save(&mut cx, configured, &out, "greptile-env-account-settings-dark");
    cx.update(|cx| {
        SettingsHub::use_environment(cx, |_| None);
        crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System);
    });

    // #233: a Copilot Business organization with no enterprise, read by its owner who has no Copilot seat, through the
    // real Copilot provider over recorded GitHub answers: this month's AI credits against 2 Business seats and its
    // AI-credit budget, then a month with no usage yet, light and dark, each focused on the organization's card.
    for (empty, name) in [(false, "copilot-business-org"), (true, "copilot-business-org-empty")] {
        let mut business_view = None;
        let business = cx
            .open_window(size(px(1440.), px(1100.)), |window, cx| {
                let source = DataSource::Live {
                    history: Arc::new(std::sync::Mutex::new(codexbar_store::HistoryStore::in_memory(
                        chrono::Duration::days(30),
                    ))),
                    providers: Arc::new(move |_| vec![business_org_provider(empty)]),
                };
                let view = cx.new(|cx| Dashboard::new(source, window, cx));
                business_view = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        let business: AnyWindowHandle = business.into();
        let business_view = business_view.unwrap();
        cx.run_until_parked();
        let request = crate::handoff::FocusRequest {
            account: "copilotorg-acme-biz".into(),
            metric: None,
        };
        cx.update(|cx| business_view.update(cx, |dashboard, cx| dashboard.focus_account(&request, cx)));
        cx.run_until_parked();
        save(&mut cx, business, &out, name);
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
        cx.run_until_parked();
        save(&mut cx, business, &out, &format!("{name}-dark"));
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
        cx.run_until_parked();
        if empty {
            cx.update_window(business, |_, window, _| window.remove_window())
                .unwrap();
            cx.run_until_parked();
            continue;
        }
        // The account's settings form: the organization set, the enterprise left blank, and the help text that no
        // longer says Enterprise only.
        let mut record = codexbar_store::settings::AccountRecord::new(
            codexbar_store::settings::names::COPILOT,
            "Business",
            codexbar_store::settings::AuthMethod::CommandLine,
        );
        record.external_id = Some("owner".into());
        record.copilot_organization = Some("acme-biz".into());
        let record_id = record.id.clone();
        cx.update(|cx| SettingsHub::update(cx, |settings| settings.upsert(record)))
            .unwrap();
        cx.update(|cx| business_view.update(cx, |dashboard, cx| dashboard.show_view(DashboardView::Settings, cx)));
        cx.run_until_parked();
        let copilot_group = crate::catalog::PROVIDERS
            .iter()
            .position(|info| info.id == codexbar_store::settings::names::COPILOT)
            .unwrap();
        for entry in ["0-1".to_owned(), format!("0-1-{copilot_group}")] {
            cx.update_window(business, |_, window, cx| {
                window.render_frame(cx);
                window.within("settings-sidebar").click(entry, cx)
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(business, |_, window, cx| {
            window.render_frame(cx);
            window.click(format!("edit-{record_id}"), cx)
        })
        .unwrap();
        cx.run_until_parked();
        save(&mut cx, business, &out, "copilot-business-settings");
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::Dark));
        cx.run_until_parked();
        save(&mut cx, business, &out, "copilot-business-settings-dark");
        cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, theme::Appearance::System));
        cx.update_window(business, |_, window, cx| {
            window.render_frame(cx);
            window.press("escape", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            SettingsHub::update(cx, |settings| {
                settings.remove(&record_id);
                Ok(())
            })
        })
        .unwrap();
        cx.update_window(business, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
    }
    let _ = std::fs::remove_dir_all(&settings);
}
