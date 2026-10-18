//! How severity reads on screen. Every status carries a text label; color only reinforces it.

use codexbar_core::Severity;
use gpui_kit::component::{ActiveTheme as _, tag::Tag};
use gpui_kit::{App, Hsla, IntoElement, ParentElement as _};

/// The color that marks a severity. Calm limits use the brand data color.
pub fn severity_color(severity: Severity, cx: &App) -> Hsla {
    match severity {
        Severity::LimitSoon => cx.theme().danger,
        Severity::AtRisk | Severity::Watch => cx.theme().warning,
        Severity::Normal => cx.theme().chart_1,
    }
}

/// The dot beside an account name. Normal accounts get a muted dot so the column keeps its rhythm.
pub fn severity_dot_color(severity: Severity, cx: &App) -> Hsla {
    match severity {
        Severity::Normal => cx.theme().muted_foreground,
        other => severity_color(other, cx),
    }
}

/// The outlined status tag ("Limit soon", "At risk", "Watch"), or nothing for normal accounts.
pub fn severity_tag(severity: Severity) -> Option<impl IntoElement> {
    let label = severity.label()?;
    let tag = match severity {
        Severity::LimitSoon => Tag::danger(),
        _ => Tag::warning(),
    };
    Some(tag.outline().child(label))
}
