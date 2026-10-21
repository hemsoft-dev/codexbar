//! One bar for a limit (#197), drawn as the Pi dashboard's usage runway (#234): every bar shares one scale, 0 to 125% of
//! the plan limit, with the limit marked at 100%. The share used is the solid fill; the share projected by the reset
//! runs on from it hatched up to the limit (#213) and past the marker in the danger color, so an overrun shows how
//! far it goes; one past 125% fades out at the end. A tick marks where the projection ends. The table, the focus card
//! and the ring gauge all describe their metric through [`Projection`], so the hover text and the accessibility label
//! read the same everywhere: the share used, the share at the reset and, when the limit comes first, when it runs out.

use chrono::{DateTime, Duration, Utc};
use codexbar_core::{Metric, Outlook, Pace, format};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, linear_color_stop, linear_gradient, pattern_slash,
    prelude::FluentBuilder as _, px, relative,
};

/// The end of every bar's scale, as a share of the limit (#234): the Pi dashboard's 125%, so a projected overrun has
/// room to show past the limit and overruns line up down a column.
pub const SCALE_MAX: f64 = 1.25;
/// The empty track past the projection: the bar's color, faint (#213 measured the old faded run at 1.4:1 against it).
const TRACK_OPACITY: f32 = 0.15;
/// The projected run is hatched in the bar's own color (#213): the stripes' gaps show the row behind them, so the run
/// reads as a texture apart from the solid share, in both themes, whatever the severity color.
const HATCH_STRIPE: f32 = 3.;
const HATCH_GAP: f32 = 3.;
/// The tick where the projection ends sticks out this far above and below the bar, so it reads even where the run is
/// a pixel wide, and it is this wide.
const TICK_OVERHANG: f32 = 2.;
const TICK_WIDTH: f32 = 2.;
/// The limit marker stands this far above and below the bar, a hairline in the muted text color, as the Pi
/// dashboard's does.
const LIMIT_OVERHANG: f32 = 4.;
const LIMIT_WIDTH: f32 = 1.;
/// A projection past the scale's end fades out over this last share of the bar, the overflow indication.
const FADE: f32 = 0.08;
/// The run past the limit is the danger color, translucent like the Pi dashboard's ember run: solid enough to read as
/// red beside the hatched run before the marker, light enough that the tick at its end stands out.
const OVER_OPACITY: f32 = 0.55;
/// The shares the runway grades a bar at (#234), as the Pi dashboard does: tight from 85%, out at 100% used.
const TIGHT: f64 = 0.85;

/// Where `share` of the limit sits on the bar, 0 to 1 of its width.
pub fn position(share: f64) -> f32 {
    (share.clamp(0.0, SCALE_MAX) / SCALE_MAX) as f32
}

/// How a bar stands against its limit, graded as the Pi dashboard's runway board does (#234). It picks the bar's color
/// and the forecast's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Runway {
    /// Comfortably under the limit.
    Ok,
    /// 85% or more used, or projected to reach 85% by the reset.
    Tight,
    /// Projected past the limit before the reset.
    Risk,
    /// The limit is used up.
    Out,
}

impl Runway {
    pub fn of(used: f64, projected: Option<f64>) -> Self {
        let projected = projected.filter(|share| share.is_finite());
        if used >= 1.0 {
            Self::Out
        } else if projected.is_some_and(|share| share > 1.0) {
            Self::Risk
        } else if used >= TIGHT || projected.is_some_and(|share| share >= TIGHT) {
            Self::Tight
        } else {
            Self::Ok
        }
    }

    /// The grade for a bar with no limit behind it (a balance, or an account's pressure): its severity's colors, as
    /// the bar had before the runway (#203), so a balance with days of credit left is never drawn used up.
    pub fn of_severity(severity: codexbar_core::Severity) -> Self {
        match severity {
            codexbar_core::Severity::LimitSoon => Self::Out,
            codexbar_core::Severity::AtRisk | codexbar_core::Severity::Watch => Self::Tight,
            codexbar_core::Severity::Normal => Self::Ok,
        }
    }

    /// The solid fill's color: the bar's own calm color when it is fine, the warning color when tight or at risk, the
    /// danger color once the limit is used up. The run past the limit is always in the danger color.
    pub fn color(self, calm: Hsla, cx: &App) -> Hsla {
        match self {
            Self::Ok => calm,
            Self::Tight | Self::Risk => cx.theme().warning,
            Self::Out => cx.theme().danger,
        }
    }
}

/// Where a bar's parts go, as shares of its width on the 0 to 125% scale (#234): the solid share, the hatched run from
/// it up to the limit, the run past the limit marker, the faint track after that, the tick at the projection's end and
/// the marker itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarLayout {
    /// The solid fill's width; at most the limit's position.
    pub used: f32,
    /// The hatched run under the limit, as start and end, when the pace projects past the solid share.
    pub run: Option<(f32, f32)>,
    /// The run past the limit marker, in the danger color, when the projection overruns the limit.
    pub over: Option<(f32, f32)>,
    /// The projection runs past the scale's end: the overrun fades out there instead of ending in a tick.
    pub clipped: bool,
    /// Where the faint track starts; it runs to the end. A plain bar keeps it whole, under its pill end.
    pub track_from: f32,
    /// The tick at the projected share, and whether the projection runs past the limit.
    pub tick: Option<Tick>,
    /// The limit marker's position: 100% of the limit, 0.8 of the bar. `None` on a bar with no limit behind it.
    pub limit: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    /// 0 to 1: the tick's centre.
    pub at: f32,
    /// The pace runs the limit out before the reset: the tick is in the danger color.
    pub past_limit: bool,
}

impl BarLayout {
    /// The parts for a share used and a projection, both as fractions of the limit. A projection at or below the
    /// solid share draws nothing past it; one past the limit continues beyond the marker, clipped at 125%.
    pub fn of(used: f64, projected: Option<f64>) -> Self {
        let used = used.clamp(0.0, 1.0);
        let used_at = position(used);
        let limit = Some(position(1.0));
        let projected = projected.filter(|share| share.is_finite() && *share > used);
        let Some(share) = projected else {
            return Self {
                used: used_at,
                run: None,
                over: None,
                clipped: false,
                track_from: 0.,
                tick: None,
                limit,
            };
        };
        let end = position(share);
        let under = position(share.min(1.0));
        let clipped = share > SCALE_MAX;
        Self {
            used: used_at,
            run: (under > used_at).then_some((used_at, under)),
            over: (share > 1.0).then(|| (position(used.max(1.0)), end)),
            clipped,
            track_from: end,
            tick: (!clipped).then_some(Tick {
                at: end,
                past_limit: share > 1.0,
            }),
            limit,
        }
    }

    /// A bar with no limit behind it (#234): a credit pool's spend, a balance's or an account's pressure. It runs the
    /// whole width at 100%, as its percentage says, with no marker and nothing projected.
    pub fn plain(used: f64) -> Self {
        Self {
            used: used.clamp(0.0, 1.0) as f32,
            run: None,
            over: None,
            clipped: false,
            track_from: 0.,
            tick: None,
            limit: None,
        }
    }
}

/// Where the pace lands by the reset and, when the limit comes first, when it runs out (#234), worded as the Pi
/// dashboard's runway board words it: "112% at reset · runs out in 5d 4h".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    /// The share used by the reset, past 1 when the pace overruns the limit.
    pub at_reset: f64,
    /// The share used now.
    pub used: f64,
    /// How long until the limit runs out, when that comes before the reset.
    pub runs_out_in: Option<Duration>,
    /// How long before the reset that is (#206).
    pub early: Option<Duration>,
}

impl Forecast {
    /// The forecast for `metric` at `now`, when its pace allows one.
    pub fn of(metric: &Metric, now: DateTime<Utc>) -> Option<Self> {
        let projected = metric.projected_at_reset(now)?;
        let used = metric.used_fraction()?;
        // A limit hit within a minute of the reset is a full share, not an overrun (#206), so it reads "100% at reset"
        // and grades as tight rather than at risk.
        let (at_reset, runs_out_in, early) = match metric.outlook(now) {
            Some(Outlook::Early(early)) if used < 1.0 => (projected, metric.time_to_limit(), Some(early)),
            Some(Outlook::Share(_)) if used < 1.0 => (projected.min(1.0), None, None),
            _ => (projected, None, None),
        };
        Some(Self {
            at_reset,
            used,
            runs_out_in,
            early,
        })
    }

    /// How the bar stands: out, at risk, tight or fine.
    pub fn runway(&self) -> Runway {
        Runway::of(self.used, Some(self.at_reset))
    }

    /// "112%": the share at the reset.
    pub fn share(&self) -> String {
        format!("{:.0}%", self.at_reset.max(0.0) * 100.0)
    }

    /// What the runway says beside the share: "runs out in 5d 4h", "limit reached", "near limit", or nothing.
    pub fn phrase(&self) -> Option<String> {
        match self.runway() {
            Runway::Out => Some("limit reached".to_owned()),
            Runway::Risk => Some(match self.runs_out_in {
                Some(left) if left >= Duration::minutes(1) => format!("runs out in {}", format::countdown(left)),
                Some(_) => "at the limit now".to_owned(),
                None => "runs out at the reset".to_owned(),
            }),
            Runway::Tight => Some("near limit".to_owned()),
            Runway::Ok => None,
        }
    }

    /// The whole forecast: "112% at reset · runs out in 5d 4h", "56% at reset".
    pub fn text(&self) -> String {
        match self.phrase() {
            Some(phrase) => format!("{} at reset · {phrase}", self.share()),
            None => format!("{} at reset", self.share()),
        }
    }
}

/// What a used-up window the account's credits carry says (#150): it neither runs out nor is reached.
pub const COVERED: &str = "covered by credits";
const REACHED: &str = "limit reached";

/// Whether `metric` is a limit used up: a window, quota or capped spend at 100%.
fn used_up(metric: &Metric) -> bool {
    matches!(
        metric,
        Metric::Window { .. } | Metric::Quota { .. } | Metric::Spend { limit: Some(_), .. }
    ) && metric.used_fraction().is_some_and(|used| used >= 1.0)
}

/// What `account`'s `metric` says about its reset (#234), in the words every surface uses (the focus card's tiles and
/// bar, the widgets): the forecast ("112% at reset · runs out in 5d 4h"), "covered by credits" for a used-up window
/// the account's credits carry (#150), "limit reached" for a used-up limit with no pace left, or nothing.
pub fn outlook(metric: &Metric, account: &codexbar_core::AccountSnapshot, now: DateTime<Utc>) -> Option<String> {
    if account.credits_cover(metric) {
        return Some(COVERED.to_owned());
    }
    Forecast::of(metric, now)
        .map(|forecast| forecast.text())
        .or_else(|| used_up(metric).then(|| REACHED.to_owned()))
}

/// What a bar says about its metric: the share used, the share projected by the reset when the pace allows one, and
/// the words for both.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// 0 to 1, what the solid fill shows.
    pub used: f64,
    /// 0 up, past 1 when the limit runs out before the reset; `None` with nothing to project.
    pub projected: Option<f64>,
    /// How the bar stands, which picks its color.
    pub runway: Runway,
    /// Whether the bar stands for a limit: drawn on the runway's 0 to 125% scale with the limit marked. A credit pool,
    /// a balance or an account's pressure isn't one and runs the plain 0 to 100% width.
    pub limited: bool,
    /// "42% used, 78% at reset", "91% used, 112% at reset · runs out in 25m" (#234), or "42% used" with nothing
    /// projected: the accessibility label.
    pub label: String,
    /// The hover text: the share, when the window resets, and the forecast with its pace, or why there is none.
    pub tooltip: String,
}

impl Projection {
    /// What `metric` shows at `now`, with `used` as the bar's own share (a credits bar runs on the spend since the
    /// remembered start, not the metric's used fraction, so the caller says what the fill is).
    pub fn of(metric: &Metric, used: f64, now: DateTime<Utc>) -> Self {
        let share = format!("{:.0}% used", used * 100.0);
        let resets = metric
            .resets_at()
            .map(|at| format!("resets {}", format::time_left_label(at, now)));
        let pace = match metric {
            Metric::Window { pace, .. } | Metric::Quota { pace, .. } | Metric::Spend { pace, .. } => *pace,
            Metric::Balance { .. } | Metric::Activity { .. } | Metric::Credits { .. } => None,
        };
        // The share at the reset and when the limit runs out (#234), with how early that is (#206) and the pace. The
        // bar draws the same share the words say, so a limit hit at the reset is a full bar, not an overrun.
        let forecast = Forecast::of(metric, now);
        let projected = forecast.map(|forecast| forecast.at_reset);
        let outlook = match forecast {
            Some(forecast) => {
                let mut told = forecast.text();
                if let Some(early) = forecast.early {
                    told = format!("{told}, {} early", format::time_left(early));
                }
                match pace {
                    Some(pace) => format!("{told} ({})", pace_text(pace)),
                    None => told,
                }
            }
            None if used_up(metric) => REACHED.to_owned(),
            None => no_projection(metric, pace, now).to_owned(),
        };
        let label = match forecast {
            Some(forecast) => format!("{share}, {}", forecast.text()),
            None if used_up(metric) => format!("{share}, {REACHED}"),
            None => share.clone(),
        };
        let tooltip = [Some(share), resets, Some(outlook)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        Self {
            used,
            projected,
            // Credits have no limit to run into (#150): their bar runs on the spend since the last top-up and stays
            // calm, as `codexbar_core::assess` keeps them.
            limited: matches!(
                metric,
                Metric::Window { .. } | Metric::Quota { .. } | Metric::Spend { limit: Some(_), .. }
            ),
            runway: match metric {
                Metric::Credits { .. } => Runway::Ok,
                // A balance has no limit: its bar runs on the pressure of its days left, graded by its severity.
                Metric::Balance { .. } | Metric::Activity { .. } => {
                    Runway::of_severity(codexbar_core::assess(metric, now).severity())
                }
                _ => Runway::of(used, projected),
            },
            label,
            tooltip,
        }
    }

    /// A bar with no metric behind it (an account's pressure): the share alone.
    pub fn bare(used: f64) -> Self {
        let share = format!("{:.0}% used", used * 100.0);
        Self {
            used,
            projected: None,
            runway: Runway::of(used, None),
            limited: false,
            label: share.clone(),
            tooltip: share,
        }
    }

    /// The same bar drawn calm whatever its share: a used-up window the account's credits carry it past (#150). The
    /// credits carry the pace past the limit too, so nothing is drawn past the marker.
    pub fn calm(mut self) -> Self {
        self.runway = Runway::Ok;
        self.projected = self.projected.map(|share| share.min(1.0));
        // The words say so too: the reset stays, the forecast gives way to the credits.
        let share = format!("{:.0}% used", self.used * 100.0);
        let resets = self
            .tooltip
            .split(" · ")
            .find(|part| part.starts_with("resets "))
            .map(str::to_owned);
        self.label = format!("{share}, {COVERED}");
        self.tooltip = [Some(share), resets, Some(COVERED.to_owned())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        self
    }

    /// The bar for `account`'s `metric`: calm when the account's credits carry it (#150).
    pub fn of_account(
        metric: &Metric,
        used: f64,
        account: &codexbar_core::AccountSnapshot,
        now: DateTime<Utc>,
    ) -> Self {
        let projection = Self::of(metric, used, now);
        if account.credits_cover(metric) {
            projection.calm()
        } else {
            projection
        }
    }

    /// The same bar graded by `severity` rather than as a limit: an account's pressure has no limit to run into.
    pub fn graded(mut self, severity: codexbar_core::Severity) -> Self {
        self.runway = Runway::of_severity(severity);
        self
    }
}

/// Why a metric projects nothing, for the tooltip: each reason is told from the metric's own state, so a window
/// that is merely young is the only one called early.
fn no_projection(metric: &Metric, pace: Option<Pace>, now: DateTime<Utc>) -> &'static str {
    match metric {
        Metric::Credits { .. } => "Credits don't project: the bar runs on the spend since the last top-up",
        Metric::Balance { .. } | Metric::Activity { .. } => "Nothing to project",
        Metric::Spend { limit: None, .. } => "No spend limit to project against",
        Metric::Window { .. } | Metric::Quota { .. } | Metric::Spend { .. } => {
            match (metric.resets_at(), pace) {
                (Some(at), _) if at <= now => "The reset is due; nothing left to project",
                // A spend's pace needs the period's start, which some providers never say (Cursor).
                (_, None) if matches!(metric, Metric::Spend { .. }) => "No pace is known for this spend",
                (_, None) => "Too early in the window to project",
                (None, Some(_)) => "No reset to project to",
                (Some(_), Some(_)) => "Nothing to project",
            }
        }
    }
}

/// A pace in the unit that keeps its figure readable: per hour from 0.1%/h up, else per day, with a second decimal
/// for a monthly trickle, so the rate never reads as zero beside a projection it produced.
fn pace_text(pace: Pace) -> String {
    let per_hour = pace.fraction_per_hour() * 100.0;
    if per_hour >= 0.1 {
        format!("{per_hour:.1}%/h")
    } else if per_hour * 24.0 >= 0.1 {
        format!("{:.1}%/day", per_hour * 24.0)
    } else {
        format!("{:.2}%/day", per_hour * 24.0)
    }
}

/// The bar: `height` tall, on the 0 to 125% scale with the limit marked at 100% (#234). The solid share is colored by
/// its [`Runway`] (`calm` while it is fine), the projected run hatched in that color up to the limit and a translucent
/// run in the danger color past it, and a tick in the text color where the projection ends (#213), in the danger color past the limit. A
/// projection past 125% fades out at the end instead. The tooltip shows on hover and `projection.label` serves screen
/// readers and the headless tests. `id` names the hoverable cell; the bar itself is `<id>-bar`, the tick
/// `<id>-projected`, the overrun `<id>-over` and the marker `<id>-limit`.
pub fn usage_bar(
    id: &str,
    projection: &Projection,
    calm: Hsla,
    height: Pixels,
    spoken_name: impl Into<SharedString>,
    cx: &App,
) -> AnyElement {
    let layout = if projection.limited {
        BarLayout::of(projection.used, projection.projected)
    } else {
        BarLayout::plain(projection.used)
    };
    let color = projection.runway.color(calm, cx);
    let tooltip: SharedString = projection.tooltip.clone().into();
    let spoken: SharedString = spoken_name.into();
    let (text, danger, muted) = (cx.theme().foreground, cx.theme().danger, cx.theme().muted_foreground);
    // Each part rounds its own outer ends: GPUI clips children to a rectangle, not to the parent's rounded corners.
    // The bar's left end belongs to whichever part starts at 0, its right end to whichever reaches 1, and a run past
    // the solid share starts square so the texture begins at a clean edge.
    let radius = height / 2.;
    let span = |from: f32, to: f32| {
        let part = div()
            .absolute()
            .top_0()
            .h_full()
            .left(relative(from))
            .w(relative((to - from).max(0.)));
        let part = if from <= 0. { part.rounded_l(radius) } else { part };
        if to >= 1. { part.rounded_r(radius) } else { part }
    };
    let solid = span(0., layout.used).bg(color);
    let solid = if layout.run.is_some() || layout.over.is_some() {
        solid
    } else {
        solid.rounded(radius)
    };
    // Past the scale's end the overrun stops hatched short of it and fades out over the last stretch.
    let over = layout.over.map(|(from, to)| {
        let fade_from = if layout.clipped { (to - FADE).max(from) } else { to };
        (from, fade_from, to)
    });
    div()
        .id(ElementId::Name(id.to_owned().into()))
        .test_support()
        .aria_label(projection.label.clone())
        .size_full()
        .flex()
        .items_center()
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(
            div()
                .id(ElementId::Name(format!("{id}-bar").into()))
                .role(Role::ProgressIndicator)
                .aria_label(format!("{spoken}: {}", projection.label))
                .aria_min_numeric_value(0.)
                .aria_max_numeric_value(100.)
                .aria_numeric_value(projection.used.clamp(0.0, 1.0) * 100.0)
                .test_support()
                .relative()
                .w_full()
                .h(height)
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .overflow_hidden()
                        .child(span(layout.track_from, 1.).bg(color.opacity(TRACK_OPACITY)))
                        .when_some(layout.run, |this, (from, to)| {
                            this.child(span(from, to).bg(pattern_slash(color, HATCH_STRIPE, HATCH_GAP)))
                        })
                        .when_some(over, |this, (from, fade_from, to)| {
                            this.child(
                                span(from, fade_from)
                                    .id(ElementId::Name(format!("{id}-over").into()))
                                    .test_support()
                                    .bg(danger.opacity(OVER_OPACITY)),
                            )
                            .when(layout.clipped, |this| {
                                this.child(span(fade_from, to).bg(linear_gradient(
                                    90.,
                                    linear_color_stop(danger.opacity(OVER_OPACITY), 0.),
                                    linear_color_stop(danger.opacity(0.), 1.),
                                )))
                            })
                        })
                        .child(solid),
                )
                // The limit marker and the tick stand outside the clipped bar, so they stand proud of it.
                .when_some(layout.limit, |this, limit| {
                    this.child(
                        div()
                            .id(ElementId::Name(format!("{id}-limit").into()))
                            .test_support()
                            .absolute()
                            .top(px(-LIMIT_OVERHANG))
                            .left(relative(limit))
                            .ml(px(-LIMIT_WIDTH / 2.))
                            .w(px(LIMIT_WIDTH))
                            .h(height + px(2. * LIMIT_OVERHANG))
                            .bg(muted.opacity(0.7)),
                    )
                })
                .when_some(layout.tick, |this, tick| {
                    let tint = if tick.past_limit { danger } else { text };
                    this.child(
                        div()
                            .id(ElementId::Name(format!("{id}-projected").into()))
                            .test_support()
                            .absolute()
                            .top(px(-TICK_OVERHANG))
                            .left(relative(tick.at))
                            .ml(px(-TICK_WIDTH / 2.))
                            .w(px(TICK_WIDTH))
                            .h(height + px(2. * TICK_OVERHANG))
                            .rounded(px(1.))
                            .bg(tint),
                    )
                }),
        )
        .into_any_element()
}

/// The height the table's bars have: tall enough for the hatched run to read (#213), with room in the row for the
/// tick above and below.
pub fn small() -> Pixels {
    px(8.)
}

/// The height the focus card's bar has, as the stock medium progress bar.
pub fn medium() -> Pixels {
    px(8.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn window(used: f64, pace: Option<Pace>) -> Metric {
        Metric::Window {
            label: "Weekly".into(),
            used,
            resets_at: now() + Duration::days(2),
            pace,
            length: None,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    /// The scale (#234): 0 to 125% of the limit, so the limit sits at 0.8 of the bar and 125% at its end.
    #[test]
    fn every_bar_shares_one_scale_with_the_limit_at_four_fifths() {
        assert!(close(position(0.0), 0.0));
        assert!(close(position(0.5), 0.4));
        assert!(close(position(1.0), 0.8));
        assert!(close(position(1.25), 1.0));
        assert!(close(position(3.0), 1.0), "past the end clips");
        assert!(close(BarLayout::of(0.3, None).limit.unwrap(), 0.8));
    }

    /// Under the limit (#213): solid to the share used, hatched run to the projection, track after, tick at the end
    /// of the run, nothing past the marker.
    #[test]
    fn a_projection_under_the_limit_runs_to_its_tick_and_leaves_the_track() {
        let layout = BarLayout::of(0.42, Some(0.9));
        assert!(close(layout.used, 0.336));
        let (from, to) = layout.run.unwrap();
        assert!(close(from, 0.336) && close(to, 0.72));
        assert_eq!(layout.over, None);
        assert!(!layout.clipped);
        assert!(close(layout.track_from, 0.72));
        let tick = layout.tick.unwrap();
        assert!(close(tick.at, 0.72));
        assert!(!tick.past_limit);
    }

    /// Exactly at the limit is not past it: the run ends at the marker, the tick is the ordinary one, no overrun.
    #[test]
    fn a_projection_at_the_limit_ends_at_the_marker_without_the_warning() {
        let layout = BarLayout::of(0.5, Some(1.0));
        assert_eq!(layout.run, Some((0.4, 0.8)));
        assert_eq!(layout.over, None);
        assert_eq!(
            layout.tick,
            Some(Tick {
                at: 0.8,
                past_limit: false
            })
        );
    }

    /// Between 100% and 125% (#234): hatched to the marker, the overrun past it to the projection, the tick there in
    /// the danger color, the track after it.
    #[test]
    fn a_projection_past_the_limit_runs_on_beyond_the_marker() {
        let layout = BarLayout::of(0.17, Some(1.12));
        assert!(close(layout.used, 0.136));
        let (from, to) = layout.run.unwrap();
        assert!(close(from, 0.136) && close(to, 0.8));
        let (from, to) = layout.over.unwrap();
        assert!(close(from, 0.8) && close(to, 0.896));
        assert!(!layout.clipped);
        assert!(close(layout.track_from, 0.896));
        let tick = layout.tick.unwrap();
        assert!(close(tick.at, 0.896) && tick.past_limit);
    }

    /// Past 125%: the overrun reaches the end and is clipped there, with no tick (it fades out instead); the solid
    /// share stays where it is.
    #[test]
    fn a_projection_past_the_scale_is_clipped_at_the_end() {
        let layout = BarLayout::of(0.91, Some(3.31));
        assert!(close(layout.used, 0.728));
        let (_, to) = layout.run.unwrap();
        assert!(close(to, 0.8));
        assert_eq!(layout.over, Some((0.8, 1.0)));
        assert!(layout.clipped);
        assert_eq!(layout.track_from, 1.0, "no track left");
        assert_eq!(layout.tick, None);
    }

    /// A used-up limit still on pace: the solid fill reaches the marker and the overrun starts there.
    #[test]
    fn an_exhausted_limit_fills_to_the_marker() {
        let layout = BarLayout::of(1.0, Some(1.1));
        assert!(close(layout.used, 0.8));
        assert_eq!(layout.run, None);
        let (from, to) = layout.over.unwrap();
        assert!(close(from, 0.8) && close(to, 0.88));
        let plain = BarLayout::of(1.0, None);
        assert!(close(plain.used, 0.8) && plain.over.is_none());
    }

    /// A projection a point ahead still gets its tick at the true share, neither moved nor widened.
    #[test]
    fn a_projection_a_point_ahead_still_gets_its_tick() {
        let layout = BarLayout::of(0.53, Some(0.54));
        let tick = layout.tick.unwrap();
        assert!(close(tick.at, 0.432));
        assert!(close(layout.used, 0.424));
    }

    /// Nothing projected, or a projection at or behind the share used: a plain bar with no run, no overrun and no tick,
    /// but still the limit marker.
    #[test]
    fn no_projection_draws_a_plain_bar() {
        for projected in [None, Some(0.42), Some(0.3), Some(f64::NAN)] {
            let layout = BarLayout::of(0.42, projected);
            assert_eq!(layout.run, None, "{projected:?}");
            assert_eq!(layout.over, None, "{projected:?}");
            assert_eq!(layout.tick, None, "{projected:?}");
            assert_eq!(layout.track_from, 0., "the whole track, under the pill end");
            assert!(close(layout.limit.unwrap(), 0.8));
        }
    }

    /// The Pi dashboard's grades (#234): out at 100% used, at risk when projected past 100%, tight from 85% used or
    /// projected, fine otherwise.
    #[test]
    fn the_runway_grades_like_the_pi_dashboard() {
        assert_eq!(Runway::of(1.0, Some(1.4)), Runway::Out);
        assert_eq!(Runway::of(1.0, None), Runway::Out);
        assert_eq!(Runway::of(0.17, Some(1.12)), Runway::Risk);
        assert_eq!(
            Runway::of(0.5, Some(1.0)),
            Runway::Tight,
            "exactly the limit is tight, not at risk"
        );
        assert_eq!(Runway::of(0.86, None), Runway::Tight);
        assert_eq!(Runway::of(0.4, Some(0.85)), Runway::Tight);
        assert_eq!(Runway::of(0.4, Some(0.84)), Runway::Ok);
        assert_eq!(Runway::of(0.4, None), Runway::Ok);
    }

    /// The Pi dashboard's example (#234): 17% used, on pace for 112% with five days left.
    #[test]
    fn an_overrun_says_its_share_at_reset_and_when_it_runs_out() {
        let weekly = Metric::Window {
            label: "Weekly".into(),
            used: 0.17,
            resets_at: now() + Duration::hours(142),
            pace: Some(Pace::per_hour(0.95 / 142.0)),
            length: None,
        };
        let forecast = Forecast::of(&weekly, now()).unwrap();
        assert_eq!(forecast.runway(), Runway::Risk);
        // 83% left at 0.669%/h: about 124 hours, 5d 4h.
        assert_eq!(forecast.text(), "112% at reset · runs out in 5d 4h");
        let projection = Projection::of(&weekly, 0.17, now());
        assert_eq!(projection.runway, Runway::Risk);
        assert_eq!(projection.label, "17% used, 112% at reset · runs out in 5d 4h");
        assert!(
            projection
                .tooltip
                .starts_with("17% used · resets in 6d · 112% at reset · runs out in 5d 4h, 17h 56m early ("),
            "{}",
            projection.tooltip
        );
    }

    /// The limit hit within a minute of the reset (#206): a full share, tight, never "runs out" at the reset.
    #[test]
    fn a_limit_hit_at_the_reset_is_a_full_share_not_an_overrun() {
        // 0.5%/h with 10% left and the reset 20h 0m 30s away: the limit comes 30s early.
        let metric = Metric::Window {
            label: "Weekly".into(),
            used: 0.9,
            resets_at: now() + Duration::seconds(20 * 3600 + 30),
            pace: Some(Pace::per_hour(0.005)),
            length: None,
        };
        let forecast = Forecast::of(&metric, now()).unwrap();
        assert_eq!(forecast.runway(), Runway::Tight);
        assert_eq!(forecast.text(), "100% at reset · near limit");
    }

    /// Used up with no pace left: the label and tooltip say the limit is reached, not that it is too early.
    #[test]
    fn a_used_up_limit_without_a_pace_says_it_is_reached() {
        let projection = Projection::of(&window(1.0, None), 1.0, now());
        assert_eq!(projection.label, "100% used, limit reached");
        assert_eq!(projection.tooltip, "100% used · resets in 2d · limit reached");
    }

    #[test]
    fn a_used_up_limit_says_so() {
        let forecast = Forecast::of(&window(1.0, Some(Pace::per_hour(0.01))), now()).unwrap();
        assert_eq!(forecast.runway(), Runway::Out);
        assert_eq!(forecast.text(), "148% at reset · limit reached");
    }

    #[test]
    fn a_metric_with_a_pace_projects_and_says_so() {
        // 1%/h over 48 hours left: 35% + 48% = 83%.
        let projection = Projection::of(&window(0.35, Some(Pace::per_hour(0.01))), 0.35, now());
        assert_eq!(projection.projected.map(|p| (p * 100.0).round()), Some(83.0));
        assert_eq!(projection.runway, Runway::Ok);
        assert_eq!(projection.label, "35% used, 83% at reset");
        assert_eq!(projection.tooltip, "35% used · resets in 2d · 83% at reset (1.0%/h)");
    }

    #[test]
    fn a_window_too_young_to_project_says_why() {
        let projection = Projection::of(&window(0.35, None), 0.35, now());
        assert_eq!(projection.projected, None);
        assert_eq!(projection.label, "35% used");
        assert_eq!(
            projection.tooltip,
            "35% used · resets in 2d · Too early in the window to project"
        );
    }

    #[test]
    fn a_projection_past_the_limit_says_how_early_it_runs_out() {
        // 5%/h with 9% left: the limit in 1h 48m, the reset in two days (#206).
        let projection = Projection::of(&window(0.91, Some(Pace::per_hour(0.05))), 0.91, now());
        assert!(projection.projected.unwrap() > 1.0, "the overrun runs past the marker");
        assert_eq!(projection.label, "91% used, 331% at reset · runs out in 1h 48m");
        assert_eq!(
            projection.tooltip,
            "91% used · resets in 2d · 331% at reset · runs out in 1h 48m, 1d 22h early (5.0%/h)"
        );
    }

    #[test]
    fn a_slow_pace_reads_per_day_so_it_never_shows_as_zero() {
        // 0.04%/h over 48h: on pace for 37%, told as 0.96%/day rather than 0.0%/h.
        let projection = Projection::of(&window(0.35, Some(Pace::per_hour(0.0004))), 0.35, now());
        assert!(projection.tooltip.ends_with("(1.0%/day)"), "{}", projection.tooltip);
        let trickle = Projection::of(&window(0.35, Some(Pace::per_hour(0.00002))), 0.35, now());
        assert!(trickle.tooltip.ends_with("(0.05%/day)"), "{}", trickle.tooltip);
    }

    #[test]
    fn each_reason_for_no_projection_is_its_own() {
        let spend = Metric::Spend {
            label: "On-demand".into(),
            spent: codexbar_core::Money::from_cents(1200),
            limit: Some(codexbar_core::Money::from_cents(5000)),
            resets_at: Some(now() + Duration::days(10)),
            pace: None,
        };
        assert!(
            Projection::of(&spend, 0.24, now())
                .tooltip
                .ends_with("No pace is known for this spend")
        );
        let uncapped = Metric::Spend {
            label: "Net spend".into(),
            spent: codexbar_core::Money::from_cents(1200),
            limit: None,
            resets_at: Some(now() + Duration::days(10)),
            pace: None,
        };
        assert!(
            Projection::of(&uncapped, 0.0, now())
                .tooltip
                .ends_with("No spend limit to project against")
        );
        let due = Metric::Window {
            label: "Weekly".into(),
            used: 0.5,
            resets_at: now() - Duration::minutes(1),
            pace: Some(Pace::per_hour(0.01)),
            length: None,
        };
        assert!(
            Projection::of(&due, 0.5, now())
                .tooltip
                .ends_with("The reset is due; nothing left to project")
        );
        assert!(
            Projection::of(&window(0.35, None), 0.35, now())
                .tooltip
                .ends_with("Too early in the window to project")
        );
    }

    /// The bar draws the forecast's share: a limit hit within a minute of the reset is a full bar, graded tight.
    #[test]
    fn the_bar_draws_the_share_the_words_say() {
        let metric = Metric::Window {
            label: "Weekly".into(),
            used: 0.9,
            resets_at: now() + Duration::seconds(20 * 3600 + 30),
            pace: Some(Pace::per_hour(0.005)),
            length: None,
        };
        let projection = Projection::of(&metric, 0.9, now());
        assert_eq!(projection.projected, Some(1.0));
        assert_eq!(projection.runway, Runway::Tight);
        assert_eq!(BarLayout::of(projection.used, projection.projected).over, None);
    }

    #[test]
    fn a_balance_bar_is_graded_by_its_severity_not_as_a_used_up_limit() {
        // Four days of credit left: pressure 1.0, but the balance only needs watching.
        let balance = Metric::Balance {
            label: "$ balance".into(),
            remaining: codexbar_core::Money::from_cents(400),
            burn_per_day: Some(codexbar_core::Money::from_cents(100)),
        };
        let severity = codexbar_core::assess(&balance, now()).severity();
        let projection = Projection::of(&balance, 1.0, now());
        assert_eq!(projection.runway, Runway::of_severity(severity));
        assert_ne!(projection.runway, Runway::Out, "{severity:?}");
    }

    /// A used-up window the credits carry (#150) is calm, draws nothing past the marker, and says so.
    #[test]
    fn a_calm_bar_draws_no_overrun() {
        let projection = Projection::of(&window(1.0, Some(Pace::per_hour(0.01))), 1.0, now()).calm();
        assert_eq!(projection.runway, Runway::Ok);
        assert_eq!(projection.label, "100% used, covered by credits");
        assert_eq!(projection.tooltip, "100% used · resets in 2d · covered by credits");
        let layout = BarLayout::of(projection.used, projection.projected);
        assert_eq!((layout.over, layout.tick), (None, None));
    }

    /// A credit pool, a balance or an account's pressure has no limit: its bar runs the whole width at 100%, with
    /// no marker.
    #[test]
    fn a_bar_with_no_limit_runs_the_whole_width() {
        let credits = Metric::Credits {
            label: "Credits".into(),
            balance: Some(0.0),
            start: Some(50_000.),
        };
        let projection = Projection::of(&credits, 1.0, now());
        assert!(!projection.limited);
        assert!(!Projection::bare(0.5).limited);
        assert!(Projection::of(&window(0.5, None), 0.5, now()).limited);
        let layout = BarLayout::plain(0.5);
        assert!(close(layout.used, 0.5));
        assert_eq!(layout.limit, None);
        assert!(close(BarLayout::plain(1.0).used, 1.0));
    }

    #[test]
    fn credits_have_no_projection_and_say_so() {
        let credits = Metric::Credits {
            label: "Credits".into(),
            balance: Some(18_400.),
            start: Some(50_000.),
        };
        let projection = Projection::of(&credits, 0.63, now());
        assert_eq!(projection.projected, None);
        assert_eq!(
            Projection::of(&credits, 1.0, now()).runway,
            Runway::Ok,
            "a spent credit pool stays calm"
        );
        assert_eq!(projection.label, "63% used");
        assert_eq!(
            projection.tooltip,
            "63% used · Credits don't project: the bar runs on the spend since the last top-up"
        );
    }
}
