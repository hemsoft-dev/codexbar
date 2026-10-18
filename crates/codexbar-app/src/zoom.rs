//! Interface zoom (#116): Ctrl+mouse wheel and Ctrl+= / Ctrl+- / Ctrl+0 (Command on macOS), matching the WPF app's 50–300% range in
//! 10% steps. Zoom scales the theme's base font, which gpui-kit uses as the rem for type, spacing, controls and
//! icons, so everything grows together. The level is saved to the shared `zoomLevel` setting after input settles.

use std::time::Duration;

use codexbar_store::settings::{ZOOM_STEP, clamp_zoom};
use gpui_kit::component::Theme;
use gpui_kit::{
    App, DispatchPhase, Global, KeyBinding, ScrollDelta, ScrollWheelEvent, TouchPhase, Window, actions, px,
};

use crate::settings_hub::SettingsHub;

actions!(codexbar, [ZoomIn, ZoomOut, ResetZoom]);

/// The theme's base font size at 100% (`font.size` in themes/codexbar.json).
const BASE_FONT_SIZE: f32 = 16.0;
/// Precise (touchpad) scrolling is accumulated so one notch-sized gesture is one step.
const PIXELS_PER_STEP: f32 = 50.0;
/// Saving waits for the wheel to settle so a fast scroll writes the settings file once.
const SAVE_DELAY: Duration = Duration::from_millis(600);
/// Another writer (the WPF app, another window) may hold the shared settings lock; retry until it's free.
const SAVE_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Quit blocks while it saves, so it retries the lock only briefly (gpui allows quit handlers 200 ms).
const QUIT_RETRIES: u32 = 4;
const QUIT_RETRY_DELAY: Duration = Duration::from_millis(15);

struct ZoomState {
    level: f64,
    /// The level last written to settings; differs from `level` while a save is pending.
    saved: f64,
    /// Bumped on every change; a pending save only runs if no newer change followed it.
    generation: u64,
    pixel_carry: f32,
}

impl Global for ZoomState {}

/// The level after `steps` 10% steps from `level`, kept in range.
pub fn stepped(level: f64, steps: i32) -> f64 {
    clamp_zoom(level + f64::from(steps) * ZOOM_STEP)
}

/// Whole zoom steps for a wheel event, carrying any partial remainder. Positive zooms in.
///
/// gpui reports a wheel as `notches × lines_per_notch` lines, where a high-resolution or free-spinning wheel sends
/// fractions of a notch, or several at once. One notch is one step, so partial notches accumulate like precise
/// (pixel) scrolling does. Without a usable `lines_per_notch`, each event counts as one notch.
fn wheel_steps(delta: &ScrollDelta, carry: &mut f32, lines_per_notch: Option<f32>) -> i32 {
    match delta {
        ScrollDelta::Lines(lines) => {
            let notches = match lines_per_notch {
                Some(per_notch) => lines.y / per_notch,
                // `signum` of 0.0 is 1.0, so a horizontal-only event must not count as a step.
                None if lines.y == 0.0 => 0.0,
                None => lines.y.signum(),
            };
            *carry += notches * PIXELS_PER_STEP;
        }
        ScrollDelta::Pixels(pixels) => *carry += f32::from(pixels.y),
    }
    let steps = (*carry / PIXELS_PER_STEP).trunc();
    *carry -= steps * PIXELS_PER_STEP;
    steps as i32
}

/// The system's "lines to scroll per notch" setting, which gpui multiplies wheel notches by. `None` when it is 0
/// or "one screen at a time", where a notch can't be recovered from the line count.
#[cfg(windows)]
fn wheel_lines_per_notch() -> Option<f32> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    const WHEEL_PAGESCROLL: u32 = u32::MAX;
    let mut lines = 0u32;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some((&raw mut lines).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    (ok.is_ok() && lines != 0 && lines != WHEEL_PAGESCROLL).then_some(lines as f32)
}

/// macOS and Linux have no such setting for gpui to apply, so each line event counts as one notch.
#[cfg(not(windows))]
fn wheel_lines_per_notch() -> Option<f32> {
    None
}

/// Steps for one event of a gesture: a new or finished touchpad gesture starts from zero, so separate partial
/// gestures never add up to an unexpected step.
fn gesture_steps(delta: &ScrollDelta, phase: TouchPhase, carry: &mut f32, lines_per_notch: Option<f32>) -> i32 {
    if phase == TouchPhase::Started {
        *carry = 0.0;
    }
    let steps = wheel_steps(delta, carry, lines_per_notch);
    if phase == TouchPhase::Ended {
        *carry = 0.0;
    }
    steps
}

/// Applies the saved level and registers the keyboard shortcuts. Call after `SettingsHub::init`.
pub fn init(cx: &mut App) {
    let level = SettingsHub::global(cx).settings().zoom_level();
    cx.set_global(ZoomState {
        level,
        saved: level,
        generation: 0,
        pixel_carry: 0.0,
    });
    apply(level, cx);
    // A change made just before Quit would otherwise be lost with its pending debounce task.
    cx.on_app_quit(|cx| {
        flush(cx);
        async {}
    })
    .detach();

    cx.bind_keys([
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary-shift-=", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ResetZoom, None),
    ]);
    cx.on_action(|_: &ZoomIn, cx| step(1, cx));
    cx.on_action(|_: &ZoomOut, cx| step(-1, cx));
    cx.on_action(|_: &ResetZoom, cx| set_level(1.0, cx));
}

/// Scales a size designed at 100% by the current zoom. For gpui-kit sizes given in pixels (table rows, columns)
/// that don't follow the rem.
pub fn scaled(px_at_100: f32, cx: &App) -> gpui_kit::Pixels {
    px(px_at_100 * level(cx) as f32)
}

pub fn level(cx: &App) -> f64 {
    cx.try_global::<ZoomState>().map_or(1.0, |state| state.level)
}

pub fn step(steps: i32, cx: &mut App) {
    set_level(stepped(level(cx), steps), cx);
}

pub fn set_level(new_level: f64, cx: &mut App) {
    let new_level = clamp_zoom(new_level);
    let generation = {
        let state = cx.global_mut::<ZoomState>();
        if (state.level - new_level).abs() < f64::EPSILON {
            return;
        }
        state.level = new_level;
        state.generation += 1;
        state.generation
    };
    apply(new_level, cx);

    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        loop {
            let outcome = cx.update(|cx| {
                if cx.global::<ZoomState>().generation != generation {
                    return SaveOutcome::Superseded;
                }
                save(cx)
            });
            if outcome != SaveOutcome::Busy {
                return;
            }
            cx.background_executor().timer(SAVE_RETRY_DELAY).await;
        }
    })
    .detach();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveOutcome {
    Saved,
    /// Another writer holds the settings lock; worth retrying.
    Busy,
    /// A newer change will save itself.
    Superseded,
    /// The settings file can't be written (newer schema, I/O); Settings shows why.
    Failed,
}

/// Writes the current level if it differs from the last saved one.
fn save(cx: &mut App) -> SaveOutcome {
    let state = cx.global::<ZoomState>();
    let level = state.level;
    if (level - state.saved).abs() < f64::EPSILON {
        return SaveOutcome::Saved;
    }
    match SettingsHub::update(cx, |settings| {
        settings.set_zoom_level(level);
        Ok(())
    }) {
        Ok(()) => {
            cx.global_mut::<ZoomState>().saved = level;
            SaveOutcome::Saved
        }
        Err(codexbar_store::settings::SettingsError::Busy) => SaveOutcome::Busy,
        Err(_) => SaveOutcome::Failed,
    }
}

/// Saves a pending change immediately (on quit), retrying briefly while another writer holds the lock.
fn flush(cx: &mut App) {
    if cx.try_global::<ZoomState>().is_none() {
        return;
    }
    for attempt in 0..QUIT_RETRIES {
        if save(cx) != SaveOutcome::Busy {
            return;
        }
        if attempt + 1 < QUIT_RETRIES {
            std::thread::sleep(QUIT_RETRY_DELAY);
        }
    }
}

/// The base font size at the current zoom, for a theme being applied (a theme resets it to 100%).
pub fn font_size(cx: &App) -> gpui_kit::Pixels {
    px(BASE_FONT_SIZE * level(cx) as f32)
}

fn apply(level: f64, cx: &mut App) {
    Theme::update(cx, |theme| theme.font_size = px(BASE_FONT_SIZE * level as f32));
}

/// Registers this frame's Ctrl+wheel (Command+wheel on macOS) listener in the capture phase, ahead of scrollable children, so the wheel
/// zooms instead of scrolling. Call from a paint callback; a wheel without Ctrl passes through untouched.
pub fn capture_wheel(window: &mut Window) {
    window.on_mouse_event(|event: &ScrollWheelEvent, phase, _window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        if !event.modifiers.secondary() {
            // Plain scrolling between Ctrl gestures must not bank toward a later zoom step.
            cx.global_mut::<ZoomState>().pixel_carry = 0.0;
            return;
        }
        cx.stop_propagation();
        let steps = {
            let state = cx.global_mut::<ZoomState>();
            gesture_steps(
                &event.delta,
                event.touch_phase,
                &mut state.pixel_carry,
                wheel_lines_per_notch(),
            )
        };
        if steps != 0 {
            step(steps, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::point;

    #[test]
    fn stepped_moves_by_ten_percent_and_clamps() {
        assert_eq!(stepped(1.0, 1), 1.1);
        assert_eq!(stepped(1.0, -2), 0.8);
        assert_eq!(stepped(2.95, 1), 3.0);
        assert_eq!(stepped(3.0, 1), 3.0);
        assert_eq!(stepped(0.5, -1), 0.5);
        // Ten steps up and down return exactly to 100%.
        let mut level = 1.0;
        for _ in 0..10 {
            level = stepped(level, 1);
        }
        for _ in 0..10 {
            level = stepped(level, -1);
        }
        assert_eq!(level, 1.0);
    }

    #[test]
    fn gesture_steps_reset_carry_between_gestures() {
        let mut carry = 0.0;
        let delta = |y: f32| ScrollDelta::Pixels(point(px(0.0), px(y)));
        assert_eq!(gesture_steps(&delta(30.0), TouchPhase::Moved, &mut carry, None), 0);
        assert_eq!(gesture_steps(&delta(10.0), TouchPhase::Ended, &mut carry, None), 0);
        assert_eq!(carry, 0.0, "a finished gesture leaves nothing behind");
        assert_eq!(gesture_steps(&delta(30.0), TouchPhase::Started, &mut carry, None), 0);
        assert_eq!(gesture_steps(&delta(25.0), TouchPhase::Moved, &mut carry, None), 1);
    }

    #[test]
    fn wheel_steps_lines_give_one_step_per_notch() {
        let lines = |y: f32| ScrollDelta::Lines(point(0.0, y));
        let mut carry = 0.0;
        assert_eq!(wheel_steps(&lines(3.0), &mut carry, Some(3.0)), 1);
        assert_eq!(wheel_steps(&lines(-3.0), &mut carry, Some(3.0)), -1);
        // Two notches in one event zoom twice.
        assert_eq!(wheel_steps(&lines(6.0), &mut carry, Some(3.0)), 2);
        assert_eq!(
            wheel_steps(&ScrollDelta::Lines(point(2.0, 0.0)), &mut carry, Some(3.0)),
            0
        );
    }

    #[test]
    fn wheel_steps_half_notches_accumulate() {
        let lines = |y: f32| ScrollDelta::Lines(point(0.0, y));
        let mut carry = 0.0;
        assert_eq!(wheel_steps(&lines(1.5), &mut carry, Some(3.0)), 0);
        assert_eq!(wheel_steps(&lines(1.5), &mut carry, Some(3.0)), 1);
        assert_eq!(carry, 0.0);
    }

    #[test]
    fn wheel_steps_without_lines_per_notch_count_events() {
        let mut carry = 0.0;
        assert_eq!(wheel_steps(&ScrollDelta::Lines(point(0.0, 40.0)), &mut carry, None), 1);
        assert_eq!(wheel_steps(&ScrollDelta::Lines(point(0.0, -0.5)), &mut carry, None), -1);
        assert_eq!(wheel_steps(&ScrollDelta::Lines(point(2.0, 0.0)), &mut carry, None), 0);
    }

    #[test]
    fn wheel_steps_pixels_accumulate_until_a_step() {
        let mut carry = 0.0;
        assert_eq!(
            wheel_steps(&ScrollDelta::Pixels(point(px(0.0), px(30.0))), &mut carry, None),
            0
        );
        assert_eq!(
            wheel_steps(&ScrollDelta::Pixels(point(px(0.0), px(30.0))), &mut carry, None),
            1
        );
        assert!((carry - 10.0).abs() < 1e-4);
        assert_eq!(
            wheel_steps(&ScrollDelta::Pixels(point(px(0.0), px(-130.0))), &mut carry, None),
            -2
        );
    }
}
