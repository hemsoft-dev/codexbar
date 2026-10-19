//! A three-quarter ring for a window's share of its pool (#174). The used arc is drawn in the status color with a
//! soft glow and a rounded head, the share projected by the reset runs on behind it in the same color faded, and
//! notches mark each quarter. The number sits on the ring's center and the pace in the ring's opening.
//!
//! Everything is painted from the box the ring gets, so it fits any card at any zoom (#176): the ring takes the
//! smaller side, the text scales with the radius, and a three-quarter ring's arc reaches only `0.707 × radius` below
//! its center, so the ring is drawn a little below the box's center to center the arc's extent.

use std::f32::consts::{FRAC_1_SQRT_2, PI};

use gpui_kit::base::plot::shape::{Arc, ArcData};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Bounds, FontWeight, Hsla, IntoElement, Pixels, ShapedLine, SharedString, Styled as _, TextAlign, TextRun,
    Window, canvas, point, px, size,
};

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
/// How far below the box's center the ring's center sits, as a share of the radius: half of what the opening
/// leaves empty at the bottom (`radius - 0.707 × radius`), so the arc's extent is centered.
const DROP_FRACTION: f32 = (1. - FRAC_1_SQRT_2) / 2.;
/// Line height as a share of the font size, for every line the ring paints.
const LINE_HEIGHT: f32 = 1.2;

/// The ring's shares, text and colors, resolved once from the theme.
pub struct Gauge {
    used: f32,
    projected: Option<f32>,
    number: SharedString,
    pace: Option<SharedString>,
    color: Hsla,
    track: Hsla,
    /// The card's background, for the notches and the head's center.
    cut: Hsla,
    foreground: Hsla,
    muted: Hsla,
    /// The largest radius, in pixels (#200); unbounded unless set.
    max_radius: f32,
}

/// The smallest ring worth drawing, in pixels: its center (the radius less the 8px band) must hold the smallest
/// number, "100" with its percent sign at 14px, about 36px wide. Below it the box keeps the number alone (#176).
pub const MIN_RADIUS: f32 = 28.;

/// Where everything goes in a box, in pixels from the box's origin.
#[derive(Debug, PartialEq)]
struct Geometry {
    height: f32,
    radius: f32,
    ring: f32,
    center_x: f32,
    center_y: f32,
    number_size: f32,
    label_size: f32,
    pace_size: f32,
}

impl Geometry {
    /// The ring that fits a box, no larger than `max_radius` (#200): a tall card on a big display doesn't balloon the
    /// ring past its text. A box too small for a ring still places the number, at its smallest, on the box's center,
    /// so the card never loses its value at extreme zoom.
    fn fit(width: f32, height: f32, max_radius: f32) -> Self {
        let side = width.min(height);
        // Room for the glow and the head past the ring.
        let margin = (side * 0.06).max(GLOW + 2.);
        let radius = (side / 2. - margin).max(0.).min(max_radius);
        let draws_ring = radius >= MIN_RADIUS;
        Self {
            height,
            radius,
            ring: (radius * 0.16).clamp(8., 20.),
            center_x: width / 2.,
            center_y: height / 2. + if draws_ring { DROP_FRACTION * radius } else { 0. },
            number_size: (radius * 0.44).max(14.),
            label_size: (radius * 0.14).max(9.),
            pace_size: (radius * 0.12).max(9.),
        }
    }

    fn draws_ring(&self) -> bool {
        self.radius >= MIN_RADIUS
    }

    /// The top of the pace line: in the opening, between the arc's two ends.
    fn pace_top(&self) -> f32 {
        self.center_y + 0.6 * self.radius
    }

    /// Whether the pace line has a ring to sit in and room in the box; otherwise it is left out.
    fn draws_pace(&self) -> bool {
        self.draws_ring() && self.pace_top() + self.pace_size * LINE_HEIGHT <= self.height
    }
}

impl Gauge {
    /// `used` and `projected` are shares of the pool, clamped to the ring. A projection no further than what is
    /// used draws nothing. `number` is the text in the middle, without its percent sign.
    pub fn new(used: f64, projected: Option<f64>, number: impl Into<SharedString>, color: Hsla, cx: &App) -> Self {
        let (used, projected) = shares(used, projected);
        Self {
            used,
            projected,
            number: number.into(),
            pace: None,
            color,
            track: cx.theme().muted,
            cut: cx.theme().group_box,
            foreground: cx.theme().foreground,
            muted: cx.theme().muted_foreground,
            max_radius: f32::INFINITY,
        }
    }

    /// The line in the ring's opening, such as "on pace for 97%".
    pub fn pace(mut self, pace: Option<impl Into<SharedString>>) -> Self {
        self.pace = pace.map(Into::into);
        self
    }

    /// The largest the ring grows, however big its box (#200); pass a zoom-scaled size.
    pub fn max_radius(mut self, radius: Pixels) -> Self {
        self.max_radius = radius.as_f32();
        self
    }

    /// The ring, filling its box.
    pub fn render(self) -> impl IntoElement {
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| self.paint(bounds, window, cx),
        )
        .size_full()
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let geometry = Geometry::fit(bounds.size.width.as_f32(), bounds.size.height.as_f32(), self.max_radius);
        if geometry.draws_ring() {
            self.paint_ring(&bounds, &geometry, window);
        }
        self.paint_text(&bounds, &geometry, window, cx);
    }

    fn paint_ring(&self, bounds: &Bounds<Pixels>, geometry: &Geometry, window: &mut Window) {
        let Geometry {
            radius,
            ring,
            center_x,
            center_y,
            ..
        } = *geometry;
        let (inner, outer) = (radius - ring, radius);
        // The arcs center on their bounds, so give them a box centered on the ring's center.
        let ring_box = Bounds::new(
            point(
                bounds.origin.x + px(center_x - radius),
                bounds.origin.y + px(center_y - radius),
            ),
            size(px(radius * 2.), px(radius * 2.)),
        );
        let arc = |inner: f32, outer: f32, from: f32, to: f32, color: Hsla, window: &mut Window| {
            let data = ArcData::new(&(), 0, 1., START + from * SWEEP, START + to * SWEEP);
            Arc::new()
                .inner_radius(inner)
                .outer_radius(outer)
                .paint(&data, color, &ring_box, window);
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
                bounds.origin.x + px(center_x + middle * angle.sin()),
                bounds.origin.y + px(center_y - middle * angle.cos()),
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

    /// The number with its percent sign on the ring's center, "used" under it, and the pace in the opening.
    fn paint_text(&self, bounds: &Bounds<Pixels>, geometry: &Geometry, window: &mut Window, cx: &mut App) {
        let Geometry {
            center_x,
            center_y,
            number_size,
            label_size,
            pace_size,
            ..
        } = *geometry;
        let number = shape(&self.number, number_size, FontWeight::BOLD, self.foreground, window);
        let percent_size = number_size * 0.42;
        let percent = shape("%", percent_size, FontWeight::SEMIBOLD, self.muted, window);
        let used = shape("used", label_size, FontWeight::NORMAL, self.muted, window);

        // The number line and the "used" line together are centered on the ring's center.
        let number_height = number_size * LINE_HEIGHT;
        let used_height = label_size * LINE_HEIGHT;
        let number_top = center_y - (number_height + used_height) / 2.;
        let used_top = number_top + number_height;
        let gap = percent_size * 0.15;
        let width = number.width().as_f32() + gap + percent.width().as_f32();
        let left = center_x - width / 2.;
        // The percent sign shares the number's baseline.
        let percent_top =
            number_top + (baseline(&number, number_height) - baseline(&percent, percent_size * LINE_HEIGHT));

        let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
        let _ = number.paint(
            at(left, number_top),
            px(number_height),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        let _ = percent.paint(
            at(left + number.width().as_f32() + gap, percent_top),
            px(percent_size * LINE_HEIGHT),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        let _ = used.paint(
            at(center_x - used.width().as_f32() / 2., used_top),
            px(used_height),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if let Some(pace) = self.pace.as_ref().filter(|_| geometry.draws_pace()) {
            let pace = shape(pace, pace_size, FontWeight::NORMAL, self.muted, window);
            let _ = pace.paint(
                at(center_x - pace.width().as_f32() / 2., geometry.pace_top()),
                px(pace_size * LINE_HEIGHT),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

/// One line of text in the window's font at `size` and `weight`.
fn shape(text: &str, size: f32, weight: FontWeight, color: Hsla, window: &mut Window) -> ShapedLine {
    let mut font = window.text_style().font();
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
}

/// Where a line's baseline falls below its top when painted at `line_height`: the glyph box is centered in the line.
fn baseline(line: &ShapedLine, line_height: f32) -> f32 {
    let ascent = line.ascent.as_f32();
    let descent = line.descent.as_f32();
    (line_height - ascent - descent) / 2. + ascent
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

    #[test]
    fn the_ring_fits_its_box_whatever_its_shape() {
        // The arc's extent is centered in the box and the pace line fits inside it, in a wide box, a tall box, a
        // small one and a zoomed-in one.
        for (width, height) in [(400., 240.), (240., 400.), (120., 120.), (300., 900.)] {
            let g = Geometry::fit(width, height, f32::INFINITY);
            assert!(g.draws_ring() && g.draws_pace(), "{width}x{height}: ring and pace");
            let arc_top = g.center_y - g.radius;
            let arc_bottom = g.center_y + FRAC_1_SQRT_2 * g.radius;
            assert!(
                arc_top >= 0. && g.center_x - g.radius >= 0.,
                "{width}x{height}: inside the box"
            );
            assert!(
                (arc_top - (height - arc_bottom)).abs() < 0.01,
                "{width}x{height}: the arc's extent is centered"
            );
            assert!(
                g.pace_top() + g.pace_size * LINE_HEIGHT <= height,
                "{width}x{height}: the pace line fits"
            );
            assert!(
                g.pace_top() > g.center_y + 0.5 * g.radius,
                "{width}x{height}: by the opening"
            );
            assert!(
                g.number_size * LINE_HEIGHT < 2. * (g.radius - g.ring),
                "{width}x{height}: the number is inside the ring"
            );
        }
    }

    #[test]
    fn a_box_too_small_for_the_ring_keeps_the_number_on_its_center() {
        // At extreme zoom the card can leave the ring a sliver of a box (#176): no ring, no pace, but the number at
        // its smallest size on the box's center.
        let g = Geometry::fit(30., 30., f32::INFINITY);
        assert!(!g.draws_ring() && !g.draws_pace());
        assert_eq!((g.center_x, g.center_y), (15., 15.));
        assert_eq!((g.number_size, g.label_size), (14., 9.));

        // The smallest ring drawn still has room in its center for "100%" at the smallest size, and a box a little
        // narrower draws none rather than a ring over the number.
        let smallest = Geometry::fit(2. * (MIN_RADIUS + GLOW + 2.), 400., f32::INFINITY);
        assert!(smallest.draws_ring(), "{smallest:?}");
        assert!(2. * (smallest.radius - smallest.ring) >= 40., "{smallest:?}");
        assert!(!Geometry::fit(2. * (MIN_RADIUS + GLOW + 1.), 400., f32::INFINITY).draws_ring());
    }

    #[test]
    fn a_capped_ring_stays_centered_at_its_cap_in_a_bigger_box() {
        // A 600×500 slot would give a 220px radius; the cap keeps it at 120 and the text sized with it (#200).
        let g = Geometry::fit(600., 500., 120.);
        assert_eq!(g.radius, 120.);
        assert_eq!(g.center_x, 300.);
        assert!(g.draws_ring() && g.draws_pace());
        assert!((g.number_size - 120. * 0.44).abs() < 1e-6);
        assert!(
            g.center_y + FRAC_1_SQRT_2 * g.radius <= 500.,
            "the arc stays in the box"
        );
    }
}
