//! A three-quarter ring for a window's share of its pool (#174). The used arc is drawn in the status color with a
//! soft glow and a rounded head, the share projected by the reset runs on behind it in the same color faded, and
//! notches mark each quarter. The opening at the bottom leaves room for the pace. The number itself is laid over the
//! ring by the card, so it stays text.

use std::f32::consts::PI;

use gpui_kit::base::plot::shape::{Arc, ArcData};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Bounds, Hsla, IntoElement, Pixels, Styled as _, Window, canvas, point, px, size};

/// Where the ring starts, clockwise from the top: the bottom left, so the opening sits at the bottom.
const START: f32 = 1.25 * PI;
/// How far the ring runs: three quarters of a turn.
const SWEEP: f32 = 1.5 * PI;
/// The notches on the ring, as shares of the pool.
const NOTCHES: [f32; 3] = [0.25, 0.5, 0.75];
/// Half the angular width of a notch, as a share of the pool.
const NOTCH_HALF_WIDTH: f32 = 0.003;
/// How far the glow reaches past the ring, in pixels.
const GLOW: f32 = 5.;
/// Space between the ring and the edge of its box, in pixels: the glow and the head need it.
const MARGIN: f32 = 7.;

/// The ring's shares and colors, resolved once from the theme.
pub struct Gauge {
    used: f32,
    projected: Option<f32>,
    color: Hsla,
    track: Hsla,
    /// The card's background, for the notches and the head's center.
    cut: Hsla,
}

impl Gauge {
    /// `used` and `projected` are shares of the pool, clamped to the ring. A projection no further than what is
    /// used draws nothing.
    pub fn new(used: f64, projected: Option<f64>, color: Hsla, cx: &App) -> Self {
        let (used, projected) = shares(used, projected);
        Self {
            used,
            projected,
            color,
            track: cx.theme().muted,
            cut: cx.theme().group_box,
        }
    }

    /// The ring, filling its box; keep it in a square-ish box so the ring is round.
    pub fn render(self) -> impl IntoElement {
        canvas(|_, _, _| {}, move |bounds, _, window, _| self.paint(bounds, window)).size_full()
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let radius = width.min(height) / 2. - MARGIN;
        if radius < 12. {
            return;
        }
        let ring = (radius * 0.16).clamp(8., 20.);
        let (inner, outer) = (radius - ring, radius);
        let arc = |inner: f32, outer: f32, from: f32, to: f32, color: Hsla, window: &mut Window| {
            let data = ArcData::new(&(), 0, 1., START + from * SWEEP, START + to * SWEEP);
            Arc::new()
                .inner_radius(inner)
                .outer_radius(outer)
                .paint(&data, color, &bounds, window);
        };

        if self.used > 0. {
            arc(
                inner - GLOW,
                outer + GLOW,
                0.,
                self.used,
                self.color.opacity(0.14),
                window,
            );
        }
        arc(inner, outer, 0., 1., self.track, window);
        if let Some(projected) = self.projected {
            arc(inner, outer, self.used, projected, self.color.opacity(0.35), window);
        }
        if self.used > 0. {
            arc(inner, outer, 0., self.used, self.color, window);
        }
        for notch in NOTCHES {
            arc(
                inner - 1.,
                outer + 1.,
                notch - NOTCH_HALF_WIDTH,
                notch + NOTCH_HALF_WIDTH,
                self.cut,
                window,
            );
        }

        // The head: a dot on the ring's middle line at the end of the used arc, with the card's color inside.
        if self.used > 0. {
            let angle = START + self.used * SWEEP;
            let middle = (inner + outer) / 2.;
            let center = point(
                bounds.origin.x + px(width / 2. + middle * angle.sin()),
                bounds.origin.y + px(height / 2. - middle * angle.cos()),
            );
            let dot = |radius: f32, color: Hsla, window: &mut Window| {
                let data = ArcData::new(&(), 0, 1., 0., 2. * PI);
                let box_ = Bounds::new(
                    point(center.x - px(radius), center.y - px(radius)),
                    size(px(radius * 2.), px(radius * 2.)),
                );
                Arc::new().outer_radius(radius).paint(&data, color, &box_, window);
            };
            dot(ring / 2. + 2., self.color, window);
            dot((ring / 2. - 3.).max(2.), self.cut, window);
        }
    }
}

/// The shares the ring draws: both clamped to it, and the projection kept only past what is used.
fn shares(used: f64, projected: Option<f64>) -> (f32, Option<f32>) {
    let used = used.clamp(0., 1.) as f32;
    let projected = projected
        .map(|projected| projected.clamp(0., 1.) as f32)
        .filter(|projected| *projected > used);
    (used, projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projection_only_draws_past_what_is_used() {
        assert_eq!(
            shares(0.59, Some(2.48)),
            (0.59, Some(1.0)),
            "a run-out projection fills the ring"
        );
        assert_eq!(shares(0.59, Some(0.4)), (0.59, None), "nothing behind what is used");
        assert_eq!(shares(1.2, None), (1.0, None));
        assert_eq!(shares(-0.5, Some(0.2)), (0.0, Some(0.2)));
    }
}
