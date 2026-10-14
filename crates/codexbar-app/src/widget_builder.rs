//! Settings › Widgets (#95): the widget builder. Up to six tiles, each an account's limit or balance with a display
//! mode, the widgets' refresh choice, a preview of the automatic, one-, two- and four-tile layouts, and a reset.
//! Widgets on the Widgets board show these tiles when their Customize widget is set to My tiles.

use chrono::Utc;
use codexbar_store::widgets::{MAX_TILES, TileMode, WidgetBuilder, WidgetRefresh, WidgetSnapshot, WidgetTile};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::setting::{SettingField, SettingGroup, SettingItem, SettingPage};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, Global, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, prelude::FluentBuilder as _, px,
    relative,
};

use crate::prefs_hub::PrefsHub;
use crate::widgets::cards::{self, Config, Focus, Size, TileView, Tiles};

/// The dashboard's latest widget snapshot, for the preview and the list of limits to add. Kept in demo mode too.
pub struct WidgetFeed(pub WidgetSnapshot);

impl Global for WidgetFeed {}

/// The layout the preview shows.
#[derive(Default)]
struct PreviewLayout(Tiles);

impl Global for PreviewLayout {}

fn preview_layout(cx: &App) -> Tiles {
    cx.try_global::<PreviewLayout>()
        .map(|layout| layout.0)
        .unwrap_or_default()
}

/// The dropdown values beside the display modes.
const MOVE_UP: &str = "up";
const REMOVE: &str = "remove";

fn tile_label(snapshot: Option<&WidgetSnapshot>, tile: &WidgetTile) -> String {
    let account = snapshot.and_then(|snapshot| snapshot.accounts.iter().find(|account| account.id == tile.account));
    match account {
        None => "Removed account".to_owned(),
        Some(account) => {
            let metric = account.metrics.iter().find(|metric| metric.key == tile.metric);
            format!(
                "{} · {}",
                account.name,
                metric.map_or(tile.metric.as_str(), |metric| metric.label.as_str())
            )
        }
    }
}

fn tile_item(ix: usize, tile: &WidgetTile, snapshot: Option<&WidgetSnapshot>) -> SettingItem {
    let mut options: Vec<(SharedString, SharedString)> = TileMode::ALL
        .iter()
        .map(|mode| (SharedString::from(mode.key()), SharedString::from(mode.label())))
        .collect();
    if ix > 0 {
        options.push((MOVE_UP.into(), "Move up".into()));
    }
    options.push((REMOVE.into(), "Remove tile".into()));
    SettingItem::new(
        SharedString::from(tile_label(snapshot, tile)),
        SettingField::dropdown(
            options,
            move |cx: &App| {
                let builder = PrefsHub::widget_builder(cx);
                SharedString::from(
                    builder
                        .tiles
                        .get(ix)
                        .map_or(TileMode::Automatic, |tile| tile.mode)
                        .key(),
                )
            },
            move |value: SharedString, cx: &mut App| {
                PrefsHub::update_widget_builder(cx, |builder| {
                    if ix >= builder.tiles.len() {
                        return;
                    }
                    match value.as_ref() {
                        MOVE_UP if ix > 0 => builder.tiles.swap(ix, ix - 1),
                        REMOVE => {
                            builder.tiles.remove(ix);
                        }
                        key => {
                            if let Some(mode) = TileMode::from_key(key) {
                                builder.tiles[ix].mode = mode;
                            }
                        }
                    }
                });
            },
        ),
    )
    .description(format!("Tile {} · how it shows", ix + 1))
}

/// Every account limit or balance the snapshot has that isn't a tile yet, as `account|metric`.
fn addable(snapshot: Option<&WidgetSnapshot>, builder: &WidgetBuilder) -> Vec<(SharedString, SharedString)> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    snapshot
        .accounts
        .iter()
        .flat_map(|account| {
            account.metrics.iter().filter_map(move |metric| {
                let taken = builder
                    .tiles
                    .iter()
                    .any(|tile| tile.account == account.id && tile.metric == metric.key);
                (!taken).then(|| {
                    (
                        SharedString::from(format!("{}|{}", account.id, metric.key)),
                        SharedString::from(format!("{} · {}", account.name, metric.label)),
                    )
                })
            })
        })
        .collect()
}

fn add_item(snapshot: Option<&WidgetSnapshot>, builder: &WidgetBuilder) -> SettingItem {
    let mut options = vec![(SharedString::from(""), SharedString::from("Choose a limit or balance…"))];
    if !builder.is_full() {
        options.extend(addable(snapshot, builder));
    }
    let description = if builder.is_full() {
        "Six tiles is the most. Remove one to add another."
    } else if options.len() == 1 {
        "Limits and balances appear here once CodexBar has fetched usage."
    } else {
        "Adds the limit or balance as the next tile."
    };
    SettingItem::new(
        "Add a tile",
        SettingField::dropdown(
            options,
            |_: &App| SharedString::from(""),
            |value: SharedString, cx: &mut App| {
                let Some((account, metric)) = value.rsplit_once('|') else {
                    return;
                };
                let tile = WidgetTile {
                    account: account.to_owned(),
                    metric: metric.to_owned(),
                    mode: TileMode::Automatic,
                };
                PrefsHub::update_widget_builder(cx, |builder| {
                    builder.add(tile);
                });
            },
        ),
    )
    .description(description)
}

fn refresh_item() -> SettingItem {
    let options = WidgetRefresh::ALL
        .iter()
        .map(|refresh| (SharedString::from(refresh.key()), SharedString::from(refresh.label())))
        .collect();
    SettingItem::new(
        "Widget refresh",
        SettingField::dropdown(
            options,
            |cx: &App| SharedString::from(PrefsHub::widget_builder(cx).refresh.key()),
            |value: SharedString, cx: &mut App| {
                if let Some(refresh) = WidgetRefresh::from_key(&value) {
                    PrefsHub::update_widget_builder(cx, |builder| builder.refresh = refresh);
                }
            },
        ),
    )
    .description("How often widgets redraw. Their usage comes from CodexBar's own refreshes.")
}

fn status_color(status: Option<&str>, cx: &App) -> Hsla {
    match status {
        Some("At risk" | "Limit soon") => cx.theme().danger,
        Some(_) => cx.theme().warning,
        None => cx.theme().success,
    }
}

fn preview_tile(ix: usize, view: &TileView, compact: bool, cx: &App) -> impl IntoElement + use<> {
    let color = status_color(view.status.as_deref(), cx);
    let muted = cx.theme().muted_foreground;
    let track = cx.theme().muted;
    let label = [
        Some(view.title.clone()),
        view.big.clone(),
        view.line.clone(),
        view.note.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(". ");
    v_flex()
        .id(("preview-tile", ix))
        .role(Role::Group)
        .test_support()
        .aria_label(SharedString::from(label))
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .child(div().text_sm().font_semibold().truncate().child(view.title.clone()))
        .when_some(view.big.clone(), |this, big| {
            this.child(
                div()
                    .when(compact, |this| this.text_lg())
                    .when(!compact, |this| this.text_2xl())
                    .font_bold()
                    .when(view.mode == TileMode::Status, |this| this.text_color(color))
                    .child(big),
            )
        })
        .when_some(
            view.line.clone().filter(|_| !compact || view.big.is_none()),
            |this, line| this.child(div().text_xs().child(line)),
        )
        .when_some(view.percent, |this, percent| {
            this.child(
                div().h(px(6.)).w_full().rounded(px(3.)).bg(track).child(
                    div()
                        .h_full()
                        .rounded(px(3.))
                        .bg(color)
                        .w(relative((percent / 100.0) as f32)),
                ),
            )
        })
        .when_some(view.note.clone().filter(|_| !compact), |this, note| {
            this.child(div().text_xs().text_color(muted).child(note))
        })
}

fn preview_widget(size: Size, tiles: Tiles, snapshot: &WidgetSnapshot, cx: &App) -> impl IntoElement + use<> {
    let config = Config {
        focus: Focus::Custom,
        tiles,
    };
    let views = cards::tile_views(snapshot, &config, size, Utc::now()).unwrap_or_default();
    let (width, name, ix) = match size {
        Size::Small => (px(170.), "Small", 0usize),
        Size::Medium => (px(340.), "Medium", 1),
        Size::Large => (px(340.), "Large", 2),
    };
    let compact = size == Size::Small && views.len() > 1;
    let two_columns = views.len() >= 3 || (views.len() == 2 && size == Size::Large);
    let mut rows = Vec::new();
    let tiles: Vec<_> = views
        .iter()
        .enumerate()
        .map(|(n, view)| preview_tile(ix * 10 + n, view, compact, cx).into_any_element())
        .collect();
    let mut tiles = tiles.into_iter();
    loop {
        let row: Vec<_> = tiles.by_ref().take(if two_columns { 2 } else { 1 }).collect();
        if row.is_empty() {
            break;
        }
        rows.push(h_flex().gap_3().items_start().children(row));
    }
    v_flex()
        .id(("widget-preview", ix))
        .role(Role::Group)
        .test_support()
        .aria_label(SharedString::from(format!(
            "{name} widget preview, {} tiles",
            views.len()
        )))
        .w(width)
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{name} · CodexBar")),
        )
        .when(views.is_empty(), |this| {
            this.child(div().text_sm().child("Add tiles above to see them here."))
        })
        .children(rows)
}

fn preview_item() -> SettingItem {
    SettingItem::render(|_, _, cx| {
        let selected = preview_layout(cx);
        let buttons = Tiles::ALL.iter().enumerate().map(move |(ix, tiles)| {
            let tiles = *tiles;
            let label = match tiles {
                Tiles::Automatic => "Automatic",
                Tiles::One => "One tile",
                Tiles::Two => "Two tiles",
                Tiles::Four => "Four tiles",
            };
            Button::new(("preview-layout", ix))
                .small()
                .label(label)
                .when(tiles == selected, |button| button.primary())
                .when(tiles != selected, |button| button.outline())
                .on_click(move |_, _, cx| {
                    cx.set_global(PreviewLayout(tiles));
                    cx.refresh_windows();
                })
        });
        let snapshot = cx.try_global::<WidgetFeed>().map(|feed| feed.0.clone());
        let body = match snapshot {
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("The preview appears once CodexBar has fetched usage.")
                .into_any_element(),
            Some(snapshot) => {
                // Automatic depends on the widget's size, so it shows all three.
                let sizes: Vec<Size> = if selected == Tiles::Automatic {
                    vec![Size::Small, Size::Medium, Size::Large]
                } else {
                    vec![Size::Medium]
                };
                h_flex()
                    .gap_3()
                    .items_start()
                    .flex_wrap()
                    .children(
                        sizes
                            .into_iter()
                            .map(|size| preview_widget(size, selected, &snapshot, cx)),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(div().font_semibold().child("Preview"))
            .child(h_flex().gap_2().flex_wrap().children(buttons))
            .child(body)
    })
}

fn reset_item() -> SettingItem {
    SettingItem::new(
        "Reset widget settings",
        SettingField::render(|_, _, _| {
            Button::new("reset-widgets")
                .small()
                .outline()
                .label("Reset…")
                .on_click(|_, window, cx| confirm_reset(window, cx))
        }),
    )
    .description("Removes every tile and sets Widget refresh back to When CodexBar refreshes.")
}

fn confirm_reset(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _, _| {
        alert
            .title("Reset widget settings?")
            .description("Every tile is removed and Widget refresh goes back to When CodexBar refreshes. Widgets set to My tiles show nothing until you add tiles again.")
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Reset")
                    .ok_variant(ButtonVariant::Danger)
                    .show_cancel(true),
            )
            .on_ok(|_, _, cx| {
                PrefsHub::update_widget_builder(cx, |builder| *builder = WidgetBuilder::default());
                true
            })
    });
}

#[cfg(windows)]
pub fn widgets_page(cx: &App) -> SettingPage {
    let builder = PrefsHub::widget_builder(cx);
    let snapshot = cx.try_global::<WidgetFeed>().map(|feed| &feed.0);
    // Windows widgets come only from packaged apps (#94), on Windows 11, and for a self-signed package only with
    // Developer Mode on. The builder itself works everywhere.
    use crate::widgets::BoardSupport;
    let (title, body): (&'static str, &'static str) = if crate::package::installed().is_none() {
        (
            "Install the package to use widgets",
            "Windows widgets work only when CodexBar is installed as a package: run .\\package.ps1 -Trust -Install \
             (see docs/PACKAGING.md). The builder below works either way.",
        )
    } else {
        match BoardSupport::current() {
            BoardSupport::NeedsWindows11 => (
                "Widgets need Windows 11",
                "The Widgets board, the only place Windows shows widgets, is part of Windows 11, so CodexBar's widget \
                 can't be added on this PC.",
            ),
            BoardSupport::NeedsDeveloperMode => (
                "Turn on Developer Mode to see the widget",
                "Windows lists widgets from a self-signed package only with Developer Mode on: Settings › System › \
                 For developers › Developer Mode. Then open the Widgets board (Windows key + W) and choose Add widgets.",
            ),
            BoardSupport::Ready => (
                "Add a CodexBar widget",
                "Open the Widgets board (Windows key + W), choose Add widgets and pick CodexBar usage. In Customize \
                 widget, choose My tiles to show the tiles below, or all accounts, one provider or one group.",
            ),
        }
    };
    let info = crate::settings_view::info_item(title, body);
    let mut tiles = SettingGroup::new()
        .title("My tiles")
        .description(format!("Up to {MAX_TILES} account limits or balances, in order."));
    for (ix, tile) in builder.tiles.iter().enumerate() {
        tiles = tiles.item(tile_item(ix, tile, snapshot));
    }
    tiles = tiles.item(add_item(snapshot, &builder));
    SettingPage::new("Widgets")
        .icon(IconName::LayoutDashboard)
        .group(SettingGroup::new().title("Windows widgets").item(info))
        .group(tiles)
        .group(SettingGroup::new().title("Refresh").item(refresh_item()))
        .group(SettingGroup::new().title("Preview").item(preview_item()))
        .group(SettingGroup::new().title("Reset").item(reset_item()))
}
