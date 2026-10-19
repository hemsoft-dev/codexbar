//! One bar for a limit (#197): the share used as a solid fill, the share projected by the reset as a faded run past
//! it in the same color, and a tooltip that says both. The table, the focus card and the ring gauge all describe
//! their metric through [`Projection`], so the hover text and the accessibility label read the same everywhere.

use chrono::{DateTime, Utc};
use codexbar_core::{Metric, format};
use gpui_kit::TestSupportExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px, relative,
};

/// How the faded run shows against the solid fill and the track: the same color at these opacities.
const PROJECTED_OPACITY: f32 = 0.4;
const TRACK_OPACITY: f32 = 0.2;

/// What a bar says about its metric: the share used, the share projected by the reset when the pace allows one, and
/// the words for both.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// 0 to 1, what the solid fill shows.
    pub used: f64,
    /// 0 up, past 1 when the limit runs out before the reset; `None` with nothing to project.
    pub projected: Option<f64>,
    /// "42% used, on pace for 78%", or "42% used" with nothing projected: the accessibility label.
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
        let outlook = match (projected, pace) {
            (Some(projected), Some(pace)) => format!(
                "on pace for {:.0}% by the reset ({:.1}%/h)",
                projected * 100.0,
                pace.fraction_per_hour() * 100.0
            ),
            (Some(projected), None) => format!("on pace for {:.0}% by the reset", projected * 100.0),
            (None, _) => match metric {
                Metric::Window { .. } | Metric::Quota { .. } | Metric::Spend { .. } => {
                    "Too early in the window to project".to_owned()
                }
                Metric::Credits { .. } => "Credits don't project: the bar runs on the spend since the last top-up".to_owned(),
                Metric::Balance { .. } | Metric::Activity { .. } => "Nothing to project".to_owned(),
            },
        };
        let label = match projected {
            Some(projected) => format!("{share}, on pace for {:.0}%", projected * 100.0),
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

/// The bar: `height` tall, its track faint, the projected share faded past the solid share, with the tooltip on
/// hover and `projection.label` for screen readers and the headless tests. `id` names the hoverable cell; the bar
/// itself is `<id>-bar`.
pub fn usage_bar(
    id: &str,
    projection: &Projection,
    color: Hsla,
    height: Pixels,
    spoken_name: impl Into<SharedString>,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    let used = projection.used.clamp(0.0, 1.0) as f32;
    // Drawn only past the solid share: a pace that slowed to nothing adds nothing.
    let projected = projection
        .projected
        .map(|share| share.clamp(0.0, 1.0) as f32)
        .filter(|share| *share > used);
    let tooltip: SharedString = projection.tooltip.clone().into();
    let spoken: SharedString = spoken_name.into();
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
                .test_support()
                .aria_label(format!("{spoken} used"))
                .relative()
                .w_full()
                .h(height)
                .rounded(height / 2.)
                .overflow_hidden()
                .child(div().absolute().size_full().bg(color.opacity(TRACK_OPACITY)))
                .when_some(projected, |this, projected| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(projected))
                            .rounded(height / 2.)
                            .bg(color.opacity(PROJECTED_OPACITY)),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(relative(used))
                        .rounded(height / 2.)
                        .bg(color),
                ),
        )
        .into_any_element()
}

/// The height the table's bars have, as the stock small progress bar.
pub fn small() -> Pixels {
    px(6.)
}

/// The height the focus card's bar has, as the stock medium progress bar.
pub fn medium() -> Pixels {
    px(8.)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use codexbar_core::metric::Pace;

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
    fn a_projection_past_the_limit_keeps_its_figure() {
        let projection = Projection::of(&window(0.91, Some(Pace::per_hour(0.05))), 0.91, now());
        assert!(projection.projected.unwrap() > 1.0);
        assert!(projection.label.ends_with("on pace for 331%"), "{}", projection.label);
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
