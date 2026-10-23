//! Chart cards for the focused account, in the gpui-kit Chart gallery's anatomy:
//! title, period, chart, a takeaway sentence, and a quiet caption.

use chrono::{DateTime, Local, Utc};
use codexbar_core::{AccountSnapshot, Metric, Severity, format};
use codexbar_store::focus::{ActivityView, FocusChoice, LimitChoice, LimitView, MAX_RINGS, UsageView};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Background, ElementId, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    linear_color_stop, linear_gradient, prelude::FluentBuilder as _, rems,
};

use crate::history_view::ValueKind;
use gpui_kit::{InteractiveElement as _, TestSupportExt as _};

/// Where a share axis ends (#200): a quarter above 100%, so a smooth curve's swing past a limit and the marker on a
/// point at 100% stay inside the plot. The ticks land on 0, 25, 50, 75 and 100%, and the headroom tick at the top
/// draws its gridline without a label.
const SHARE_TOP: f64 = 1.25;
const SHARE_TICKS: usize = 6;
/// The least height a card's chart slot keeps, in rems so it follows the zoom (#200); below it the card grows and
/// the dashboard scrolls rather than the chart drawing past its box.
const CHART_MIN_HEIGHT_REMS: f32 = 11.;

/// A trend tick's label: the share ticks for a share trend (#200), the headline's own unit for a count or money.
fn trend_tick(kind: ValueKind, counted: bool, value: f64) -> String {
    if counted { kind.value(value) } else { share_tick(value) }
}

/// A share tick's label: a percentage up to 100%, nothing for the headroom above it.
fn share_tick(value: f64) -> String {
    // The chart reads each tick's value back from its pixel row in f32, so 100% can arrive a hair above 1.0.
    if value > 1.0 + 1e-3 {
        String::new()
    } else {
        format!("{:.0}%", value * 100.0)
    }
}
use crate::smooth_plot::{Series, SmoothPlot};
use crate::status::severity_color;

/// One card. Built fresh each frame; the charts own their own hover state by id.
struct Card {
    /// The id of the card and, suffixed, of its parts, so a headless test can check that its content stays inside
    /// it (#200). Derived from the title unless set.
    id: SharedString,
    title: SharedString,
    period: SharedString,
    legend: Vec<(Hsla, SharedString)>,
    chart: AnyElement,
    takeaway: SharedString,
    alert: Option<Hsla>,
    caption: SharedString,
    /// What the card can show instead, at the header's end (#218).
    menu: Option<AnyElement>,
    /// A hover group spanning the whole card, so hovering anywhere on it reveals what listens to the group (#219).
    hover_group: Option<&'static str>,
}

impl Card {
    fn new(title: impl Into<SharedString>, period: impl Into<SharedString>, chart: impl IntoElement) -> Self {
        let title = title.into();
        Self {
            id: format!("focus-card-{}", title.to_lowercase().replace(' ', "-")).into(),
            title,
            period: period.into(),
            legend: Vec::new(),
            chart: chart.into_any_element(),
            takeaway: SharedString::default(),
            alert: None,
            caption: SharedString::default(),
            menu: None,
            hover_group: None,
        }
    }

    fn hover_group(mut self, group: &'static str) -> Self {
        self.hover_group = Some(group);
        self
    }

    /// Keeps the card's id unique when it shows in `slot` rather than in the slot it was made for, `home` (#230):
    /// a chosen limit can put the same kind of card on two slots.
    fn in_slot(mut self, slot: usize, home: usize) -> Self {
        if slot != home {
            self.id = format!("{}-{slot}", self.id).into();
        }
        self
    }

    fn menu(mut self, menu: AnyElement) -> Self {
        self.menu = Some(menu);
        self
    }

    fn legend(mut self, color: Hsla, label: impl Into<SharedString>) -> Self {
        self.legend.push((color, label.into()));
        self
    }

    fn takeaway(mut self, text: impl Into<SharedString>) -> Self {
        self.takeaway = text.into();
        self
    }

    /// Marks the takeaway as a warning, with an icon so the state is not carried by color alone.
    fn alert(mut self, color: Hsla) -> Self {
        self.alert = Some(color);
        self
    }

    fn caption(mut self, text: impl Into<SharedString>) -> Self {
        self.caption = text.into();
        self
    }

    fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = id.into();
        self
    }

    fn render(self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let part = |name: &str| ElementId::Name(format!("{}-{name}", self.id).into());
        let legend_id = part("legend");
        let takeaway_id = part("takeaway");
        v_flex()
            .id(ElementId::Name(self.id.clone()))
            .test_support()
            .when_some(self.hover_group, |this, group| this.group(group))
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_4()
            .gap_1()
            .bg(cx.theme().group_box)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            // The legend sits beside the title when there is room, otherwise under it (#200): zoomed in, a wide
            // legend used to run past the card's edge. The menu keeps the top right corner (#230), so the header
            // leaves room for it and a long title or period wraps instead of pushing it onto a line of its own.
            .child(
                h_flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_x_3()
                    .gap_y_1()
                    .when(self.menu.is_some(), |this| this.pr(rems(1.75)))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child(self.title))
                            .child(div().text_sm().text_color(muted).child(self.period)),
                    )
                    .when(!self.legend.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .items_start()
                                .when(!self.legend.is_empty(), |this| {
                                    this.child(
                                        h_flex()
                                            .id(legend_id)
                                            .test_support()
                                            .flex_wrap()
                                            .min_w_0()
                                            .justify_end()
                                            .gap_3()
                                            .text_xs()
                                            .text_color(muted)
                                            .children(self.legend.into_iter().map(|(color, label)| {
                                                h_flex()
                                                    .gap_1p5()
                                                    .items_center()
                                                    .child(div().size_2().rounded_full().bg(color))
                                                    .child(label)
                                            })),
                                    )
                                }),
                        )
                    }),
            )
            .children(
                self.menu
                    .map(|menu| div().absolute().top(rems(1.)).right(rems(1.)).child(menu)),
            )
            // Never squeezed below what a chart needs (#200): the card grows and the dashboard scrolls instead.
            .child(
                div()
                    .flex_1()
                    .min_h(rems(CHART_MIN_HEIGHT_REMS))
                    .py_3()
                    .child(self.chart),
            )
            // The takeaway wraps instead of running past the card (#200).
            .child(
                h_flex()
                    .id(takeaway_id)
                    .test_support()
                    .gap_1p5()
                    .items_start()
                    .text_sm()
                    .font_semibold()
                    .when_some(self.alert, |this, color| {
                        this.child(Icon::new(IconName::TriangleAlert).small().text_color(color))
                    })
                    .child(div().flex_1().min_w_0().child(self.takeaway)),
            )
            .child(div().text_sm().text_color(muted).child(self.caption))
    }
}

/// The color the drawn copy of a smooth chart gives its series, so only the smooth curves over it show (#229).
const CLEAR: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.,
};

fn area_fill(color: Hsla) -> Background {
    linear_gradient(
        0.,
        linear_color_stop(color.opacity(0.4), 1.),
        linear_color_stop(color.opacity(0.), 0.),
    )
}

/// Every metric the account reports, compactly (#82): the cards chart two of them, and a provider like Claude can
/// return more (model-scoped weekly limits, usage credits, a credit balance). Each says its value, reset and projection.
pub fn metric_list(account: &AccountSnapshot, now: DateTime<Utc>, cx: &App) -> Option<AnyElement> {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    if account.metrics().len() < 2 {
        return None;
    }
    let muted = cx.theme().muted_foreground;
    let entries = account.metrics().iter().enumerate().map(|(ix, metric)| {
        let mut detail = Vec::new();
        if let Some(at) = metric.resets_at().filter(|at| *at > now) {
            detail.push(format!("resets in {}", format::countdown(at - now)));
        }
        // The share at the reset and when the limit runs out (#234), as the bars say it.
        if let Some(outlook) = crate::usage_bar::outlook(metric, account, now) {
            detail.push(outlook);
        }
        // What is left under a spend limit, derived from spend and limit; never presented as a balance.
        if let Some(headroom) = metric.headroom() {
            detail.push(format!("{} left to the limit", headroom.display()));
        }
        if let Some(spent) = credit_spend(metric) {
            detail.push(spent);
        }
        let detail = detail.join(" · ");
        let spoken = if detail.is_empty() {
            format!("{}: {}", metric.label(), metric.used_display())
        } else {
            format!("{}: {}, {detail}", metric.label(), metric.used_display())
        };
        let severity = codexbar_core::assess(metric, now).severity();
        v_flex()
            .id(("account-metric", ix))
            .role(Role::Note)
            .test_support()
            .aria_label(spoken)
            .min_w(gpui_kit::rems(9.))
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .child(div().text_xs().text_color(muted).child(metric.label().to_owned()))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .when(severity >= Severity::Watch, |this| {
                        this.child(div().size_2().rounded_full().bg(severity_color(severity, cx)))
                    })
                    .child(div().text_sm().font_semibold().child(metric.used_display()))
                    .children(crate::sus::metric(metric, now, &format!("focus-metric-{ix}-sus"), cx)),
            )
            .when(!detail.is_empty(), |this| {
                this.child(div().text_xs().text_color(muted).child(detail))
            })
    });
    Some(
        h_flex()
            .id("account-metrics")
            .flex_wrap()
            .gap_2()
            .children(entries)
            .into_any_element(),
    )
}

/// The three cards for `account`, left to right.
pub fn focus_cards(
    account: &AccountSnapshot,
    now: DateTime<Utc>,
    ring: &crate::ring_size::RingControl,
    cx: &App,
) -> Vec<AnyElement> {
    let id = account.id().as_str();
    let choice = crate::prefs_hub::PrefsHub::focus_choice(cx, id);
    let menus = card_menus(account, &choice, cx);
    menus
        .into_iter()
        .enumerate()
        .map(|(slot, menu)| {
            // A limit chosen for the card (#230) takes it over while the account reports it.
            let card = match limit_in_effect(account, &choice.limits[slot]) {
                Some((metric, view)) => limit_card(account, metric, view, now, slot, ring, cx),
                None => match slot {
                    0 => usage_card(account, now, usage_in_effect(account, choice.usage), cx),
                    1 => share_card(account, now, &choice.rings, ring, 1, cx),
                    _ => activity_card(account, activity_in_effect(account, choice.activity), cx),
                },
            };
            card.menu(menu).render(cx).into_any_element()
        })
        .collect()
}

/// The limit chosen for a card and how to draw it, while the account still reports that limit (#230).
fn limit_in_effect<'a>(account: &'a AccountSnapshot, chosen: &Option<LimitChoice>) -> Option<(&'a Metric, LimitView)> {
    let chosen = chosen.as_ref()?;
    limits(account)
        .find(|metric| metric.key() == chosen.key)
        .map(|metric| (metric, chosen.view))
}

/// A card showing one chosen limit (#230): over time, as a progress bar, or as a gauge.
fn limit_card(
    account: &AccountSnapshot,
    metric: &Metric,
    view: LimitView,
    now: DateTime<Utc>,
    slot: usize,
    ring: &crate::ring_size::RingControl,
    cx: &App,
) -> Card {
    match view {
        LimitView::Gauge => share_card(account, now, &[metric.key()], ring, slot, cx),
        LimitView::Bar => bar_card(account, metric, now, slot, cx),
        LimitView::Graph => graph_card(&limit_graph(account, metric, now), now, slot, cx),
    }
}

/// What a card graphs for one chosen limit (#230): its own window curve or daily trend, in its own unit. A capped
/// spend's history is money, as History stores it (#232), so it is a counted series, not a share.
fn limit_graph<'a>(account: &'a AccountSnapshot, metric: &'a Metric, now: DateTime<Utc>) -> Graph<'a> {
    // The primary limit's history is the account's own; every other limit keeps its own (#230).
    let primary = account.primary().is_some_and(|primary| primary.key() == metric.key());
    let series = account.detail().limit_series(&metric.key());
    let (curve, trend) = if primary {
        (account.detail().window_curve(), account.trend())
    } else {
        (
            series.and_then(|series| series.curve.as_ref()),
            series.map_or(&[][..], |series| series.trend.as_slice()),
        )
    };
    Graph {
        metric: Some(metric),
        curve: comparable(curve),
        trend,
        kind: ValueKind::of(metric),
        counted: counted_metric(metric),
        title: Some(window_title(metric)),
        severity: codexbar_core::assess(metric, now).severity(),
    }
}

/// A window curve the card can draw: one with both the current and the previous window (#232). In a limit's first
/// window there is no previous one yet, and pairing the two would leave no points, so the card shows the trend.
fn comparable(curve: Option<&codexbar_core::WindowCurve>) -> Option<&codexbar_core::WindowCurve> {
    curve.filter(|curve| !curve.current().is_empty() && !curve.previous().is_empty())
}

/// A limit's title on a card: its label, with "window" added when it doesn't say so.
fn window_title(metric: &Metric) -> String {
    if metric.label().to_lowercase().contains("window") {
        metric.label().to_owned()
    } else {
        format!("{} window", metric.label())
    }
}

/// One limit as a large progress bar (#230): the share used, the bar with its projection as the table draws it, the
/// window's period, pace and reset.
fn bar_card(account: &AccountSnapshot, metric: &Metric, now: DateTime<Utc>, slot: usize, cx: &App) -> Card {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    let used = metric.used_fraction().unwrap_or_default();
    // Calm when the account's credits carry it (#150), as the table's bar is.
    let projection = crate::usage_bar::Projection::of_account(metric, used, account, now);
    // The share at the reset and when the limit runs out (#234), as the table's "At reset" column says it.
    let pace = crate::usage_bar::outlook(metric, account, now);
    let spoken = format!("{}: {}", metric.label(), projection.label);
    let muted = cx.theme().muted_foreground;
    let bar_height = crate::zoom::scaled(14., cx);
    let body = v_flex()
        .id(SharedString::from(format!("focus-bar-{slot}")))
        .role(Role::Note)
        .test_support()
        .aria_label(spoken)
        .size_full()
        .justify_center()
        .gap_3()
        .child(
            h_flex()
                .items_baseline()
                .gap_1()
                .child(div().text_3xl().font_semibold().child(format!("{:.0}", used * 100.0)))
                .child(div().text_sm().text_color(muted).child("% used")),
        )
        // The bar fills whatever box it is given, so it gets one of its own: twice its height, for the projection's
        // tick above and below it.
        .child(
            div()
                .w_full()
                .h(bar_height * 2.)
                .flex_none()
                .child(crate::usage_bar::usage_bar(
                    &format!("focus-bar-{slot}-bar"),
                    &projection,
                    cx.theme().chart_1,
                    bar_height,
                    metric.label().to_owned(),
                    cx,
                )),
        )
        .children(crate::sus::metric(metric, now, &format!("focus-bar-{slot}-sus"), cx))
        .children(pace.map(|pace| div().text_sm().text_color(muted).child(pace)));
    let card = Card::new(window_title(metric), window_period(metric, now), body)
        .id(format!("focus-bar-card-{slot}"))
        .takeaway(pool_takeaway(1.0 - used))
        .caption(reset_caption(metric, now));
    // A window the account's credits carry (#150) raises no alert, as the account's own assessment doesn't.
    let severity = codexbar_core::assess(metric, now).severity();
    if severity >= Severity::AtRisk && !account.credits_cover(metric) {
        card.alert(severity_color(severity, cx))
    } else {
        card
    }
}

/// The words in a ring's opening (#234): the forecast's share at the reset, short enough for the ring, or what the
/// bars say when there is none ("covered by credits", "limit reached"). The whole forecast is in its label and hover.
fn ring_pace(metric: &Metric, account: &AccountSnapshot, now: DateTime<Utc>) -> Option<String> {
    if account.credits_cover(metric) {
        return crate::usage_bar::outlook(metric, account, now);
    }
    match crate::usage_bar::Forecast::of(metric, now) {
        Some(forecast) => Some(format!("{} at reset", forecast.share())),
        None => crate::usage_bar::outlook(metric, account, now),
    }
}

/// One entry in a card's menu: what it shows and what choosing it does.
#[derive(Clone)]
struct MenuEntry {
    label: String,
    checked: bool,
    /// Shown but not pickable: another ring once the gauge card has its most (#221).
    disabled: bool,
    /// Starts a new group in the menu: the limits a card can show (#230).
    separator_before: bool,
    action: MenuAction,
}

#[derive(Clone)]
enum MenuAction {
    Usage(Option<UsageView>),
    /// `None` clears the rings back to automatic; a key adds or removes that limit's ring.
    Ring(Option<String>),
    /// Back to the ring's default size (#219).
    DefaultRingSize,
    Activity(Option<ActivityView>),
    /// A limit for this card in a view (#230).
    Limit(LimitChoice),
}

/// What each card can show for `account`, from what it reports (#218). The usage card offers the window curve when
/// the provider sends one and the trend once history has two days; the gauge card offers every metric with a share
/// of a limit; the activity card offers requests by hour when the provider counts them.
fn menu_entries(account: &AccountSnapshot, choice: &FocusChoice, sized: bool) -> [Vec<MenuEntry>; 3] {
    let entry = |label: &str, checked: bool, action: MenuAction| MenuEntry {
        label: label.to_owned(),
        checked,
        disabled: false,
        separator_before: false,
        action,
    };
    // Each card checks what it shows, so a saved view the account can no longer draw reads as Automatic (#221).
    let usage_view = usage_in_effect(account, choice.usage);
    let activity_view = activity_in_effect(account, choice.activity);
    let mut usage = vec![entry("Automatic", usage_view.is_none(), MenuAction::Usage(None))];
    if comparable(account.detail().window_curve()).is_some() {
        usage.push(entry(
            "This window and the last",
            usage_view == Some(UsageView::Window),
            MenuAction::Usage(Some(UsageView::Window)),
        ));
    }
    if account.trend().len() >= 2 {
        usage.push(entry(
            "Last 14 days",
            usage_view == Some(UsageView::Trend),
            MenuAction::Usage(Some(UsageView::Trend)),
        ));
    }
    // Automatic is what the card shows when no chosen limit is reported any more, too (#221).
    let shown = chosen_rings(account, &choice.rings).len();
    let mut rings = vec![entry("Automatic", shown == 0, MenuAction::Ring(None))];
    for metric in limits(account) {
        let key = metric.key();
        let checked = choice.rings.contains(&key);
        rings.push(MenuEntry {
            disabled: !checked && shown >= MAX_RINGS,
            ..entry(metric.label(), checked, MenuAction::Ring(Some(key)))
        });
    }
    // A resized ring can go back to its default size from here (#219).
    if sized {
        rings.push(entry("Default ring size", false, MenuAction::DefaultRingSize));
    }
    let mut activity = vec![entry("Automatic", activity_view.is_none(), MenuAction::Activity(None))];
    if !account.detail().requests_by_hour().is_empty() {
        activity.push(entry(
            "Requests by hour",
            activity_view == Some(ActivityView::Hours),
            MenuAction::Activity(Some(ActivityView::Hours)),
        ));
    }
    activity.push(entry(
        "Usage by day",
        activity_view == Some(ActivityView::Days),
        MenuAction::Activity(Some(ActivityView::Days)),
    ));
    let mut cards = [usage, rings, activity];
    // Any limit as a graph, a bar or a gauge on any card (#230); the gauge card's own limits are its rings. While a
    // limit is chosen for a card it is what the card shows, so nothing above it reads as checked.
    let views = [
        (LimitView::Graph, "as a graph"),
        (LimitView::Bar, "as a bar"),
        (LimitView::Gauge, "as a gauge"),
    ];
    for (slot, entries) in cards.iter_mut().enumerate() {
        let here = limit_in_effect(account, &choice.limits[slot]).map(|(metric, view)| (metric.key(), view));
        if here.is_some() {
            entries.iter_mut().for_each(|entry| entry.checked = false);
        }
        let mut first = true;
        for metric in limits(account) {
            for (view, words) in views {
                if slot == 1 && view == LimitView::Gauge {
                    continue;
                }
                let key = metric.key();
                let checked = here.as_ref() == Some(&(key.clone(), view));
                entries.push(MenuEntry {
                    separator_before: std::mem::take(&mut first),
                    ..entry(
                        &format!("{} {words}", metric.label()),
                        checked,
                        MenuAction::Limit(LimitChoice { key, view }),
                    )
                });
            }
        }
    }
    cards
}

/// The usage view a card shows: the chosen one while the account can draw it, otherwise automatic (#221), so the
/// card and its menu agree on what is on screen.
fn usage_in_effect(account: &AccountSnapshot, chosen: Option<UsageView>) -> Option<UsageView> {
    chosen.filter(|view| match view {
        UsageView::Window => comparable(account.detail().window_curve()).is_some(),
        UsageView::Trend => account.trend().len() >= 2,
    })
}

/// The activity view a card shows: requests by hour only while the provider counts them (#221).
fn activity_in_effect(account: &AccountSnapshot, chosen: Option<ActivityView>) -> Option<ActivityView> {
    chosen.filter(|view| match view {
        ActivityView::Hours => !account.detail().requests_by_hour().is_empty(),
        ActivityView::Days => true,
    })
}

/// The metrics a ring can show: those with a share of a limit, in the provider's order.
fn limits(account: &AccountSnapshot) -> impl Iterator<Item = &Metric> {
    account
        .metrics()
        .iter()
        .filter(|metric| metric.used_fraction().is_some())
}

/// Each card's menu button. A choice saves at once and the cards redraw; the gauge's limits toggle, so the menu
/// builds up several rings one pick at a time.
fn card_menus(account: &AccountSnapshot, choice: &FocusChoice, cx: &App) -> [AnyElement; 3] {
    use gpui_kit::component::Disableable as _;
    use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
    let read_only = crate::prefs_hub::PrefsHub::is_read_only(cx);
    let names = ["usage chart", "gauge", "activity chart"];
    let sized = crate::prefs_hub::PrefsHub::ring_size(cx).is_some();
    let entries = menu_entries(account, choice, sized);
    // The limits the account reports now, so a pick drops chosen ones it no longer reports (#221).
    let available: Vec<String> = limits(account).map(Metric::key).collect();
    let mut menus = entries.into_iter().enumerate().map(|(ix, entries)| {
        // Whether a chosen limit covers this card now, as the card and its menu show it (#232).
        let covered = limit_in_effect(account, &choice.limits[ix]).is_some();
        let account = account.id().as_str().to_owned();
        let available = available.clone();
        let spoken = format!("Choose what the {} shows", names[ix]);
        Button::new(("focus-card-menu", ix))
            .ghost()
            .xsmall()
            .icon(IconName::Settings)
            .accessibility_label(spoken.clone())
            .tooltip(spoken)
            .disabled(read_only)
            .dropdown_menu(move |mut menu, _, _| {
                for entry in &entries {
                    let account = account.clone();
                    let action = entry.action.clone();
                    let available = available.clone();
                    if entry.separator_before {
                        menu = menu.separator();
                    }
                    let item = PopupMenuItem::new(entry.label.clone())
                        .checked(entry.checked)
                        .disabled(entry.disabled);
                    menu = menu.item(item.on_click(
                        move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut App| {
                            let action = action.clone();
                            let available = available.clone();
                            if let MenuAction::DefaultRingSize = action {
                                crate::prefs_hub::PrefsHub::set_ring_size(cx, None);
                                return;
                            }
                            crate::prefs_hub::PrefsHub::update_focus_choice(cx, &account, move |choice| {
                                apply_pick(choice, ix, action, &available, covered)
                            });
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    });
    [menus.next().unwrap(), menus.next().unwrap(), menus.next().unwrap()]
}

/// What picking `action` in card `slot`'s menu does to the account's choices. Any pick other than a limit is the
/// card's own view again (#230). A ring picked while a limit covers the gauge card shows that ring: it may still be
/// chosen underneath, unchecked in the menu, and toggling it would hide it instead (#232).
fn apply_pick(choice: &mut FocusChoice, slot: usize, action: MenuAction, available: &[String], covered: bool) {
    // `covered` is whether the limit is in effect, not merely saved: one the account no longer reports leaves the
    // card and its menu in their own state, so a checked ring there toggles as usual (#232).
    choice.limits[slot] = None;
    match action {
        MenuAction::Usage(view) => choice.usage = view,
        MenuAction::Ring(None) => choice.rings.clear(),
        MenuAction::Ring(Some(key)) if covered && choice.rings.contains(&key) => {}
        MenuAction::Ring(Some(key)) => choice.toggle_ring_among(&key, available),
        MenuAction::Activity(view) => choice.activity = view,
        MenuAction::Limit(limit) => choice.limits[slot] = Some(limit),
        MenuAction::DefaultRingSize => {}
    }
}

/// The wide card: the current window against the previous one, or the 14-day trend when no curve exists.
fn usage_card(account: &AccountSnapshot, now: DateTime<Utc>, view: Option<UsageView>, cx: &App) -> Card {
    // The window curve unless the trend was chosen (#218); a chosen window curve the provider no longer sends falls
    // back to the trend.
    let graph = Graph {
        metric: account.primary(),
        curve: comparable(account.detail().window_curve()).filter(|_| view != Some(UsageView::Trend)),
        trend: account.trend(),
        kind: trend_kind(account),
        counted: counted_name(account),
        title: None,
        severity: account.assess(now).severity(),
    };
    graph_card(&graph, now, 0, cx)
}

/// What a graph card draws: a limit's window curve, or its daily trend when there is no curve, in the limit's unit.
struct Graph<'a> {
    /// The limit the takeaway and caption describe.
    metric: Option<&'a Metric>,
    curve: Option<&'a codexbar_core::WindowCurve>,
    trend: &'a [f64],
    kind: ValueKind,
    /// The name of a counted trend (credits, a count, money), which is its title.
    counted: Option<String>,
    /// The title of a chosen limit's graph (#230); the automatic card's titles otherwise.
    title: Option<String>,
    severity: Severity,
}

/// A graph card in `slot`. The usage card's slot is 0; elsewhere its ids take the slot (#230).
fn graph_card(graph: &Graph, now: DateTime<Utc>, slot: usize, cx: &App) -> Card {
    let current_color = cx.theme().chart_1;
    let previous_color = cx.theme().chart_4;
    let primary = graph.metric;
    let severity = graph.severity;
    let id = |base: &str| -> SharedString {
        if slot == 0 {
            base.to_owned().into()
        } else {
            format!("{base}-{slot}").into()
        }
    };
    let curve = graph.curve;
    let card = if let Some(curve) = curve {
        let points: Vec<(SharedString, f64, f64)> = curve
            .labels()
            .iter()
            .zip(curve.current())
            .zip(curve.previous())
            .map(|((label, current), previous)| (SharedString::from(label.clone()), *current, *previous))
            .collect();
        let count = curve.labels().len();
        let window_chart = |strokes: [Hsla; 2], fills: [Background; 2]| {
            AreaChart::new(points.clone())
                .id(id("focus-window"))
                .x(|(label, _, _)| label.clone())
                .y(|(_, current, _)| *current)
                .stroke(strokes[0])
                .fill(fills[0])
                .name("This window")
                .linear()
                .y(|(_, _, previous)| *previous)
                .stroke(strokes[1])
                .fill(fills[1])
                .name("Previous window")
                .linear()
                .y_domain(0.0, SHARE_TOP)
                .y_axis(true)
                .y_padding(0., 0.)
                .y_tick_count(SHARE_TICKS)
                .point_count(count)
                .tick_margin(6)
                .y_tick_format(share_tick)
        };
        let (current_fill, previous_fill) = (area_fill(current_color), area_fill(previous_color.opacity(0.5)));
        // Smooth curves that stay between their readings (#229), over the chart's own axes and hover.
        let chart = SmoothPlot::new(
            window_chart([CLEAR; 2], [CLEAR.into(); 2]),
            window_chart([current_color, previous_color], [current_fill, previous_fill]),
            vec![
                Series::area(current_color, current_fill),
                Series::area(previous_color, previous_fill),
            ],
            true,
        );
        let title = primary
            .map(|m| format!("{} usage", m.label()))
            .unwrap_or_else(|| "Usage".into());
        let start = curve.labels().first().cloned().unwrap_or_default();
        // Findable by the headless tests, as the day plot is (#200).
        let plot = div()
            .id(id("focus-window-plot"))
            .test_support()
            .size_full()
            .child(chart);
        Card::new(title, format!("Today, {start} – now"), plot)
            .id(id("focus-window-card"))
            .legend(current_color, "This window")
            .legend(previous_color, "Previous window")
    } else if graph.trend.len() < 2 {
        let title = graph.title.clone().unwrap_or_else(|| "Usage over time".into());
        Card::new(title, "History", no_history(cx)).id(id("focus-history-card"))
    } else {
        let points: Vec<(SharedString, f64)> = graph
            .trend
            .iter()
            .enumerate()
            .map(|(ix, value)| (SharedString::from(day_label(graph.trend.len(), ix)), *value))
            .collect();
        // The trend is in the headline's own unit: a fraction, credits left (#150), a count (#103) or money (#104).
        let kind = graph.kind;
        let counted = graph.counted.clone();
        let is_counted = counted.is_some();
        let (low, high) = if is_counted {
            day_domain(graph.trend, kind)
        } else {
            (0.0, SHARE_TOP)
        };
        let name = counted.clone().unwrap_or_else(|| "Pressure".into());
        let trend_chart = |stroke: Hsla, fill: Background| {
            let chart = AreaChart::new(points.clone())
                .id(id("focus-trend"))
                .x(|(label, _)| label.clone())
                .y(|(_, value)| *value)
                .stroke(stroke)
                .fill(fill)
                .name(name.clone())
                .linear()
                .y_domain(low, high)
                .tick_margin(3)
                .y_tick_format(move |value| trend_tick(kind, is_counted, value));
            // A share trend takes the share ticks (#200), so the 100% line keeps its label and the headroom has none.
            if is_counted {
                chart
            } else {
                chart.y_tick_count(SHARE_TICKS)
            }
        };
        let fill = area_fill(current_color);
        // Smooth, and still inside the plot after a reset (#229).
        let chart = SmoothPlot::new(
            trend_chart(CLEAR, CLEAR.into()),
            trend_chart(current_color, fill),
            vec![Series::area(current_color, fill)],
            true,
        );
        let title = counted
            .or_else(|| graph.title.clone())
            .unwrap_or_else(|| "Daily pressure".into());
        Card::new(title, "Last 14 days", chart).id(id("focus-trend-card"))
    };

    let card = match primary {
        Some(metric) => card
            .takeaway(pace_takeaway(metric, now))
            .caption(reset_caption(metric, now)),
        None => card,
    };
    if severity >= Severity::AtRisk {
        card.alert(severity_color(severity, cx))
    } else {
        card
    }
}

/// The share of a pool as a ring gauge (#174), or the credits, balance or spend when no metric has a share.
fn share_card(
    account: &AccountSnapshot,
    now: DateTime<Utc>,
    rings: &[String],
    control: &crate::ring_size::RingControl,
    slot: usize,
    cx: &App,
) -> Card {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    // The chosen limits (#218), in the order chosen; any the account no longer reports are skipped, and with none
    // left the card is automatic again.
    let chosen = chosen_rings(account, rings);
    if chosen.len() >= 2 {
        return rings_card(account, &chosen, now, control, cx);
    }
    let Some(metric) = chosen.first().copied().or_else(|| share_metric(account, now)) else {
        return Card::new("No limits", "", div()).takeaway("This account reports no limits");
    };

    if let Metric::Credits { balance, .. } = metric {
        return credits_card(metric, *balance, cx);
    }
    let Some(used) = metric.used_fraction() else {
        let amount = match metric {
            Metric::Balance { remaining, .. } => remaining.display(),
            Metric::Spend { spent, .. } => spent.display(),
            Metric::Activity { .. } => metric.used_display(),
            _ => "—".into(),
        };
        let body = v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(div().text_3xl().font_semibold().child(amount))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(metric.label().to_owned()),
            );
        if let Metric::Spend { .. } = metric {
            return Card::new(metric.label().to_owned(), "Spent so far", body)
                .takeaway("No spend limit is set for this account");
        }
        if let Metric::Activity { .. } = metric {
            return Card::new(
                metric.label().to_owned(),
                "Activity",
                body.children(crate::sus::metric(
                    metric,
                    now,
                    &format!("focus-activity-{slot}-sus"),
                    cx,
                )),
            )
            .takeaway("Tracked activity, no quota percentage or reset");
        }
        let takeaway = metric
            .days_of_credit()
            .map(|days| format!("About {days:.0} days of credit left"))
            .unwrap_or_else(|| "No spend recorded this week".into());
        return Card::new(metric.label().to_owned(), "Current balance", body).takeaway(takeaway);
    };

    // The number leads (#174): large in the ring, with the pace in the ring's opening and the reset in the caption.
    // The ring draws, colors and says what the bars do (#234): the forecast's share in its opening, the whole forecast
    // to screen readers and on hover, calm when the account's credits carry it (#150).
    let projection = crate::usage_bar::Projection::of_account(metric, used, account, now);
    let color = projection.runway.color(cx.theme().chart_1, cx);
    let projected = projection.projected;
    let pace = ring_pace(metric, account, now);
    let spoken = format!("{}: {}", metric.label(), projection.label);
    // The ring paints its number and pace itself, on its own geometry, so it fits any card width and zoom (#176).
    // Hovering it says what the table's bars say (#197).
    let hover: gpui_kit::SharedString = projection.tooltip.into();
    let ring = div()
        .id(if slot == 1 {
            SharedString::from("focus-share")
        } else {
            format!("focus-share-{slot}").into()
        })
        .role(Role::Note)
        .test_support()
        .aria_label(spoken)
        .size_full()
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hover.clone()).build(window, cx))
        .child(
            crate::gauge::Gauge::new(used, projected, format!("{:.0}", used * 100.0), color, cx)
                .pace(pace)
                .max_radius(control.radius(cx))
                .render(),
        );
    // The ring's size slider overlays its right edge and shows while the card is hovered or the slider is focused
    // (#219). It lives on the gauge card's own slot; a gauge chosen for another card takes the same size (#230).
    let ring = div()
        .relative()
        .size_full()
        .child(ring)
        .when(slot == 1, |this| this.child(control.render(cx)));
    let takeaway = pool_takeaway(1.0 - used);
    let title = window_title(metric);
    let mut caption = reset_caption(metric, now);
    if let Some(credits) = displaced_credits(account) {
        caption = if caption.is_empty() {
            credits
        } else {
            format!("{caption} · {credits}")
        };
    }
    Card::new(
        title,
        window_period(metric, now),
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(ring))
            .children(crate::sus::metric(metric, now, &format!("focus-share-{slot}-sus"), cx)),
    )
    .in_slot(slot, 1)
    .hover_group(crate::ring_size::GROUP)
    .takeaway(takeaway)
    .caption(caption)
}

/// The chosen ring metrics that the account still reports with a share of a limit, in the order chosen.
fn chosen_rings<'a>(account: &'a AccountSnapshot, rings: &[String]) -> Vec<&'a Metric> {
    rings
        .iter()
        .filter_map(|key| limits(account).find(|metric| &metric.key() == key))
        .take(MAX_RINGS)
        .collect()
}

/// Several rings side by side (#218), one per chosen limit, each with its own number, pace and reset. The takeaway
/// names the limit with the least of its pool left.
fn rings_card(
    account: &AccountSnapshot,
    metrics: &[&Metric],
    now: DateTime<Utc>,
    control: &crate::ring_size::RingControl,
    cx: &App,
) -> Card {
    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    let muted = cx.theme().muted_foreground;
    let rings = metrics.iter().enumerate().map(|(ix, metric)| {
        let used = metric.used_fraction().unwrap_or_default();
        let projection = crate::usage_bar::Projection::of_account(metric, used, account, now);
        let color = projection.runway.color(cx.theme().chart_1, cx);
        let projected = projection.projected;
        let pace = ring_pace(metric, account, now);
        let spoken = format!("{}: {}", metric.label(), projection.label);
        let hover: SharedString = projection.tooltip.into();
        v_flex()
            .id(("focus-ring", ix))
            .role(Role::Note)
            .test_support()
            .aria_label(spoken)
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_1()
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(hover.clone()).build(window, cx))
            .child(
                // Two lines are kept for every label, so a label that wraps doesn't push its ring below its
                // neighbours'.
                v_flex()
                    .min_h(rems(2.6))
                    .justify_end()
                    .text_sm()
                    .text_color(muted)
                    .text_center()
                    .child(metric.label().to_owned()),
            )
            .child(
                div().flex_1().min_h_0().child(
                    crate::gauge::Gauge::new(used, projected, format!("{:.0}", used * 100.0), color, cx)
                        .pace(pace)
                        .max_radius(control.radius(cx))
                        .render(),
                ),
            )
            .children(crate::sus::metric(
                metric,
                now,
                &format!("focus-limits-{}-sus", metric.key()),
                cx,
            ))
            // The reset line is kept even when a limit has no reset, such as Claude's Extra usage, so its ring sits
            // level with the others.
            .child(
                div()
                    .min_h(rems(2.4))
                    .text_xs()
                    .text_color(muted)
                    .text_center()
                    .child(reset_caption(metric, now)),
            )
    });
    // Labels and resets wrap rather than cut off when the card is narrow or zoomed in.
    let body = h_flex()
        .relative()
        .size_full()
        .gap_3()
        .children(rings)
        .child(control.render(cx));
    let tightest = metrics
        .iter()
        .max_by(|a, b| {
            let used = |metric: &&&Metric| metric.used_fraction().unwrap_or_default();
            used(a).total_cmp(&used(b))
        })
        .copied();
    let takeaway = tightest
        .map(|metric| {
            format!(
                "{}: {}",
                metric.label(),
                pool_takeaway(1.0 - metric.used_fraction().unwrap_or_default()).to_lowercase()
            )
        })
        .unwrap_or_default();
    Card::new("Limits", format!("{} limits", metrics.len()), body)
        .id("focus-card-limits")
        .hover_group(crate::ring_size::GROUP)
        .takeaway(takeaway)
}

/// The metric for the second card: the one after the headline that raises the status when it has a share of a pool
/// (so Cursor's on-demand spend stays on screen), otherwise the headline's share, otherwise the most pressing share
/// further down (GitHub Billing lists its allowances after two uncapped spends) (#174). Only when no metric has a
/// share is it whatever the second one is: credits, a balance, a spend with no limit.
fn share_metric(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<&Metric> {
    let primary = account.primary()?;
    let secondary = secondary_metric(account, now);
    let has_share = |metric: &Metric| metric.used_fraction().is_some();
    secondary
        .filter(|metric| has_share(metric))
        .or_else(|| primary.used_fraction().map(|_| primary))
        .or_else(|| most_severe_after_primary(account, now, has_share))
        .or(secondary)
        .or(Some(primary))
}

/// The credits line for the ring's caption when the share took the card (#174): the credits after the headline,
/// wherever the provider lists them (Codex puts both windows before them). Credits that lead the account (#150)
/// already have the first card, so they get no line.
fn displaced_credits(account: &AccountSnapshot) -> Option<String> {
    account.metrics().iter().skip(1).find_map(|metric| match metric {
        Metric::Credits {
            balance: Some(balance), ..
        } => Some(format!("{} credits left", codexbar_core::format_credits(*balance))),
        Metric::Credits { balance: None, .. } => Some("Unlimited credits".to_owned()),
        _ => None,
    })
}

/// Requests by hour when the provider reports them, otherwise daily pressure as bars.
fn activity_card(account: &AccountSnapshot, view: Option<ActivityView>, cx: &App) -> Card {
    let color = cx.theme().chart_1;
    let bar_fill = move |_: &(SharedString, f64), _, _, _| -> Background {
        linear_gradient(
            180.,
            linear_color_stop(color, 0.),
            linear_color_stop(color.opacity(0.55), 1.),
        )
    };
    let requests = account.detail().requests_by_hour();
    // Requests by hour unless usage by day was chosen (#218).
    if !requests.is_empty() && view != Some(ActivityView::Days) {
        let bars: Vec<(SharedString, f64)> = requests
            .iter()
            .enumerate()
            .map(|(hour, count)| (SharedString::from(format!("{hour:02}:00")), *count as f64))
            .collect();
        let (peak_hour, _) = requests
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .unwrap_or((0, &0));
        let total: u32 = requests.iter().sum();
        let chart = BarChart::new(bars)
            .id("focus-requests")
            .band(|(label, _)| label.clone())
            .value(|(_, value)| *value)
            .fill(bar_fill)
            .tick_margin(6)
            .value_axis(true)
            .value_tick_count(4)
            .value_tick_format(|value| format!("{value:.0}"))
            .name("Requests");
        return Card::new("Requests by hour", "Last 24 hours", chart)
            .takeaway(format!("Peak at {peak_hour:02}:00"))
            .caption(format!(
                "{} requests today",
                codexbar_core::group_thousands(u64::from(total))
            ));
    }

    use gpui_kit::{InteractiveElement as _, Role, StatefulInteractiveElement as _, TestSupportExt as _};
    let trend = account.trend();
    if trend.len() < 2 {
        let empty = div()
            .id("focus-days-empty")
            .role(Role::Note)
            .test_support()
            .aria_label("No daily usage yet; it appears once history is stored")
            .size_full()
            .child(no_history(cx));
        return Card::new("Usage by day", "Last 7 days", empty)
            .takeaway("No history yet")
            .caption("Daily usage appears once history is stored");
    }
    let days: Vec<(SharedString, f64)> = trend
        .iter()
        .enumerate()
        .skip(trend.len().saturating_sub(7))
        .map(|(ix, value)| (SharedString::from(day_label(trend.len(), ix)), *value))
        .collect();
    let kind = trend_kind(account);
    let counted = counted_name(account);
    let values: Vec<f64> = days.iter().map(|(_, value)| *value).collect();
    let (low, high) = day_domain(&values, kind);
    let ticks = day_ticks((low, high), kind);
    let spoken = format!(
        "Usage by day: {}",
        days.iter()
            .map(|(day, value)| format!("{day} {}", kind.value(*value)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // A line through the week's points, one marker per day, in the headline's unit (#180). The axis is pinned to the
    // unit's range with no pixel padding, so its ticks read 0, 33, 67, 100%: the chart spaces ticks over the padded
    // extent, and padding made them read -8% and 108%. The line is a monotone curve (#229): gpui-kit's own smooth
    // curve is a natural spline, which swings past 0% or 100% around a sharp change such as a reset (#217).
    let name = counted.clone().unwrap_or_else(|| "Pressure".into());
    let days_chart = |stroke: Hsla| {
        LineChart::new(days.clone())
            .id("focus-days")
            .x(|(label, _)| label.clone())
            .y(|(_, value)| *value)
            .stroke(stroke)
            .linear()
            .dot()
            .y_domain(low, high)
            .y_padding(0., 0.)
            .y_axis(true)
            .y_tick_count(ticks)
            .y_tick_format(move |value| {
                if kind == ValueKind::Percent {
                    share_tick(value)
                } else {
                    kind.value(value)
                }
            })
            .tooltip_value(move |_, value| kind.value(value).into())
            .name(name.clone())
    };
    let chart = SmoothPlot::new(days_chart(CLEAR), days_chart(color), vec![Series::line(color)], true);
    let plot = div()
        .id("focus-days-plot")
        .role(Role::Note)
        .test_support()
        .aria_label(spoken)
        .size_full()
        .child(chart);
    Card::new("Usage by day", "Last 7 days", plot)
        .takeaway(day_takeaway(trend, kind, on_credits(account)))
        .caption(day_caption(counted.as_deref()))
}

/// The day line's axis range: a share is pinned to 0–100%; a count, credits or money runs from zero (or below the
/// week's lowest value, for a balance gone negative, #104) to 15% over its highest day, never flat.
fn day_domain(values: &[f64], kind: ValueKind) -> (f64, f64) {
    if kind == ValueKind::Percent {
        return (0.0, SHARE_TOP);
    }
    let low = values.iter().copied().fold(0.0, f64::min);
    let high = values.iter().copied().fold(0.0, f64::max);
    let low = if low < 0.0 { low * 1.15 } else { 0.0 };
    (low, (high * 1.15).max(low + 1.0))
}

/// How many ticks the day line's axis gets: four when the unit's formatting tells them apart, otherwise the most
/// that still read distinctly, down to two. Ticks sit evenly over `low..=high` before formatting, so a week of one
/// or two activities, or of credits down to a fraction, never reads "0, 1, 1, 2".
fn day_ticks((low, high): (f64, f64), kind: ValueKind) -> usize {
    if kind == ValueKind::Percent {
        return SHARE_TICKS;
    }
    (2..=4)
        .rev()
        .find(|count| {
            let labels: Vec<String> = (0..*count)
                .map(|ix| kind.value(low + (high - low) * ix as f64 / (count - 1) as f64))
                .collect();
            labels.iter().collect::<std::collections::HashSet<_>>().len() == labels.len()
        })
        .unwrap_or(2)
}

/// How yesterday compares: "Up 12% since yesterday" for a share, "Down $2.50 since yesterday" for an amount (#103,
/// #104), "Flat since yesterday", or "Topped up since yesterday" for credits that came back from zero (#150).
fn day_takeaway(trend: &[f64], kind: ValueKind, on_credits: bool) -> String {
    let change = match trend {
        [.., before, last] if *before > 0.0 => (last - before) / before * 100.0,
        _ => 0.0,
    };
    let topped_up = on_credits && matches!(trend, [.., before, last] if *before <= 0.0 && *last > 0.0);
    // A count (#103) or money (#104) changes by an amount, not a share.
    let by_amount = matches!(kind, ValueKind::Count | ValueKind::Money(_));
    if topped_up {
        // From zero there is no percentage to give (#150).
        "Topped up since yesterday".to_owned()
    } else if let (true, [.., before, last]) = (by_amount, trend) {
        let delta = last - before;
        if kind.change(delta) == "no change" {
            "Flat since yesterday".to_owned()
        } else {
            format!(
                "{} {} since yesterday",
                if delta > 0.0 { "Up" } else { "Down" },
                kind.value(delta.abs())
            )
        }
    } else if change.abs() < 1.0 {
        "Flat since yesterday".to_owned()
    } else {
        format!(
            "{} {:.0}% since yesterday",
            if change > 0.0 { "Up" } else { "Down" },
            change.abs()
        )
    }
}

/// What the day card's points are: the share of the tightest limit, or the counted headline at each day's end.
fn day_caption(counted: Option<&str>) -> String {
    match counted {
        Some(name) => format!("{name} at the end of each day"),
        None => "Share of the tightest limit".to_owned(),
    }
}

/// True when the account's headline is its credits (#150), so its trend is a count of credits left.
fn on_credits(account: &AccountSnapshot) -> bool {
    matches!(account.primary(), Some(Metric::Credits { .. }))
}

/// What the trend holds when the headline isn't a fraction: credits left (#150), an activity count such as completed
/// reviews (#103), or money left or spent (#104).
fn counted_name(account: &AccountSnapshot) -> Option<String> {
    account.primary().and_then(counted_metric)
}

/// The name of a metric whose history is counted rather than a share: credits, a count, a balance or spend.
fn counted_metric(metric: &Metric) -> Option<String> {
    match metric {
        Metric::Credits { .. } => Some("Credits left".to_owned()),
        Metric::Activity { label, .. } | Metric::Spend { label, .. } | Metric::Balance { label, .. } => {
            Some(label.clone())
        }
        _ => None,
    }
}

/// The unit the trend is in, as History stores it: a fraction of a limit, money, credits or a count.
fn trend_kind(account: &AccountSnapshot) -> ValueKind {
    account.primary().map_or(ValueKind::Percent, ValueKind::of)
}

/// How much of a window's pool is left, in words.
fn pool_takeaway(remaining: f64) -> String {
    match remaining {
        r if r >= 0.9 => "Nearly all of the pool left".to_owned(),
        r if (0.45..=0.55).contains(&r) => "About half the pool left".to_owned(),
        r if r <= 0.0 => "Pool used up".to_owned(),
        r => format!("{:.0}% of the pool left", r * 100.0),
    }
}

/// The empty state for charts that need stored history.
fn no_history(cx: &App) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Icon::new(IconName::Calendar))
        .child("History builds up as CodexBar refreshes")
}

/// How much of the remembered starting balance the credits have used (#150): "63% of 50,000 used".
fn credit_spend(metric: &Metric) -> Option<String> {
    let Metric::Credits { start: Some(start), .. } = metric else {
        return None;
    };
    let progress = metric.progress()?;
    Some(format!(
        "{:.0}% of {} used",
        progress * 100.0,
        codexbar_core::format_credits(*start)
    ))
}

/// Credits left, with a bar running down from the remembered start when there is one (#150).
fn credits_card(metric: &Metric, balance: Option<f64>, cx: &App) -> Card {
    let amount = balance.map_or_else(|| "Unlimited".to_owned(), codexbar_core::format_credits);
    let mut body = v_flex()
        .size_full()
        .gap_2()
        .items_center()
        .justify_center()
        .child(div().text_3xl().font_semibold().child(amount))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("credits left"),
        );
    if let Some(progress) = metric.progress() {
        // The same bar as the table's, with its tooltip (#197); credits project nothing, and it says so.
        body = body.child(div().w_full().px_4().child(crate::usage_bar::usage_bar(
            "focus-credits",
            &crate::usage_bar::Projection::of(metric, progress, Utc::now()),
            cx.theme().chart_1,
            crate::usage_bar::medium(),
            "Credits since the last top-up",
            cx,
        )));
    }
    let takeaway = match (balance, credit_spend(metric)) {
        (None, _) => "This account's credits are unlimited".to_owned(),
        (Some(_), Some(spent)) => format!("{spent} since the last top-up"),
        (Some(_), None) => "The bar starts with the next refresh".to_owned(),
    };
    Card::new(metric.label().to_owned(), "Credits pool", body).takeaway(takeaway)
}

/// The most severe metric after the primary, so whatever raises the account's status (such as Cursor's on-demand
/// spend) is on screen. Ties keep provider order, so it is usually the second metric.
fn secondary_metric(account: &AccountSnapshot, now: DateTime<Utc>) -> Option<&Metric> {
    most_severe_after_primary(account, now, |_| true)
}

/// The most severe of the metrics after the primary that `keep` accepts; ties keep provider order.
fn most_severe_after_primary(
    account: &AccountSnapshot,
    now: DateTime<Utc>,
    keep: impl Fn(&Metric) -> bool,
) -> Option<&Metric> {
    account
        .metrics()
        .iter()
        .skip(1)
        .enumerate()
        .filter(|(_, metric)| keep(metric))
        .max_by(|(a_ix, a), (b_ix, b)| {
            let severity = |metric: &Metric| codexbar_core::assess(metric, now).severity();
            severity(a).cmp(&severity(b)).then(b_ix.cmp(a_ix))
        })
        .map(|(_, metric)| metric)
}

fn pace_takeaway(metric: &Metric, now: DateTime<Utc>) -> String {
    if let Some(days) = metric.days_of_credit() {
        return format!("About {days:.0} days of credit left");
    }
    // The outlook picks the wording (#206), so the card and its tile agree even at the minute where a limit hit
    // just before the reset counts as a full share.
    match (metric.outlook(now), metric.time_to_limit()) {
        (Some(outlook @ codexbar_core::Outlook::Early(_)), Some(to_limit)) => format!(
            "Hits the {} limit in ~{} at this pace, {}",
            metric.label().to_lowercase(),
            format::countdown(to_limit),
            format::outlook_text(outlook)
        ),
        (Some(codexbar_core::Outlook::Share(share)), _) => {
            format!("On pace for about {:.0}% by the reset", share * 100.0)
        }
        (_, Some(_)) => "On pace to stay under the limit".to_owned(),
        (_, None) => "Steady".to_owned(),
    }
}

/// "Resets in 2d 4h", or "Resets now": always time left (#196), as the table's Resets column says it.
fn reset_caption(metric: &Metric, now: DateTime<Utc>) -> String {
    match metric.resets_at() {
        Some(at) => format!("Resets {}", format::time_left_label(at, now)),
        None => String::new(),
    }
}

/// "Oct 2 – Oct 9" for a window that resets within a week; otherwise its reset date.
fn window_period(metric: &Metric, now: DateTime<Utc>) -> String {
    period_in(metric, now, &Local)
}

/// The stretch of time a window covers, from the length the provider reports (#223): start and end times for a window
/// of a day or less, with dates only when it crosses midnight, and start and end dates for longer ones. Without a
/// length there is no start to show, so only the end: its date, or its time when that is today.
fn period_in<Tz: chrono::TimeZone>(metric: &Metric, now: DateTime<Utc>, tz: &Tz) -> String {
    let style = crate::locale::style();
    let Some(end) = metric.resets_at() else {
        return String::new();
    };
    let end = end.with_timezone(tz);
    match metric.window_length() {
        Some(length) if length <= chrono::Duration::days(1) => {
            let start = end.clone() - length;
            if start.date_naive() == end.date_naive() {
                format!(
                    "{} – {}",
                    style.time(start.naive_local()),
                    style.time(end.naive_local())
                )
            } else {
                format!(
                    "{} – {}",
                    style.month_day_time(start.naive_local()),
                    style.month_day_time(end.naive_local())
                )
            }
        }
        Some(length) => format!(
            "{} – {}",
            style.month_day((end.clone() - length).date_naive()),
            style.month_day(end.date_naive())
        ),
        None if end.date_naive() == now.with_timezone(tz).date_naive() => {
            format!("Until {}", style.time(end.naive_local()))
        }
        None => format!("Until {}", style.month_day(end.date_naive())),
    }
}

/// Weekday initials for a trend whose last point is today.
fn day_label(len: usize, ix: usize) -> String {
    let days_ago = (len - 1 - ix) as i64;
    crate::locale::style().weekday((Local::now() - chrono::Duration::days(days_ago)).date_naive())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use codexbar_core::Pace;

    fn window(used: f64, per_hour: f64) -> Metric {
        Metric::Window {
            label: "Included usage".into(),
            used,
            resets_at: now() + Duration::hours(10),
            pace: Some(Pace::per_hour(per_hour)),
            length: None,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    /// A window resetting at `resets_at` (UTC), with the given length if known.
    fn lasting(resets_at: &str, length: Option<Duration>) -> Metric {
        Metric::Window {
            label: "Limit".into(),
            used: 0.5,
            resets_at: resets_at.parse().unwrap(),
            pace: None,
            length,
        }
    }

    #[test]
    fn the_period_follows_the_window_s_own_length() {
        // US Eastern in October: four hours behind UTC.
        let eastern = chrono::FixedOffset::west_opt(4 * 3600).unwrap();
        let now: DateTime<Utc> = "2026-10-19T14:00:00Z".parse().unwrap();
        // A 5-hour window shows times, not a week of dates (it read "Oct 12 – Oct 19").
        let five_hours = lasting("2026-10-19T19:22:00Z", Some(Duration::hours(5)));
        assert_eq!(period_in(&five_hours, now, &eastern), "10:22 – 15:22");
        // Across midnight, both ends carry their date.
        let late = lasting("2026-10-20T07:00:00Z", Some(Duration::hours(5)));
        assert_eq!(period_in(&late, now, &eastern), "Oct 19 22:00 – Oct 20 03:00");
        // A weekly window keeps its dates.
        let weekly = lasting("2026-10-22T13:00:00Z", Some(Duration::days(7)));
        assert_eq!(period_in(&weekly, now, &eastern), "Oct 15 – Oct 22");
        // Without a length there is no start to show: only the end, as a time when it is today.
        let unknown = lasting("2026-10-22T13:00:00Z", None);
        assert_eq!(period_in(&unknown, now, &eastern), "Until Oct 22");
        let unknown_today = lasting("2026-10-19T19:22:00Z", None);
        assert_eq!(period_in(&unknown_today, now, &eastern), "Until 15:22");
        // An activity count has no reset and no period.
        let count = Metric::Activity {
            label: "Reviews".into(),
            count: 3,
        };
        assert_eq!(period_in(&count, now, &eastern), "");
    }

    fn claude() -> AccountSnapshot {
        use codexbar_core::{AccountId, Money, Provider};
        let named = |label: &str, used: f64| Metric::Window {
            label: label.into(),
            used,
            resets_at: now() + Duration::hours(10),
            pace: None,
            length: None,
        };
        AccountSnapshot::new(
            AccountId::new("claude"),
            Provider::Claude,
            vec![
                named("Weekly", 0.82),
                named("5-hour window", 0.64),
                Metric::Balance {
                    label: "Credits".into(),
                    remaining: Money::from_cents(500),
                    burn_per_day: None,
                },
            ],
            now(),
        )
        .with_trend(vec![0.4, 0.5, 0.6])
    }

    #[test]
    fn a_non_primary_limit_graph_uses_its_own_window_curve() {
        use codexbar_core::{AccountDetail, LimitSeries, WindowCurve};
        let curve = WindowCurve::new(vec!["start".into(), "end".into()], vec![0.2, 0.64], vec![0.1, 0.5]);
        let account = claude().with_detail(AccountDetail::default().with_limit_series(
            "5-hour-window",
            LimitSeries {
                curve: Some(curve),
                trend: vec![0.3, 0.64],
            },
        ));
        let graph = limit_graph(&account, &account.metrics()[1], now());
        let curve = graph.curve.expect("the non-primary window must keep its own curve");
        assert_eq!(curve.current(), [0.2, 0.64]);
        assert_eq!(curve.previous(), [0.1, 0.5]);
        assert_eq!(graph.trend, [0.3, 0.64]);
    }

    #[test]
    fn each_card_menu_offers_only_what_the_account_reports() {
        let account = claude();
        let [usage, rings, activity] = menu_entries(&account, &FocusChoice::default(), false);
        let labels = |entries: &[MenuEntry]| entries.iter().map(|e| e.label.clone()).collect::<Vec<_>>();
        // No window curve from this account, so only the trend; no requests by hour, so only usage by day. Then each
        // limit in each view (#230); a balance has no share of a limit, so it is not one.
        let limit_entries = |gauge: bool| {
            let mut entries = Vec::new();
            for limit in ["Weekly", "5-hour window"] {
                entries.push(format!("{limit} as a graph"));
                entries.push(format!("{limit} as a bar"));
                if gauge {
                    entries.push(format!("{limit} as a gauge"));
                }
            }
            entries
        };
        let with = |own: &[&str], gauge: bool| {
            let mut all: Vec<String> = own.iter().map(|label| (*label).to_owned()).collect();
            all.extend(limit_entries(gauge));
            all
        };
        assert_eq!(labels(&usage), with(&["Automatic", "Last 14 days"], true));
        assert_eq!(labels(&activity), with(&["Automatic", "Usage by day"], true));
        // The gauge card's limits are its rings, so it offers no "as a gauge".
        assert_eq!(labels(&rings), with(&["Automatic", "Weekly", "5-hour window"], false));
        // The limits start a new group.
        assert!(usage[2].separator_before && !usage[3].separator_before && !usage[1].separator_before);
        assert!(
            usage[0].checked && rings[0].checked && activity[0].checked,
            "automatic by default"
        );

        let mut choice = FocusChoice::default();
        choice.toggle_ring("5-hour-window");
        let [_, rings, _] = menu_entries(&account, &choice, false);
        let checked: Vec<_> = rings.iter().filter(|e| e.checked).map(|e| e.label.as_str()).collect();
        assert_eq!(checked, ["5-hour window"]);
    }

    #[test]
    fn a_limit_chosen_for_a_card_is_the_only_checked_entry_and_falls_back_when_unreported() {
        let account = claude();
        let mut choice = FocusChoice::default();
        choice.limits[0] = Some(LimitChoice {
            key: "5-hour-window".into(),
            view: LimitView::Graph,
        });
        choice.limits[2] = Some(LimitChoice {
            key: "gone".into(),
            view: LimitView::Bar,
        });
        let [usage, _, activity] = menu_entries(&account, &choice, false);
        let checked: Vec<_> = usage.iter().filter(|e| e.checked).map(|e| e.label.as_str()).collect();
        assert_eq!(checked, ["5-hour window as a graph"]);
        // A limit the account doesn't report: the card is automatic again, and its menu says so.
        assert_eq!(limit_in_effect(&account, &choice.limits[2]), None);
        let checked: Vec<_> = activity
            .iter()
            .filter(|e| e.checked)
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(checked, ["Automatic"]);
    }

    #[test]
    fn a_chosen_spend_graph_stays_in_money() {
        // Claude's Extra usage is a capped spend in euros; its history is money, not a share (#232).
        use codexbar_core::{AccountId, Currency, LimitSeries, Money, Provider};
        let extra = Metric::Spend {
            label: "Extra usage".into(),
            spent: Money::new(420, Currency::Eur),
            limit: Some(Money::new(2000, Currency::Eur)),
            resets_at: None,
            pace: None,
        };
        let account = AccountSnapshot::new(
            AccountId::new("claude"),
            Provider::Claude,
            vec![window(0.5, 0.01), extra.clone()],
            now(),
        )
        .with_detail(codexbar_core::AccountDetail::default().with_limit_series(
            "extra-usage",
            LimitSeries {
                curve: None,
                trend: vec![1.0, 2.5, 4.2],
            },
        ));
        let graph = limit_graph(&account, &account.metrics()[1], now());
        assert_eq!(graph.counted.as_deref(), Some("Extra usage"));
        assert_eq!(graph.kind, ValueKind::Money(Currency::Eur));
        assert_eq!(graph.trend, [1.0, 2.5, 4.2]);
        // A share keeps the share axis.
        assert_eq!(limit_graph(&account, &account.metrics()[0], now()).counted, None);
    }

    #[test]
    fn a_curve_needs_both_windows() {
        // In a limit's first window there is no previous one yet (#232): the card falls back to its trend.
        let first = codexbar_core::WindowCurve::new(vec!["10:00".into(), "10:10".into()], vec![0.1, 0.2], vec![]);
        assert!(comparable(Some(&first)).is_none());
        let both =
            codexbar_core::WindowCurve::new(vec!["10:00".into(), "10:10".into()], vec![0.1, 0.2], vec![0.1, 0.3]);
        assert!(comparable(Some(&both)).is_some());
        assert!(comparable(None).is_none());
    }

    #[test]
    fn a_ring_picked_under_a_limit_shows_that_ring() {
        let available = vec!["weekly".to_owned(), "5-hour-window".to_owned()];
        let mut choice = FocusChoice {
            rings: vec!["weekly".into()],
            ..FocusChoice::default()
        };
        choice.limits[1] = Some(LimitChoice {
            key: "5-hour-window".into(),
            view: LimitView::Bar,
        });
        // Weekly is still chosen underneath, unchecked in the menu: picking it shows it rather than hiding it (#232).
        apply_pick(
            &mut choice,
            1,
            MenuAction::Ring(Some("weekly".into())),
            &available,
            true,
        );
        assert_eq!(choice.limits[1], None);
        assert_eq!(choice.rings, ["weekly"]);
        // Without a limit over the card, a pick toggles as before.
        apply_pick(
            &mut choice,
            1,
            MenuAction::Ring(Some("weekly".into())),
            &available,
            false,
        );
        assert!(choice.rings.is_empty());
        // A saved limit the account no longer reports doesn't cover the card: its checked ring toggles off at once,
        // and the stale limit goes too (#232).
        choice.rings = vec!["weekly".into()];
        choice.limits[1] = Some(LimitChoice {
            key: "gone".into(),
            view: LimitView::Bar,
        });
        apply_pick(
            &mut choice,
            1,
            MenuAction::Ring(Some("weekly".into())),
            &available,
            false,
        );
        assert!(choice.rings.is_empty());
        assert_eq!(choice.limits[1], None);
        // Any other pick clears the card's limit too, and a limit pick sets it.
        let limit = LimitChoice {
            key: "weekly".into(),
            view: LimitView::Graph,
        };
        apply_pick(&mut choice, 0, MenuAction::Limit(limit.clone()), &available, false);
        assert_eq!(choice.limits[0], Some(limit));
        apply_pick(
            &mut choice,
            0,
            MenuAction::Usage(Some(UsageView::Trend)),
            &available,
            true,
        );
        assert_eq!((choice.limits[0].clone(), choice.usage), (None, Some(UsageView::Trend)));
    }

    #[test]
    fn a_saved_view_the_account_cannot_draw_reads_as_automatic() {
        // Claude here has a trend but no window curve and no requests by hour.
        let account = claude();
        let choice = FocusChoice {
            usage: Some(UsageView::Window),
            activity: Some(ActivityView::Hours),
            ..FocusChoice::default()
        };
        assert_eq!(usage_in_effect(&account, choice.usage), None);
        assert_eq!(activity_in_effect(&account, choice.activity), None);
        let [usage, _, activity] = menu_entries(&account, &choice, false);
        assert!(usage[0].checked, "Automatic, since the card shows its automatic view");
        assert!(activity[0].checked);
        // A trend chosen with fewer than two days of history falls back too, rather than hiding a window curve.
        let young = AccountSnapshot::new(
            account.id().clone(),
            account.provider(),
            account.metrics().to_vec(),
            now(),
        );
        assert_eq!(usage_in_effect(&young, Some(UsageView::Trend)), None);
        assert_eq!(
            usage_in_effect(&account, Some(UsageView::Trend)),
            Some(UsageView::Trend)
        );
    }

    #[test]
    fn the_gauge_menu_reads_automatic_when_no_chosen_limit_is_reported_and_stops_at_three() {
        let account = claude();
        let stale = FocusChoice {
            rings: vec!["opus-weekly".into()],
            ..FocusChoice::default()
        };
        let [_, rings, _] = menu_entries(&account, &stale, false);
        assert!(rings[0].checked, "Automatic, since the card shows the automatic ring");

        let full = FocusChoice {
            rings: vec!["weekly".into(), "5-hour-window".into(), "spare".into()],
            ..FocusChoice::default()
        };
        // Only two of the three are reported, so the card isn't full and nothing is disabled.
        let [_, rings, _] = menu_entries(&account, &full, false);
        assert!(rings.iter().all(|entry| !entry.disabled));
    }

    #[test]
    fn chosen_rings_skip_limits_the_account_no_longer_reports() {
        let account = claude();
        let keys = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        let labels = |metrics: Vec<&Metric>| metrics.into_iter().map(|m| m.label().to_owned()).collect::<Vec<_>>();
        assert_eq!(
            labels(chosen_rings(&account, &keys(&["5-hour-window", "weekly"]))),
            ["5-hour window", "Weekly"],
            "in the order chosen"
        );
        assert_eq!(
            labels(chosen_rings(&account, &keys(&["opus-weekly", "weekly"]))),
            ["Weekly"]
        );
        assert!(
            chosen_rings(&account, &keys(&["gone", "credits"])).is_empty(),
            "back to automatic"
        );
    }

    #[test]
    fn the_second_card_shows_the_metric_that_raises_the_status() {
        use codexbar_core::{AccountId, Money, Provider};
        let account = |on_demand_spent: i64| {
            AccountSnapshot::new(
                AccountId::new("cursor"),
                Provider::Cursor,
                vec![
                    window(0.3, 0.01),
                    window(0.2, 0.01),
                    window(0.1, 0.01),
                    Metric::Spend {
                        label: "On-demand".into(),
                        spent: Money::from_cents(on_demand_spent),
                        limit: Some(Money::from_cents(2000)),
                        resets_at: Some(now() + Duration::hours(10)),
                        pace: None,
                    },
                ],
                now(),
            )
        };
        let calm = account(100);
        assert_eq!(
            secondary_metric(&calm, now()),
            calm.metrics().get(1),
            "provider order when calm"
        );
        let urgent = account(1980);
        assert_eq!(secondary_metric(&urgent, now()).unwrap().label(), "On-demand");
    }

    #[test]
    fn the_share_leads_the_second_card_and_displaced_credits_keep_a_line() {
        use codexbar_core::{AccountId, Provider};
        let credits = |balance: f64| Metric::Credits {
            label: "Credits".into(),
            balance: Some(balance),
            start: None,
        };
        let account = |metrics| AccountSnapshot::new(AccountId::new("codex"), Provider::Codex, metrics, now());

        // A Pro account mid-week (#174): the weekly share takes the card, and the credits keep a caption line.
        let pro = account(vec![window(0.59, 0.02), credits(55_234.7)]);
        assert_eq!(share_metric(&pro, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&pro).as_deref(), Some("55,234.70 credits left"));

        // Codex lists both windows before the credits: the ring shows the second window, the credits keep the line.
        let three = account(vec![window(0.91, 0.2), window(0.47, 0.01), credits(55_234.7)]);
        assert_eq!(share_metric(&three, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&three).as_deref(), Some("55,234.70 credits left"));
        let unlimited = account(vec![
            window(0.3, 0.01),
            Metric::Credits {
                label: "Credits".into(),
                balance: None,
                start: None,
            },
        ]);
        assert_eq!(displaced_credits(&unlimited).as_deref(), Some("Unlimited credits"));

        // With the window used up the credits lead (#150) and have the first card, so the ring gets no line.
        let used_up = account(vec![window(1.0, 0.0), credits(18_400.0)]).with_credits_placed();
        assert!(matches!(used_up.primary(), Some(Metric::Credits { .. })));
        assert_eq!(share_metric(&used_up, now()).unwrap().label(), "Included usage");
        assert_eq!(displaced_credits(&used_up), None);

        // Credits alone keep their card.
        let only = account(vec![credits(10.0)]);
        assert!(matches!(share_metric(&only, now()), Some(Metric::Credits { .. })));
        assert_eq!(displaced_credits(&only), None);

        // GitHub Billing: two uncapped spends lead, and the allowance after them is the share the ring shows.
        use codexbar_core::Money;
        let spend = |label: &str| Metric::Spend {
            label: label.into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        };
        let billing = account(vec![
            spend("Net spend"),
            spend("Gross charges"),
            Metric::Quota {
                label: "Actions minutes".into(),
                used: 600,
                limit: 2000,
                resets_at: now() + Duration::days(10),
                pace: None,
                length: None,
            },
        ]);
        assert_eq!(share_metric(&billing, now()).unwrap().label(), "Actions minutes");
        let uncapped = account(vec![spend("Net spend"), spend("Gross charges")]);
        assert_eq!(
            share_metric(&uncapped, now()).unwrap().label(),
            "Gross charges",
            "no share anywhere: the second metric keeps the card"
        );
    }

    #[test]
    fn pace_takeaway_shows_the_projection_or_when_the_limit_is_hit() {
        assert_eq!(
            pace_takeaway(&window(0.4, 0.03), now()),
            "On pace for about 70% by the reset"
        );
        // 60% left at 10%/h: the limit in 6h, four hours before the 10h reset (#206).
        assert_eq!(
            pace_takeaway(&window(0.4, 0.1), now()),
            "Hits the included usage limit in ~6h at this pace, 4h early"
        );
        // 60% left at 6%/h lands on the 10h reset: a full share, as the tile says, not "hits the limit".
        assert_eq!(
            pace_takeaway(&window(0.4, 0.06), now()),
            "On pace for about 100% by the reset"
        );
    }

    #[test]
    fn the_day_card_says_how_yesterday_compares_in_the_headlines_unit() {
        use codexbar_core::Currency;
        assert_eq!(
            day_takeaway(&[0.5, 0.6], ValueKind::Percent, false),
            "Up 20% since yesterday"
        );
        assert_eq!(
            day_takeaway(&[0.6, 0.6], ValueKind::Percent, false),
            "Flat since yesterday"
        );
        assert_eq!(
            day_takeaway(&[3.0, 5.0], ValueKind::Count, false),
            "Up 2 since yesterday"
        );
        assert_eq!(
            day_takeaway(&[10.0, 7.5], ValueKind::Money(Currency::Usd), false),
            "Down $2.50 since yesterday"
        );
        assert_eq!(
            day_takeaway(&[0.0, 500.0], ValueKind::Credits, true),
            "Topped up since yesterday"
        );
        assert_eq!(
            day_takeaway(&[500.0, 400.0], ValueKind::Credits, true),
            "Down 20% since yesterday"
        );
        assert_eq!(day_takeaway(&[0.4], ValueKind::Percent, false), "Flat since yesterday");
        assert_eq!(day_caption(None), "Share of the tightest limit");
        assert_eq!(day_caption(Some("Credits left")), "Credits left at the end of each day");
    }

    /// #217: the charts draw straight segments, which never leave the range of their own points, so a line stays
    /// in its plot exactly when every point is inside the axis range. A reset's sharp drop to near zero, a peak at
    /// the whole allowance and a balance gone negative all have to fit.
    #[test]
    fn every_trend_point_sits_inside_its_axis() {
        use codexbar_core::Currency;
        let usd = ValueKind::Money(Currency::Usd);
        let cases: [(&[f64], ValueKind); 4] = [
            (&[0.93, 0.97, 0.03, 0.12, 0.22, 0.32, 0.41], ValueKind::Percent),
            (&[0.0, 1.0, 0.0, 1.0], ValueKind::Percent),
            (&[12.0, -4.5, 3.0, 0.0], usd),
            (&[40.0, 0.0, 37.0], ValueKind::Count),
        ];
        for (values, kind) in cases {
            let (low, high) = day_domain(values, kind);
            for value in values {
                assert!(
                    (low..=high).contains(value),
                    "{value} outside {low}..={high} for {kind:?}"
                );
            }
        }
    }

    #[test]
    fn the_day_axis_fits_the_unit_and_labels_whole_counts() {
        use codexbar_core::Currency;
        let usd = ValueKind::Money(Currency::Usd);
        assert_eq!(
            day_domain(&[0.2, 1.0], ValueKind::Percent),
            (0.0, SHARE_TOP),
            "a share is pinned, with headroom over 100% (#200)"
        );
        assert_eq!(day_ticks((0.0, SHARE_TOP), ValueKind::Percent), SHARE_TICKS);
        assert_eq!(share_tick(1.0), "100%");
        assert_eq!(share_tick(1.000_000_1), "100%", "a tick read back from its pixel row");
        assert_eq!(share_tick(0.75), "75%");
        assert_eq!(share_tick(SHARE_TOP), "", "the headroom gridline carries no label");
        // The fallback share trend (no window curve) labels like the other share charts; a count keeps its unit.
        assert_eq!(trend_tick(ValueKind::Percent, false, SHARE_TOP), "");
        assert_eq!(trend_tick(ValueKind::Percent, false, 1.0), "100%");
        assert_eq!(
            trend_tick(ValueKind::Percent, true, SHARE_TOP),
            ValueKind::Percent.value(SHARE_TOP)
        );
        assert_eq!(day_domain(&[0.0, 0.0], ValueKind::Count), (0.0, 1.0), "never flat");
        assert_eq!(
            day_domain(&[10.0, 20.0], ValueKind::Credits),
            (0.0, 23.0),
            "headroom over the week"
        );
        // A balance that went negative (#104) keeps its points on the plot.
        let (low, high) = day_domain(&[-4.0, -2.0], usd);
        assert!(
            (low - -4.6).abs() < 1e-9 && high == 0.0,
            "all negative keeps the zero line: {low} {high}"
        );
        let (low, high) = day_domain(&[-4.0, 6.0], usd);
        assert!((low - -4.6).abs() < 1e-9 && (high - 6.9).abs() < 1e-9, "{low} {high}");
        // Ticks are as many as the unit's formatting tells apart: whole counts, credits down to a fraction, money.
        let ticks = |values: &[f64], kind: ValueKind| day_ticks(day_domain(values, kind), kind);
        assert_eq!(ticks(&[0.0, 1.0], ValueKind::Count), 2);
        assert_eq!(ticks(&[2.0, 1.0], ValueKind::Count), 3);
        assert_eq!(ticks(&[37.0], ValueKind::Count), 4);
        assert_eq!(ticks(&[1.0], ValueKind::Credits), 2);
        assert_eq!(ticks(&[1.1], ValueKind::Credits), 2, "0, 1, 1 would repeat");
        assert_eq!(ticks(&[36_800.0], ValueKind::Credits), 4);
        assert_eq!(ticks(&[0.5, 1.0], ValueKind::Percent), SHARE_TICKS);
        assert_eq!(ticks(&[1.0], usd), 4);
    }

    #[test]
    fn a_trend_is_charted_in_its_headlines_own_unit() {
        use codexbar_core::{AccountId, Currency, Money, Provider};
        let account =
            |metric: Metric| AccountSnapshot::new(AccountId::new("a"), Provider::GitHubBilling, vec![metric], now());
        let spend = account(Metric::Spend {
            label: "Net spend".into(),
            spent: Money::from_cents(2389),
            limit: None,
            resets_at: None,
            pace: None,
        });
        assert_eq!(trend_kind(&spend), ValueKind::Money(Currency::Usd));
        assert_eq!(counted_name(&spend).as_deref(), Some("Net spend"));
        assert_eq!(trend_kind(&spend).value(23.89), "$23.89");
        let reviews = account(Metric::Activity {
            label: "Completed reviews".into(),
            count: 3,
        });
        assert_eq!(trend_kind(&reviews), ValueKind::Count);
        let window = account(window(0.4, 0.1));
        assert_eq!(trend_kind(&window), ValueKind::Percent);
        assert_eq!(counted_name(&window), None, "a fraction keeps the pressure chart");
    }
}
