//! The gauge ring's size (#219): a slider beside the ring, shown while the pointer is over the ring's card or the
//! slider has keyboard focus, sets the ring's largest radius. The ring still fits its card (`Gauge` shrinks to the
//! box), so a size larger than the card only fills it.

use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::{
    AnyElement, App, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, rems,
};

/// The smallest radius the slider sets, in pixels: the gauge's own smallest ring.
const MIN_RADIUS: f32 = 28.;
/// The largest radius the slider sets, in pixels. Larger than any card, so the top of the slider means "fill the card".
const MAX_RADIUS: f32 = 488.;
/// The default size: the 120px radius the ring had before it could be resized (#200), along the slider.
pub const DEFAULT: f32 = (120. - MIN_RADIUS) / (MAX_RADIUS - MIN_RADIUS) * 100.;
/// One arrow-key step along the slider.
pub const STEP: f32 = 5.;
/// The group the gauge card's body belongs to, so hovering anywhere over it reveals the slider.
pub const GROUP: &str = "focus-ring-area";

/// The ring's largest radius, in unzoomed pixels, for a size along the slider (0 to 100).
pub fn radius(size: f32) -> f32 {
    MIN_RADIUS + size.clamp(0., 100.) / 100. * (MAX_RADIUS - MIN_RADIUS)
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

/// What the slider says to a screen reader: "Ring size, 20%".
pub fn spoken(size: f32) -> String {
    format!("Ring size, {:.0}%", size.clamp(0., 100.))
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

    /// The ring's largest radius at the current size, in pixels at the current zoom.
    pub fn radius(&self, cx: &App) -> gpui_kit::Pixels {
        crate::zoom::scaled(radius(self.size(cx)), cx)
    }

    /// The vertical slider beside the ring. Hidden until the card is hovered or the slider is focused; the arrow
    /// keys, Home and End move it, and every change is saved.
    pub fn render(&self, cx: &App) -> AnyElement {
        let size = self.size(cx);
        let state = self.state.clone();
        let revealed = self.revealed;
        div()
            .id("ring-size")
            .role(Role::Group)
            // Registered before the focus binding, so the headless tests can see it.
            .test_support()
            .track_focus(&self.focus)
            .aria_label(spoken(size))
            .flex_shrink_0()
            .w(rems(1.5))
            .h(rems(9.))
            .py_1()
            .opacity(if revealed { 1. } else { 0. })
            .group_hover(GROUP, |style| style.opacity(1.))
            .on_key_down(move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let current = state.read(cx).value().end();
                let Some(next) = key_target(current, event.keystroke.key.as_str()) else {
                    return;
                };
                cx.stop_propagation();
                state.update(cx, |state, cx| state.set_value(next, window, cx));
                crate::prefs_hub::PrefsHub::set_ring_size(cx, Some(next));
            })
            .child(Slider::new(&self.state).vertical().h_full())
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
        assert_eq!(spoken(DEFAULT), "Ring size, 20%");
    }
}
