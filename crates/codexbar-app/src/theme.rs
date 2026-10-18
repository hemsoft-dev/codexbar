//! Appearance (#92): the HemSoft brand themes, gold on black and gold on white, chosen by System, Light or Dark, and
//! Windows high contrast. Orange and red stay reserved for status.
//!
//! System follows Windows' app mode as it changes. While Windows high contrast is on, CodexBar uses the colors the user
//! chose for it, whatever the choice here, as Windows apps are expected to.

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeRegistry, ThemeSet};

const THEME_FILE: &str = include_str!("../themes/codexbar.json");
const DARK: &str = "CodexBar Dark";
const LIGHT: &str = "CodexBar Light";
const HIGH_CONTRAST: &str = "CodexBar High Contrast";

/// The user's appearance choice, kept in `dashboard.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|appearance| appearance.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// What decided the colors on screen, for Settings and the tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub theme: &'static str,
    pub high_contrast: bool,
}

struct State {
    applied: Option<Applied>,
    /// The high-contrast colors last applied, so a change of high-contrast scheme is picked up too.
    contrast: Option<SystemColors>,
}

impl gpui_kit::Global for State {}

/// Registers the brand themes and applies `appearance`. Falls back to the stock theme if they fail to parse.
pub fn init(cx: &mut App) {
    if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(THEME_FILE) {
        eprintln!("codexbar: brand themes failed to load: {err}");
        return;
    }
    cx.set_global(State {
        applied: None,
        contrast: None,
    });
    let appearance = crate::prefs_hub::PrefsHub::appearance(cx);
    apply(cx, appearance);
}

/// Applies the theme for `appearance`, Windows' current app mode and high contrast. Does nothing when that is what's
/// already showing, so it is cheap to call on every clock tick.
pub fn apply(cx: &mut App, appearance: Appearance) {
    apply_with(cx, appearance, system_prefers_dark(cx), system_high_contrast());
}

/// `apply` with the system state given, for the tests.
pub fn apply_with(cx: &mut App, appearance: Appearance, system_dark: bool, contrast: Option<SystemColors>) {
    if !cx.has_global::<State>() {
        return;
    }
    let theme = match (contrast.is_some(), appearance) {
        (true, _) => HIGH_CONTRAST,
        (false, Appearance::Light) => LIGHT,
        (false, Appearance::Dark) => DARK,
        (false, Appearance::System) if system_dark => DARK,
        (false, Appearance::System) => LIGHT,
    };
    let wanted = Applied {
        theme,
        high_contrast: contrast.is_some(),
    };
    let state = cx.global::<State>();
    if state.applied.as_ref() == Some(&wanted) && state.contrast == contrast {
        return;
    }
    let config = match &contrast {
        // Parsed here rather than registered: any change to the registry makes gpui-kit re-apply the current theme
        // afterwards, which would undo the zoom set below.
        Some(colors) => match serde_json::from_str::<ThemeSet>(&high_contrast_json(colors)) {
            Ok(set) => set.themes.into_iter().next().map(std::rc::Rc::new),
            Err(err) => {
                eprintln!("codexbar: high contrast theme failed to load: {err}");
                return;
            }
        },
        None => ThemeRegistry::global(cx).themes().get(theme).cloned(),
    };
    let Some(config) = config else {
        return;
    };
    // A theme carries a 100% font size; keep the user's zoom (#116).
    let font_size = crate::zoom::font_size(cx);
    Theme::update(cx, |current| {
        current.apply_config(&config);
        current.font_size = font_size;
    });
    let state = cx.global_mut::<State>();
    state.applied = Some(wanted);
    state.contrast = contrast;
}

/// True while Windows high contrast decides the colors.
pub fn is_high_contrast(cx: &App) -> bool {
    cx.try_global::<State>()
        .and_then(|state| state.applied.as_ref())
        .is_some_and(|applied| applied.high_contrast)
}

/// The theme on screen, for the tests.
#[cfg(test)]
pub fn applied(cx: &App) -> Option<Applied> {
    cx.try_global::<State>().and_then(|state| state.applied.clone())
}

fn system_prefers_dark(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        gpui_kit::WindowAppearance::Dark | gpui_kit::WindowAppearance::VibrantDark
    )
}

/// The colors of the user's Windows high-contrast scheme, as `#RRGGBB`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemColors {
    pub window: String,
    pub text: String,
    pub highlight: String,
    pub highlight_text: String,
    pub disabled_text: String,
    pub hotlight: String,
}

/// The high-contrast colors while Windows high contrast is on.
#[cfg(windows)]
fn system_high_contrast() -> Option<SystemColors> {
    use windows::Win32::Graphics::Gdi::{
        COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT,
        GetSysColor, SYS_COLOR_INDEX,
    };
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut contrast = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `contrast` is a correctly sized HIGHCONTRASTW that outlives the call.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some((&raw mut contrast).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if read.is_err() || !contrast.dwFlags.contains(HCF_HIGHCONTRASTON) {
        return None;
    }
    let color = |index: SYS_COLOR_INDEX| {
        // SAFETY: GetSysColor reads a system color by a valid index.
        let rgb = unsafe { GetSysColor(index) };
        format!("#{:02X}{:02X}{:02X}", rgb & 0xFF, (rgb >> 8) & 0xFF, (rgb >> 16) & 0xFF)
    };
    Some(SystemColors {
        window: color(COLOR_WINDOW),
        text: color(COLOR_WINDOWTEXT),
        highlight: color(COLOR_HIGHLIGHT),
        highlight_text: color(COLOR_HIGHLIGHTTEXT),
        disabled_text: color(COLOR_GRAYTEXT),
        hotlight: color(COLOR_HOTLIGHT),
    })
}

#[cfg(not(windows))]
fn system_high_contrast() -> Option<SystemColors> {
    None
}

/// The registry name of the theme for one high-contrast scheme.
fn high_contrast_name(colors: &SystemColors) -> String {
    let id: String = [
        &colors.window,
        &colors.text,
        &colors.highlight,
        &colors.highlight_text,
        &colors.disabled_text,
        &colors.hotlight,
    ]
    .iter()
    .map(|color| color.trim_start_matches('#'))
    .collect();
    format!("{HIGH_CONTRAST} {id}")
}

/// A theme made of the user's high-contrast colors, always in Windows' pairs: window text on the window, highlight
/// text on the highlight. Marks on the window (focus ring, caret, charts, progress) use the window text, which is
/// guaranteed to show there; filled controls (buttons, status) use the highlight with its text. Hovered and selected
/// rows keep the window background and show a highlight border, since a row's text doesn't change color. Status and
/// switch tracks use the window text (with the window color on them), so they show whether drawn bare or filled.
/// Status keeps its words and icons.
fn high_contrast_json(colors: &SystemColors) -> String {
    let dark = luminance(&colors.window) < 0.5;
    let (window, text, highlight, highlight_text) =
        (&colors.window, &colors.text, &colors.highlight, &colors.highlight_text);
    let disabled = &colors.disabled_text;
    let link = &colors.hotlight;
    let surface = [
        "background",
        "accent.background",
        "muted.background",
        "group_box.background",
        "popover.background",
        "secondary.background",
        "secondary.hover.background",
        "secondary.active.background",
        "skeleton.background",
        "list.head.background",
        "list.hover.background",
        "list.active.background",
        "table.background",
        "table.head.background",
        "table.even.background",
        "table.hover.background",
        "table.active.background",
        "sidebar.background",
        "sidebar.accent.background",
        "tab_bar.segmented.background",
        "title_bar.background",
        "status_bar.background",
    ];
    let transparent = [
        "list.background",
        "list.even.background",
        "tab.background",
        "tab.active.background",
        "tab_bar.background",
        "scrollbar.background",
    ];
    let foreground = [
        "foreground",
        "accent.foreground",
        "group_box.foreground",
        "group_box.title.foreground",
        "popover.foreground",
        "secondary.foreground",
        "table.head.foreground",
        "sidebar.foreground",
        "sidebar.accent.foreground",
        "tab.foreground",
        "tab.active.foreground",
    ];
    // Marks drawn straight on the window.
    let marks = [
        "border",
        "window.border",
        "input.border",
        "table.row.border",
        "sidebar.border",
        "title_bar.border",
        "status_bar.border",
        "chart.grid",
        "scrollbar.thumb.background",
        "scrollbar.thumb.hover.background",
        "ring",
        "caret",
        "chart.1",
        "chart.2",
        "chart.4",
        "chart.bullish",
        "chart.bearish",
        "progress.bar.background",
        "base.cyan",
        "base.green",
        "base.yellow",
        "base.red",
        "base.blue",
        "base.magenta",
    ];
    // Status, drawn bare on the window as often as filled: the window text, with the window color on top when filled.
    let status = [
        "danger.background",
        "danger.hover.background",
        "danger.active.background",
        "warning.background",
        "warning.hover.background",
        "warning.active.background",
        "success.background",
        "success.hover.background",
        "success.active.background",
        "info.background",
        "info.hover.background",
        "info.active.background",
        // A switch's unchecked track; its thumb is the window color on it.
        "switch.background",
    ];
    let on_status = [
        "danger.foreground",
        "warning.foreground",
        "success.foreground",
        "info.foreground",
        "switch.thumb.background",
    ];
    // Filled controls: the highlight, with its own text on top.
    let filled = [
        "primary.background",
        "primary.hover.background",
        "primary.active.background",
        "sidebar.primary.background",
        "selection.background",
        "list.active.border",
        "table.active.border",
    ];
    let on_filled = ["primary.foreground", "sidebar.primary.foreground"];
    let mut entries: Vec<String> = Vec::new();
    let mut put = |keys: &[&str], value: &str| {
        entries.extend(keys.iter().map(|key| format!("\"{key}\": \"{value}\"")));
    };
    put(&surface, window);
    put(&transparent, "#00000000");
    put(&foreground, text);
    put(&marks, text);
    put(&status, text);
    put(&on_status, window);
    put(&filled, highlight);
    put(&on_filled, highlight_text);
    // Supporting text keeps the window text: Windows reserves the disabled-text color for disabled UI, and enabled
    // details (limits, resets, descriptions) use `muted.foreground`. Only the quietest chart series takes it.
    put(&["muted.foreground"], text);
    put(&["chart.5"], disabled);
    put(&["chart.3"], link);
    put(&["overlay"], "#00000099");
    format!(
        r#"{{"name": "CodexBar High Contrast", "author": "HemSoft", "themes": [{{
            "name": "{name}", "mode": "{mode}", "font.size": 16, "font.family": "Segoe UI Variable Text",
            "mono_font.family": "Cascadia Mono", "radius": 6, "radius.lg": 10, "shadow": false,
            "colors": {{ {colors} }} }}]}}"#,
        name = high_contrast_name(colors),
        mode = if dark { "dark" } else { "light" },
        colors = entries.join(", ")
    )
}

/// Relative luminance of `#RRGGBB`, 0 for black to 1 for white.
fn luminance(hex: &str) -> f64 {
    let channel = |at: usize| {
        let value = u8::from_str_radix(hex.get(at..at + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: &str, b: &str) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn colors(theme: &str) -> serde_json::Map<String, serde_json::Value> {
        let doc: serde_json::Value = serde_json::from_str(THEME_FILE).unwrap();
        doc["themes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == theme)
            .unwrap()["colors"]
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn both_brand_themes_define_the_same_tokens() {
        let (dark, light) = (colors(DARK), colors(LIGHT));
        let mut a: Vec<&String> = dark.keys().collect();
        let mut b: Vec<&String> = light.keys().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn text_and_actions_keep_their_contrast_in_both_themes() {
        for theme in [DARK, LIGHT] {
            let c = colors(theme);
            let get = |key: &str| c[key].as_str().unwrap().get(..7).unwrap().to_owned();
            // WCAG AA: 4.5:1 for text.
            for (fg, bg) in [
                ("foreground", "background"),
                ("muted.foreground", "background"),
                ("muted.foreground", "group_box.background"),
                ("table.head.foreground", "table.head.background"),
                ("primary.foreground", "primary.background"),
                ("danger.foreground", "danger.background"),
            ] {
                let ratio = contrast(&get(fg), &get(bg));
                assert!(ratio >= 4.5, "{theme}: {fg} on {bg} is {ratio:.2}:1");
            }
            // 3:1 for marks such as chart lines and the focus ring.
            for mark in ["chart.1", "ring", "progress.bar.background"] {
                let ratio = contrast(&get(mark), &get("background"));
                assert!(ratio >= 3.0, "{theme}: {mark} is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn high_contrast_uses_the_system_colors() {
        let scheme = SystemColors {
            window: "#000000".into(),
            text: "#FFFFFF".into(),
            highlight: "#1AEBFF".into(),
            highlight_text: "#000000".into(),
            disabled_text: "#3FF23F".into(),
            hotlight: "#FFFF00".into(),
        };
        let doc: serde_json::Value = serde_json::from_str(&high_contrast_json(&scheme)).unwrap();
        let theme = &doc["themes"][0];
        assert_eq!(theme["mode"], "dark");
        let c = theme["colors"].as_object().unwrap();
        assert_eq!(c["background"], "#000000");
        assert_eq!(c["foreground"], "#FFFFFF");
        assert_eq!(c["primary.background"], "#1AEBFF");
        assert_eq!(c["border"], "#FFFFFF");
        // Pairs only: marks on the window use its text, hovered rows keep the window, disabled text its own color.
        assert_eq!(c["ring"], "#FFFFFF");
        assert_eq!(c["chart.1"], "#FFFFFF");
        assert_eq!(c["table.hover.background"], "#000000");
        assert_eq!(c["table.active.border"], "#1AEBFF");
        assert_eq!(c["primary.foreground"], "#000000");
        assert_eq!(c["muted.foreground"], "#FFFFFF", "secondary text stays readable");
        assert_eq!(c["danger.background"], "#FFFFFF", "status shows bare on the window");
        assert_eq!(c["danger.foreground"], "#000000");
        assert_eq!(c["switch.background"], "#FFFFFF");
        assert_eq!(c["switch.thumb.background"], "#000000");
        assert_ne!(
            high_contrast_name(&scheme),
            HIGH_CONTRAST,
            "each scheme has its own name"
        );
        // Every brand token is covered, so nothing keeps a brand color under high contrast.
        let brand = colors(DARK);
        let missing: Vec<&String> = brand.keys().filter(|key| !c.contains_key(*key)).collect();
        assert!(missing.is_empty(), "{missing:?}");
        let white = SystemColors {
            window: "#FFFFFF".into(),
            text: "#000000".into(),
            ..scheme
        };
        let doc: serde_json::Value = serde_json::from_str(&high_contrast_json(&white)).unwrap();
        assert_eq!(doc["themes"][0]["mode"], "light");
    }
}
