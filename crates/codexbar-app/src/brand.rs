//! Provider branding (#92): a small badge with each provider's own logo on its accent color, so accounts are
//! recognizable at a glance. The badge always carries the provider's name for screen readers; the name also sits
//! beside it. The logos are the providers' marks (`assets/brand`, see its LICENSE.md), drawn in one color.
//!
//! This is the product's token layer for provider colors, the one place they are defined. The accents approximate
//! each provider's own brand color; the logo's color is picked for contrast on it. Under Windows high contrast the
//! badge uses the scheme's text and window pair instead.

use codexbar_core::Provider;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, rgb, svg,
};

/// A provider's accent (`0xRRGGBB`) and logo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brand {
    pub accent: u32,
    /// The logo's asset path (`crate::assets`).
    pub logo: &'static str,
}

pub fn brand(provider: Provider) -> Brand {
    let accent = match provider {
        Provider::Codex => 0x10A37F,
        Provider::Claude => 0xD97757,
        Provider::Copilot => 0x8957E5,
        Provider::Cursor => 0x262626,
        Provider::OpenRouter => 0x4F52D9,
        Provider::OpenCode => 0xB45309,
        Provider::Moonshot => 0x1D4ED8,
        Provider::Greptile => 0x44A775,
        Provider::GitHubBilling => 0x1F2328,
    };
    Brand {
        accent,
        logo: crate::assets::logo_path(provider),
    }
}

/// The provider for a settings record's provider name (`OpenCodeGo` and `OpenCodeZen` are both OpenCode).
pub fn from_settings_name(name: &str) -> Option<Provider> {
    use codexbar_store::settings::names;
    Some(match name {
        names::CODEX => Provider::Codex,
        names::CLAUDE => Provider::Claude,
        names::COPILOT => Provider::Copilot,
        names::CURSOR => Provider::Cursor,
        names::OPENROUTER => Provider::OpenRouter,
        names::OPENCODE_GO | names::OPENCODE_ZEN => Provider::OpenCode,
        names::MOONSHOT => Provider::Moonshot,
        names::GREPTILE => Provider::Greptile,
        names::GITHUB_BILLING => Provider::GitHubBilling,
        _ => return None,
    })
}

/// White or near-black, whichever reads better on `accent`.
fn logo_color(accent: u32) -> u32 {
    if contrast(accent, 0xFFFFFF) >= contrast(accent, 0x111111) {
        0xFFFFFF
    } else {
        0x111111
    }
}

/// The badge: accent square, the provider's logo, and the provider's name as its accessible label. `key` makes its id unique
/// where several badges share a view (one per row).
pub fn badge(provider: Provider, key: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let brand = brand(provider);
    let high_contrast = crate::theme::is_high_contrast(cx);
    let (fill, ink): (Hsla, Hsla) = if high_contrast {
        (cx.theme().foreground, cx.theme().background)
    } else {
        (rgb(brand.accent).into(), rgb(logo_color(brand.accent)).into())
    };
    let key: SharedString = key.into();
    div()
        .id(SharedString::from(format!("provider-badge-{key}")))
        .role(Role::Image)
        .test_support()
        .aria_label(provider.display_name())
        .flex_shrink_0()
        .size(gpui_kit::rems(1.25))
        .flex()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius)
        .bg(fill)
        .child(svg().path(brand.logo).size(gpui_kit::rems(0.875)).text_color(ink))
}

fn luminance(color: u32) -> f64 {
    let channel = |shift: u32| {
        let value = ((color >> shift) & 0xFF) as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

fn contrast(a: u32, b: u32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_is_readable_on_its_accent() {
        for provider in Provider::ALL {
            let brand = brand(provider);
            let ratio = contrast(brand.accent, logo_color(brand.accent));
            assert!(ratio >= 4.5, "{provider:?}: {ratio:.2}:1");
        }
    }

    #[test]
    fn providers_are_told_apart_by_logo_not_color_alone() {
        let mut logos: Vec<&str> = Provider::ALL.iter().map(|provider| brand(*provider).logo).collect();
        logos.sort_unstable();
        logos.dedup();
        assert_eq!(logos.len(), Provider::ALL.len());
    }

    #[test]
    fn settings_names_map_to_providers() {
        use codexbar_store::settings::names;
        for name in names::ALL {
            assert!(from_settings_name(name).is_some(), "{name}");
        }
        assert_eq!(from_settings_name(names::OPENCODE_ZEN), Some(Provider::OpenCode));
    }
}
