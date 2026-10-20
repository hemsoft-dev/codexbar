//! Smooth chart curves that stay inside their plots (#229).
//!
//! gpui-kit's only smooth curve is a Catmull-Rom spline, which swings past its points around a sharp change: below
//! 0% after a reset, above 100% beside a peak (#217). A monotone cubic (Fritsch–Carlson, d3's `curveMonotoneX`) is
//! as smooth and passes through every point, but between two points it never goes beyond either of them.
//!
//! [`SmoothPlot`] draws that curve over one of gpui-kit's own charts, so the chart keeps its axes, grid, labels,
//! hover and tooltip. It holds two copies of the chart, built alike: one drawn with its series transparent, which
//! paints everything but the curves, and one with the series' real colors, which is never painted and only answers
//! hover. The curves go through the exact spots that chart puts its hover dots on.

use gpui_kit::base::plot::label::TEXT_SIZE;
use gpui_kit::base::plot::{Plot, PlotAppear, PlotElement, PlotHover, TooltipState, axis_gutter};
use gpui_kit::{
    AnyElement, App, Background, BorderStyle, Bounds, ContentMask, ElementId, Hsla, IntoElement, PathBuilder, Pixels,
    Point, Window, point, px, quad, size,
};

/// How one of the chart's series is drawn.
#[derive(Clone, Copy)]
pub struct Series {
    stroke: Hsla,
    width: Pixels,
    fill: Option<Background>,
    dots: bool,
}

impl Series {
    /// An area: a 1px line with `fill` below it down to the plot's floor, as gpui-kit's area chart draws it.
    pub fn area(stroke: Hsla, fill: Background) -> Self {
        Self {
            stroke,
            width: px(1.),
            fill: Some(fill),
            dots: false,
        }
    }

    /// A 2px line with an 8px dot on every reading, as gpui-kit's line chart draws it with `.dot()`.
    pub fn line(stroke: Hsla) -> Self {
        Self {
            stroke,
            width: px(2.),
            fill: None,
            dots: true,
        }
    }
}

/// One of gpui-kit's point charts with its series redrawn as monotone curves. See the module docs.
pub struct SmoothPlot<P> {
    drawn: P,
    hover: P,
    series: Vec<Series>,
    x_axis: bool,
    appear: f32,
}

impl<P: Plot + 'static> SmoothPlot<P> {
    /// `drawn` and `hover` must be the same chart built from the same data and settings: `drawn` with every series'
    /// stroke and fill transparent, `hover` with their real colors. `series` describes how each one is drawn, in the
    /// chart's order. `x_axis` says whether the chart shows its x axis, whose label gutter sits below the plot.
    pub fn new(drawn: P, hover: P, series: Vec<Series>, x_axis: bool) -> Self {
        Self {
            drawn,
            hover,
            series,
            x_axis,
            appear: 1.,
        }
    }

    /// Each reading's spot in the plot, relative to `bounds`: its x and one y per series. They come from the hover
    /// chart itself, so the curves pass exactly through the points its hover dots mark.
    fn readings(&self, bounds: Bounds<Pixels>, cx: &App) -> Vec<(f32, Vec<f32>)> {
        let width = bounds.size.width.as_f32();
        let probe = |x: f32| self.hover.tooltip_state(point(px(x), px(0.)), bounds, cx);
        // The last reading is the nearest to the right edge; the first, to where the plot starts past the y labels.
        let Some(last) = probe(width - 0.5) else {
            return Vec::new();
        };
        let Some(first) = (0..width as usize).step_by(2).find_map(|x| probe(x as f32)) else {
            return Vec::new();
        };
        let count = last.index + 1;
        let (x0, x1) = (first.cross_line.x.as_f32(), last.cross_line.x.as_f32());
        // Readings are evenly spaced, so each is probed at its own x and the chart snaps to it exactly.
        (0..count)
            .filter_map(|ix| {
                let x = if count > 1 {
                    x0 + (x1 - x0) * ix as f32 / (count - 1) as f32
                } else {
                    x0
                };
                let state: TooltipState = probe(x)?;
                (state.index == ix).then(|| {
                    (
                        state.cross_line.x.as_f32(),
                        state.dots.iter().map(|dot| dot.y.as_f32()).collect(),
                    )
                })
            })
            .collect()
    }

    /// The mask the chart's series draw under while it appears from the left, as gpui-kit's charts reveal theirs.
    fn reveal(&self, bounds: Bounds<Pixels>) -> Option<ContentMask<Pixels>> {
        if self.appear >= 1. {
            return None;
        }
        let bleed = px(4.);
        let end = bounds.left() - bleed + (bounds.size.width + bleed * 2.) * self.appear.max(0.);
        Some(ContentMask {
            bounds: Bounds::from_corners(
                point(bounds.left() - bleed, bounds.top() - bleed),
                point(end, bounds.bottom() + bleed),
            ),
        })
    }
}

impl<P: Plot + 'static> IntoElement for SmoothPlot<P> {
    type Element = PlotElement<Self>;

    fn into_element(self) -> Self::Element {
        PlotElement::new(self)
    }
}

impl<P: Plot + 'static> Plot for SmoothPlot<P> {
    fn prepaint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
        // Both measure their y labels here, so both lay the plot out past the same gutter.
        let _ = self.hover.prepaint(bounds, window, cx);
        self.drawn.prepaint(bounds, window, cx)
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        self.drawn.paint(bounds, window, cx);
        let readings = self.readings(bounds, cx);
        if readings.is_empty() {
            return;
        }
        let origin = bounds.origin;
        let floor = origin.y + bounds.size.height
            - px(if self.x_axis {
                // gpui-kit's point charts keep this much below the plot for the x labels, at their default size.
                axis_gutter(px(TEXT_SIZE))
            } else {
                0.
            });
        window.with_content_mask(self.reveal(bounds), |window| {
            for (ix, series) in self.series.iter().enumerate() {
                let points: Vec<Point<Pixels>> = readings
                    .iter()
                    .filter_map(|(x, ys)| ys.get(ix).map(|y| origin + point(px(*x), px(*y))))
                    .collect();
                let Some(&start) = points.first() else {
                    continue;
                };
                let segments = monotone(&points);
                if let Some(fill) = series.fill {
                    let mut area = PathBuilder::fill();
                    area.move_to(start);
                    for [a, b, to] in &segments {
                        area.cubic_bezier_to(*to, *a, *b);
                    }
                    let end = *points.last().unwrap_or(&start);
                    area.line_to(point(end.x, floor));
                    area.line_to(point(start.x, floor));
                    area.close();
                    if let Ok(path) = area.build() {
                        window.paint_path(path, fill);
                    }
                }
                let mut line = PathBuilder::stroke(series.width);
                line.move_to(start);
                for [a, b, to] in &segments {
                    line.cubic_bezier_to(*to, *a, *b);
                }
                if let Ok(path) = line.build() {
                    window.paint_path(path, series.stroke);
                }
                if series.dots {
                    let size_px = px(8.);
                    for dot in &points {
                        window.paint_quad(quad(
                            Bounds::new(*dot - point(size_px / 2., size_px / 2.), size(size_px, size_px)),
                            size_px / 2.,
                            series.stroke,
                            px(1.),
                            series.stroke,
                            BorderStyle::default(),
                        ));
                    }
                }
            }
        });
    }

    fn id(&self) -> Option<ElementId> {
        self.hover.id()
    }

    fn interactive(&self) -> bool {
        self.hover.interactive()
    }

    fn appear(&mut self, appear: PlotAppear, window: &mut Window, cx: &mut App) {
        self.appear = appear.progress();
        self.drawn.appear(appear.clone(), window, cx);
        self.hover.appear(appear, window, cx);
    }

    fn appear_generation(&self) -> Option<u64> {
        self.drawn.appear_generation()
    }

    fn tooltip_state(&self, position: Point<Pixels>, bounds: Bounds<Pixels>, cx: &App) -> Option<TooltipState> {
        self.hover.tooltip_state(position, bounds, cx)
    }

    fn hover(&mut self, hover: Option<&PlotHover>, window: &mut Window, cx: &mut App) {
        self.hover.hover(hover, window, cx);
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.hover.tooltip(state, cursor, bounds, window, cx)
    }
}

/// The cubic Bézier segments of a monotone curve through `points`, whose x must increase: for each segment its two
/// control points and its end. Between two points the curve never leaves the range of their two y values, so a curve
/// through points inside a plot stays inside it. Two points make a straight segment; fewer make none.
///
/// This is d3's `curveMonotoneX` (Steffen's method): the tangent at an inner point is the smaller of the slopes around
/// it, or flat where they turn, and the end tangents follow their segment.
pub fn monotone(points: &[Point<Pixels>]) -> Vec<[Point<Pixels>; 3]> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let xy: Vec<(f32, f32)> = points.iter().map(|p| (p.x.as_f32(), p.y.as_f32())).collect();
    let tangents = tangents(&xy);
    (0..n - 1)
        .map(|ix| {
            let ((x0, y0), (x1, y1)) = (xy[ix], xy[ix + 1]);
            let third = (x1 - x0) / 3.;
            [
                point(px(x0 + third), px(y0 + third * tangents[ix])),
                point(px(x1 - third), px(y1 - third * tangents[ix + 1])),
                points[ix + 1],
            ]
        })
        .collect()
}

/// The slope of the curve at each point (see [`monotone`]).
fn tangents(xy: &[(f32, f32)]) -> Vec<f32> {
    let n = xy.len();
    let slope = |ix: usize| {
        let ((x0, y0), (x1, y1)) = (xy[ix], xy[ix + 1]);
        if x1 > x0 { (y1 - y0) / (x1 - x0) } else { 0. }
    };
    if n == 2 {
        let s = slope(0);
        return vec![s, s];
    }
    let sign = |v: f32| if v < 0. { -1. } else { 1. };
    let mut tangents = vec![0.; n];
    for ix in 1..n - 1 {
        let (h0, h1) = (xy[ix].0 - xy[ix - 1].0, xy[ix + 1].0 - xy[ix].0);
        let (s0, s1) = (slope(ix - 1), slope(ix));
        let p = if h0 + h1 > 0. {
            (s0 * h1 + s1 * h0) / (h0 + h1)
        } else {
            0.
        };
        let t = (sign(s0) + sign(s1)) * s0.abs().min(s1.abs()).min(0.5 * p.abs());
        tangents[ix] = if t.is_finite() { t } else { 0. };
    }
    // An end follows its segment's slope, bent by its neighbour's tangent, as d3 does.
    let end = |segment: f32, inner: f32| (3. * segment - inner) / 2.;
    tangents[0] = end(slope(0), tangents[1]);
    tangents[n - 1] = end(slope(n - 2), tangents[n - 2]);
    tangents
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(values: &[(f32, f32)]) -> Vec<Point<Pixels>> {
        values.iter().map(|(x, y)| point(px(*x), px(*y))).collect()
    }

    /// Every point along the curve, sampled finely, stays within its segment's two end values.
    fn assert_stays_between_points(values: &[(f32, f32)]) {
        let pts = points(values);
        let segments = monotone(&pts);
        assert_eq!(segments.len(), pts.len().saturating_sub(1));
        for (ix, [a, b, to]) in segments.iter().enumerate() {
            let from = pts[ix];
            let (low, high) = (from.y.min(to.y).as_f32(), from.y.max(to.y).as_f32());
            for step in 0..=64 {
                let t = step as f32 / 64.;
                let u = 1. - t;
                let y = u * u * u * from.y.as_f32()
                    + 3. * u * u * t * a.y.as_f32()
                    + 3. * u * t * t * b.y.as_f32()
                    + t * t * t * to.y.as_f32();
                assert!(
                    y >= low - 1e-3 && y <= high + 1e-3,
                    "segment {ix} of {values:?} reaches {y} outside {low}..{high}"
                );
            }
        }
    }

    #[test]
    fn the_curve_never_passes_its_points() {
        // y grows downwards, as on screen: 0 is the plot's top, 100 its floor.
        // A reset: a climb to 97%, a drop to 3%, then back up (the demo's Copilot case).
        assert_stays_between_points(&[(0., 40.), (10., 20.), (20., 3.), (30., 97.), (40., 85.), (50., 70.)]);
        // A swing from the floor to the top and back.
        assert_stays_between_points(&[(0., 100.), (10., 0.), (20., 100.), (30., 0.)]);
        // A flat run, and two equal readings, stay flat.
        assert_stays_between_points(&[(0., 50.), (10., 50.), (20., 50.), (30., 10.)]);
        assert_stays_between_points(&[(0., 50.), (10., 50.)]);
        // Uneven spacing, and a single step.
        assert_stays_between_points(&[(0., 10.), (3., 90.), (30., 95.), (31., 0.)]);
        assert_stays_between_points(&[(0., 10.), (10., 90.)]);
    }

    #[test]
    fn the_curve_passes_through_every_point_and_bends_where_it_turns() {
        let pts = points(&[(0., 80.), (10., 20.), (20., 60.)]);
        let segments = monotone(&pts);
        // Each segment ends on the next point.
        assert_eq!(segments[0][2], pts[1]);
        assert_eq!(segments[1][2], pts[2]);
        // At a turning point the tangent is flat, so the curve peaks exactly on the point.
        assert_eq!(segments[0][1].y, pts[1].y);
        assert_eq!(segments[1][0].y, pts[1].y);
        // A rising run is smooth, not a corner: the controls around the middle point line up.
        let rising = points(&[(0., 90.), (10., 50.), (20., 10.)]);
        let segments = monotone(&rising);
        let (before, after) = (segments[0][1], segments[1][0]);
        let slope_in = (rising[1].y - before.y).as_f32() / (rising[1].x - before.x).as_f32();
        let slope_out = (after.y - rising[1].y).as_f32() / (after.x - rising[1].x).as_f32();
        assert!((slope_in - slope_out).abs() < 1e-4);
    }

    #[gpui_kit::test]
    fn the_curve_goes_through_the_chart_s_own_hover_points(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::component::chart::LineChart;
        let chart = |stroke: Hsla| {
            LineChart::new(vec![("a", 10.), ("b", 90.), ("c", 50.)])
                .id("smooth-test")
                .x(|(label, _)| *label)
                .y(|(_, value)| *value)
                .stroke(stroke)
                .y_domain(0., 100.)
                .y_padding(0., 0.)
                .x_axis(false)
        };
        let color = gpui_kit::hsla(0.1, 0.8, 0.5, 1.);
        let plot = SmoothPlot::new(chart(CLEAR), chart(color), vec![Series::line(color)], false);
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(200.), px(100.)));
        cx.update(|cx| {
            // Three readings, spread across the plot, at the heights of 10, 90 and 50 on a 0–100 axis.
            let readings = plot.readings(bounds, cx);
            let xs: Vec<f32> = readings.iter().map(|(x, _)| *x).collect();
            let ys: Vec<f32> = readings.iter().map(|(_, ys)| ys[0]).collect();
            assert_eq!(xs, [0., 100., 200.]);
            assert_eq!(ys, [90., 10., 50.]);
            // Hover is the real-colored chart's: the middle reading, with its dot where the curve passes.
            let state = plot.tooltip_state(point(px(110.), px(40.)), bounds, cx).unwrap();
            assert_eq!(state.index, 1);
            assert_eq!(state.dots, [point(px(100.), px(10.))]);
        });
    }

    const CLEAR: Hsla = Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: 0.,
    };

    #[test]
    fn fewer_than_two_points_make_no_curve() {
        assert!(monotone(&[]).is_empty());
        assert!(monotone(&points(&[(0., 10.)])).is_empty());
    }
}
