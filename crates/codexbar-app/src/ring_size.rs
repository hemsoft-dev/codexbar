//! The gauge ring's size (#219): a slider beside the ring, shown while the pointer is over the ring's card or the
//! slider has keyboard focus, sets the ring's largest radius. The ring still fits its card (`Gauge` shrinks to the
//! box), so a size larger than the card only fills it.

use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::{
    AccessibleAction, AnyElement, App, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    Orientation, ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    div, rems,
};

/// The smallest radius the slider sets, in pixels: the gauge's own smallest ring.
const MIN_RADIUS: f32 = crate::gauge::MIN_RADIUS;
/// The largest radius the slider sets below its top, in pixels. The top itself lifts the cap (see [`cap`]).
const MAX_RADIUS: f32 = 488.;
/// The default size: the 120px radius the ring had before it could be resized (#200), along the slider.
pub const DEFAULT: f32 = (120. - MIN_RADIUS) / (MAX_RADIUS - MIN_RADIUS) * 100.;
/// One arrow-key step along the slider.
pub const STEP: f32 = 5.;
/// The gauge card's hover group: hovering anywhere on the card, header and takeaway included, reveals the slider.
pub const GROUP: &str = "focus-ring-area";

/// The ring's largest radius, in unzoomed pixels, for a size along the slider (0 to 100).
pub fn radius(size: f32) -> f32 {
    MIN_RADIUS + size.clamp(0., 100.) / 100. * (MAX_RADIUS - MIN_RADIUS)
}

/// The cap on the ring's radius at a zoom level, in pixels. It never falls below the smallest ring the gauge draws,
/// so the slider's floor keeps a ring when zoomed out, and the top of the slider lifts the cap, so the ring fills a
/// card of any size.
pub fn cap(size: f32, zoom: f32) -> f32 {
    if size >= 100. {
        return f32::INFINITY;
    }
    (radius(size) * zoom).max(MIN_RADIUS)
}

/// The size an arrow, Home or End key moves to, or `None` for any other key.
pub fn key_target(size: f32, key: &str) -> Option<f32> {
    let next = match key {
        "up" | "right" => size + STEP,
        "down" | "left" => size - STEP,
        "home" => 0.,
        "end" => 100.,
        _ => return None,
    };
    Some(next.clamp(0., 100.))
}

/// The slider's accessible name.
pub const LABEL: &str = "Ring size";

/// The slider's accessible value: "20%".
pub fn spoken(size: f32) -> String {
    format!("{:.0}%", size.clamp(0., 100.))
}

/// Moves the slider to `size` and saves it, unless the preferences are read-only.
fn move_to(state: &Entity<SliderState>, size: f32, window: &mut Window, cx: &mut App) {
    if crate::prefs_hub::PrefsHub::is_read_only(cx) {
        return;
    }
    state.update(cx, |state, cx| state.set_value(size, window, cx));
    crate::prefs_hub::PrefsHub::set_ring_size(cx, Some(size));
}

/// The slider's state, its focus stop, and whether it shows without hover (it has keyboard focus).
#[derive(Clone)]
pub struct RingControl {
    pub state: Entity<SliderState>,
    pub focus: FocusHandle,
    pub revealed: bool,
}

impl RingControl {
    /// The current size along the slider.
    pub fn size(&self, cx: &App) -> f32 {
        self.state.read(cx).value().end()
    }

    /// The cap on the ring's radius at the current size, in pixels at the current zoom.
    pub fn radius(&self, cx: &App) -> gpui_kit::Pixels {
        gpui_kit::px(cap(self.size(cx), crate::zoom::level(cx) as f32))
    }

    /// The vertical slider beside the ring. Hidden until the card is hovered or the slider is focused; the arrow
    /// keys, Home and End move it, and every change is saved.
    ///
    /// It overlays the right edge of its parent, which must be `relative()` inside a card with the [`GROUP`] hover group, centered on the edge of the card's
    /// content, so half of it sits in the card's padding. Taking a column of its own squeezed the Limits card's rings
    /// at 150% zoom until their labels broke mid-word and the rings had no height left.
    ///
    /// The focus stop is the slider screen readers meet: named "Ring size", with the value, range and step, and the
    /// increment and decrement actions. gpui-kit 0.7.1's own slider node inside it can't be named (#222).
    ///
    /// While the preferences are read-only (a newer `dashboard.json`), nothing moves it, since the size couldn't be
    /// saved.
    pub fn render(&self, cx: &App) -> AnyElement {
        let size = self.size(cx);
        let read_only = crate::prefs_hub::PrefsHub::is_read_only(cx);
        let state = self.state.clone();
        let revealed = self.revealed;
        let slider = div()
            .id("ring-size")
            .role(Role::Slider)
            // Registered before the focus binding, so the headless tests can see it.
            .test_support()
            .track_focus(&self.focus)
            .aria_label(LABEL)
            .aria_value(spoken(size))
            .aria_numeric_value(size as f64)
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(100.)
            .aria_numeric_value_step(STEP as f64)
            .aria_orientation(Orientation::Vertical)
            .w(rems(1.5))
            .h(rems(9.))
            .py_1()
            .opacity(if revealed { 1. } else { 0. })
            .group_hover(GROUP, |style| style.opacity(1.))
            .on_a11y_action(AccessibleAction::Increment, {
                let state = state.clone();
                move |_, window, cx| {
                    let next = key_target(state.read(cx).value().end(), "up").unwrap_or(100.);
                    move_to(&state, next, window, cx);
                }
            })
            .on_a11y_action(AccessibleAction::Decrement, {
                let state = state.clone();
                move |_, window, cx| {
                    let next = key_target(state.read(cx).value().end(), "down").unwrap_or(0.);
                    move_to(&state, next, window, cx);
                }
            })
            .on_key_down(move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let current = state.read(cx).value().end();
                let Some(next) = key_target(current, event.keystroke.key.as_str()) else {
                    return;
                };
                cx.stop_propagation();
                move_to(&state, next, window, cx);
            })
            .child(Slider::new(&self.state).vertical().disabled(read_only).h_full());
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(rems(-0.75))
            .flex()
            .items_center()
            .child(slider)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_size_keeps_the_old_ring_and_the_ends_reach_the_floor_and_beyond_any_card() {
        assert!((radius(DEFAULT) - 120.).abs() < 0.01);
        assert_eq!(radius(0.), MIN_RADIUS);
        assert_eq!(radius(100.), MAX_RADIUS);
        assert_eq!(radius(-20.), MIN_RADIUS, "clamped");
        assert_eq!(radius(400.), MAX_RADIUS, "clamped");
    }

    #[test]
    fn keys_step_and_jump_within_the_range() {
        assert_eq!(key_target(20., "up"), Some(25.));
        assert_eq!(key_target(20., "left"), Some(15.));
        assert_eq!(key_target(98., "right"), Some(100.));
        assert_eq!(key_target(2., "down"), Some(0.));
        assert_eq!(key_target(60., "home"), Some(0.));
        assert_eq!(key_target(60., "end"), Some(100.));
        assert_eq!(key_target(60., "a"), None);
        assert_eq!(spoken(DEFAULT), "20%");
    }

    #[test]
    fn the_cap_keeps_a_ring_when_zoomed_out_and_lifts_at_the_top() {
        // Zoomed out, the floor would fall below the smallest ring the gauge draws, which left only the number.
        assert_eq!(cap(0., 0.5), crate::gauge::MIN_RADIUS);
        assert_eq!(cap(0., 1.5), crate::gauge::MIN_RADIUS * 1.5);
        assert!((cap(DEFAULT, 1.5) - 180.).abs() < 0.01, "scaled with zoom");
        // The top fills a card of any size, even past the largest set radius.
        assert!(cap(100., 1.).is_infinite());
        assert!(cap(95., 2.) < f32::INFINITY);
    }
}
