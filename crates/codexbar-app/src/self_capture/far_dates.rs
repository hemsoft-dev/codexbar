//! Provider dates half an hour before the last one chrono holds (#312), in the real Spend view and focus cards, shown
//! in a fixed zone east of UTC without changing the machine's, and once in the machine's own zone.
use super::{save, settle};
use crate::dashboard::{Dashboard, DashboardView};
use crate::theme::Appearance;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, AsyncApp, Entity};
use std::path::Path;

pub(super) async fn run(
    cx: &mut AsyncApp,
    handle: AnyWindowHandle,
    dashboard: &Entity<Dashboard>,
    dir: &Path,
) -> Vec<String> {
    let tokyo = chrono::FixedOffset::east_opt(9 * 3600);
    cx.update(|cx| crate::prefs_hub::PrefsHub::set_appearance(cx, Appearance::Light));
    let mut saved = Vec::new();
    for (zone, sibling, views, name) in [
        (
            tokyo,
            true,
            &[DashboardView::Spend, DashboardView::Usage][..],
            "utc0900-with-ordinary",
        ),
        (tokyo, false, &[DashboardView::Spend][..], "utc0900-alone"),
        (None, true, &[DashboardView::Spend][..], "local-with-ordinary"),
    ] {
        for view in views {
            cx.update(|cx| {
                dashboard.update(cx, |d, cx| {
                    d.capture_far_dates(zone, sibling, cx);
                    d.show_view(*view, cx);
                })
            });
            settle(cx).await;
            let far = cx
                .update(|cx| {
                    cx.update_window(handle, |_, window, cx| {
                        window.render_frame(cx);
                        if *view == DashboardView::Usage {
                            assert!(window.try_find("focus-bar-card-1").is_some(), "{name}");
                            return String::new();
                        }
                        // The view orders balances itself; the far one is found by its label.
                        let far = (0..)
                            .map_while(|ix| {
                                window.try_find(gpui_kit::ElementId::Name(format!("balance-row-{ix}").into()))
                            })
                            .filter_map(|found| found.label().map(str::to_owned))
                            .find(|label| label.contains("Far run-out"))
                            .expect("the far balance row");
                        let first = window
                            .try_find("spend-runs-out-first")
                            .and_then(|found| found.label().map(str::to_owned));
                        assert_eq!(first.is_some(), sibling, "{name}: {first:?}");
                        far
                    })
                })
                .expect("the far-dates fixture must reach the actual views");
            if zone.is_some() && *view == DashboardView::Spend {
                assert!(far.ends_with("No spending rate known yet"), "{name}: {far}");
            }
            let view = if *view == DashboardView::Usage {
                "focus"
            } else {
                "spend"
            };
            let name = format!("far-dates-{name}-{view}");
            assert!(save(cx, handle, dir, &name));
            saved.push(name);
        }
    }
    cx.update(|cx| crate::locale::capture_zone(None, cx));
    saved
}
