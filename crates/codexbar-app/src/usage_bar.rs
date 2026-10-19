//! One bar for a limit (#197): the share used as a solid fill, the share projected by the reset as a hatched run past
//! it with a tick where it ends (#213), and a tooltip that says both. The table, the focus card and the ring gauge all
//! describe their metric through [`Projection`], so the hover text and the accessibility label read the same
//! everywhere.

use chrono::{DateTime, Utc};
use codexbar_core::{Metric, Pace, format};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, pattern_slash, prelude::FluentBuilder as _, px,
    relative,
};

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

/// Where a bar's parts go, as shares of its width (#213): the solid share, the hatched run from it to the projection,
/// the faint track after that, and the tick at the projection's end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarLayout {
    /// 0 to 1: the solid fill's width.
    pub used: f32,
    /// The hatched run, as start and end, when the pace projects past the solid share.
    pub run: Option<(f32, f32)>,
    /// Where the faint track starts; it runs to the end. A plain bar keeps it whole, under its pill end.
    pub track_from: f32,
    /// The tick at the projected share, and whether the projection runs past the limit.
    pub tick: Option<Tick>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    /// 0 to 1: the tick's centre.
    pub at: f32,
    /// The pace runs the limit out before the reset: the tick sits at the end in the warning color.
    pub past_limit: bool,
}

impl BarLayout {
    /// The parts for a share used and a projection, both as fractions of the limit. A projection at or below the
    /// solid share draws nothing past it; one past the limit fills the bar and flags the tick.
    pub fn of(used: f64, projected: Option<f64>) -> Self {
        let used_share = used.clamp(0.0, 1.0) as f32;
        let projected = projected.filter(|share| share.is_finite() && *share > used);
        match projected {
            Some(share) => {
                let end = share.clamp(0.0, 1.0) as f32;
                Self {
                    used: used_share,
                    run: (end > used_share).then_some((used_share, end)),
                    track_from: end,
                    tick: Some(Tick {
                        at: end,
                        past_limit: share > 1.0,
                    }),
                }
            }
            None => Self {
                used: used_share,
                run: None,
                track_from: 0.,
                tick: None,
            },
        }
    }
}

/// What a bar says about its metric: the share used, the share projected by the reset when the pace allows one, and
/// the words for both.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// 0 to 1, what the solid fill shows.
    pub used: f64,
    /// 0 up, past 1 when the limit runs out before the reset; `None` with nothing to project.
    pub projected: Option<f64>,
    /// "42% used, on pace for 78%", "91% used, 21m early" (#206), or "42% used" with nothing projected: the
    /// accessibility label.
    pub label: String,
    /// The hover text: the share, when the window resets, and the projection with its pace, or why there is none.
    pub tooltip: String,
}

impl Projection {
    /// What `metric` shows at `now`, with `used` as the bar's own share (a credits bar runs on the spend since the
    /// remembered start, not the metric's used fraction, so the caller says what the fill is).
    pub fn of(metric: &Metric, used: f64, now: DateTime<Utc>) -> Self {
        let projected = metric.projected_at_reset(now);
        let share = format!("{:.0}% used", used * 100.0);
        let resets = metric
            .resets_at()
            .map(|at| format!("resets {}", format::time_left_label(at, now)));
        let pace = match metric {
            Metric::Window { pace, .. } | Metric::Quota { pace, .. } | Metric::Spend { pace, .. } => *pace,
            Metric::Balance { .. } | Metric::Activity { .. } | Metric::Credits { .. } => None,
        };
        // The share by the reset, or how early the limit runs out (#206).
        let outlook = metric.outlook(now);
        let told = outlook.map(|outlook| match outlook {
            codexbar_core::Outlook::Share(_) => format!("{} by the reset", format::outlook_text(outlook)),
            codexbar_core::Outlook::Early(_) => format::outlook_text(outlook),
        });
        let outlook = match (told, pace) {
            (Some(told), Some(pace)) => format!("{told} ({})", pace_text(pace)),
            (Some(told), None) => told,
            (None, _) => no_projection(metric, pace, now).to_owned(),
        };
        let label = match metric.outlook(now) {
            Some(outlook) => format!("{share}, {}", format::outlook_text(outlook)),
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
            label: share.clone(),
            tooltip: share,
        }
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

/// The bar: `height` tall, the solid share in `color`, the projected run hatched in it, the faint track after, and a
/// tick in the text color where the projection ends (#213), in the warning color at the end when the pace runs the
/// limit out first. The tooltip shows on hover and `projection.label` serves screen readers and the headless tests.
/// `id` names the hoverable cell; the bar itself is `<id>-bar` and the tick `<id>-projected`.
pub fn usage_bar(
    id: &str,
    projection: &Projection,
    color: Hsla,
    height: Pixels,
    spoken_name: impl Into<SharedString>,
    cx: &App,
) -> AnyElement {
    let layout = BarLayout::of(projection.used, projection.projected);
    let tooltip: SharedString = projection.tooltip.clone().into();
    let spoken: SharedString = spoken_name.into();
    let (text, danger) = (cx.theme().foreground, cx.theme().danger);
    let span = |this: gpui_kit::Div, from: f32, to: f32| {
        this.absolute()
            .top_0()
            .h_full()
            .left(relative(from))
            .w(relative((to - from).max(0.)))
    };
    // A plain bar keeps its pill end; one with a run past it ends square, so the texture starts at a clean edge.
    let solid = div()
        .absolute()
        .top_0()
        .left_0()
        .h_full()
        .w(relative(layout.used))
        .bg(color);
    let solid = if layout.run.is_some() {
        solid
    } else {
        solid.rounded(height / 2.)
    };
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
                .aria_numeric_value(f64::from(layout.used) * 100.0)
                .test_support()
                .relative()
                .w_full()
                .h(height)
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .rounded(height / 2.)
                        .overflow_hidden()
                        .child(span(div(), layout.track_from, 1.).bg(color.opacity(TRACK_OPACITY)))
                        .when_some(layout.run, |this, (from, to)| {
                            this.child(span(div(), from, to).bg(pattern_slash(color, HATCH_STRIPE, HATCH_GAP)))
                        })
                        .child(solid),
                )
                // Outside the clipped bar, so it stands proud of it and reads at any width of run.
                .when_some(layout.tick, |this, tick| {
                    let (shift, tint) = if tick.past_limit {
                        (px(-TICK_WIDTH), danger)
                    } else {
                        (px(-TICK_WIDTH / 2.), text)
                    };
                    this.child(
                        div()
                            .id(ElementId::Name(format!("{id}-projected").into()))
                            .test_support()
                            .absolute()
                            .top(px(-TICK_OVERHANG))
                            .left(relative(tick.at))
                            .ml(shift)
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
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        "2026-10-07T02:00:00Z".parse().unwrap()
    }

    fn window(used: f64, pace: Option<Pace>) -> Metric {
        Metric::Window {
            label: "Weekly".into(),
            used,
            resets_at: now() + Duration::days(2),
            pace,
        }
    }

    /// Under the limit (#213): solid to the share used, hatched run to the projection, track after, tick at the end
    /// of the run.
    #[test]
    fn a_projection_under_the_limit_runs_to_its_tick_and_leaves_the_track() {
        let layout = BarLayout::of(0.42, Some(0.9));
        assert!((layout.used - 0.42).abs() < 1e-6);
        let (from, to) = layout.run.unwrap();
        assert!((from - 0.42).abs() < 1e-6 && (to - 0.9).abs() < 1e-6);
        assert!((layout.track_from - 0.9).abs() < 1e-6);
        let tick = layout.tick.unwrap();
        assert!((tick.at - 0.9).abs() < 1e-6);
        assert!(!tick.past_limit);
    }

    /// Exactly at the limit is not past it: the run fills the bar, the tick is the ordinary one.
    #[test]
    fn a_projection_at_the_limit_fills_the_bar_without_the_warning() {
        let layout = BarLayout::of(0.5, Some(1.0));
        assert_eq!(layout.run, Some((0.5, 1.0)));
        assert_eq!(
            layout.tick,
            Some(Tick {
                at: 1.0,
                past_limit: false
            })
        );
    }

    /// Past the limit, the run fills the bar and the tick sits at the end, flagged, so it looks different without a
    /// hover; the solid share stays where it is.
    #[test]
    fn a_projection_past_the_limit_flags_the_tick_at_the_end() {
        let layout = BarLayout::of(0.91, Some(3.31));
        assert!((layout.used - 0.91).abs() < 1e-6);
        assert_eq!(layout.run.map(|(_, to)| to), Some(1.0));
        assert_eq!(layout.track_from, 1.0, "no track left");
        assert_eq!(
            layout.tick,
            Some(Tick {
                at: 1.0,
                past_limit: true
            })
        );
    }

    /// A projection a point ahead still gets its tick at the true share, neither moved nor widened.
    #[test]
    fn a_projection_a_point_ahead_still_gets_its_tick() {
        let layout = BarLayout::of(0.53, Some(0.54));
        let tick = layout.tick.unwrap();
        assert!((tick.at - 0.54).abs() < 1e-6);
        assert!((layout.used - 0.53).abs() < 1e-6);
    }

    /// Nothing projected, or a projection at or behind the share used: a plain bar with no run and no tick.
    #[test]
    fn no_projection_draws_a_plain_bar() {
        for projected in [None, Some(0.42), Some(0.3), Some(f64::NAN)] {
            let layout = BarLayout::of(0.42, projected);
            assert_eq!(layout.run, None, "{projected:?}");
            assert_eq!(layout.tick, None, "{projected:?}");
            assert_eq!(layout.track_from, 0., "the whole track, under the pill end");
        }
    }

    #[test]
    fn a_metric_with_a_pace_projects_and_says_so() {
        // 1%/h over 48 hours left: 35% + 48% = 83%.
        let projection = Projection::of(&window(0.35, Some(Pace::per_hour(0.01))), 0.35, now());
        assert_eq!(projection.projected.map(|p| (p * 100.0).round()), Some(83.0));
        assert_eq!(projection.label, "35% used, on pace for 83%");
        assert_eq!(
            projection.tooltip,
            "35% used · resets in 2d · on pace for 83% by the reset (1.0%/h)"
        );
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
        assert!(
            projection.projected.unwrap() > 1.0,
            "the faded segment still runs to the end"
        );
        assert_eq!(projection.label, "91% used, 1d 22h early");
        assert_eq!(projection.tooltip, "91% used · resets in 2d · 1d 22h early (5.0%/h)");
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

    #[test]
    fn credits_have_no_projection_and_say_so() {
        let credits = Metric::Credits {
            label: "Credits".into(),
            balance: Some(18_400.),
            start: Some(50_000.),
        };
        let projection = Projection::of(&credits, 0.63, now());
        assert_eq!(projection.projected, None);
        assert_eq!(projection.label, "63% used");
        assert_eq!(
            projection.tooltip,
            "63% used · Credits don't project: the bar runs on the spend since the last top-up"
        );
    }
}
