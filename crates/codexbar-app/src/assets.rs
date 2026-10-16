//! The app's assets: gpui-kit's icons, plus the provider logos in `assets/brand` (#92), embedded in the executable.

use std::borrow::Cow;

use codexbar_core::Provider;
use gpui_kit::{AssetSource, Result, SharedString};

/// Where a provider's logo is served from.
pub fn logo_path(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "brand/openai.svg",
        Provider::Claude => "brand/claude.svg",
        Provider::Copilot => "brand/githubcopilot.svg",
        Provider::Cursor => "brand/cursor.svg",
        Provider::OpenRouter => "brand/openrouter.svg",
        Provider::OpenCode => "brand/opencode.svg",
        Provider::Moonshot => "brand/kimi.svg",
        Provider::Greptile => "brand/greptile.svg",
        Provider::GitHubBilling => "brand/github.svg",
        Provider::Gemini => "brand/gemini.svg",
        Provider::Grok => "brand/grok.svg",
        Provider::XSubscription => "brand/xai.svg",
    }
}

const LOGOS: [(&str, &[u8]); 12] = [
    ("brand/openai.svg", include_bytes!("../assets/brand/openai.svg")),
    ("brand/claude.svg", include_bytes!("../assets/brand/claude.svg")),
    (
        "brand/githubcopilot.svg",
        include_bytes!("../assets/brand/githubcopilot.svg"),
    ),
    ("brand/cursor.svg", include_bytes!("../assets/brand/cursor.svg")),
    ("brand/openrouter.svg", include_bytes!("../assets/brand/openrouter.svg")),
    ("brand/opencode.svg", include_bytes!("../assets/brand/opencode.svg")),
    ("brand/kimi.svg", include_bytes!("../assets/brand/kimi.svg")),
    ("brand/greptile.svg", include_bytes!("../assets/brand/greptile.svg")),
    ("brand/github.svg", include_bytes!("../assets/brand/github.svg")),
    ("brand/gemini.svg", include_bytes!("../assets/brand/gemini.svg")),
    ("brand/grok.svg", include_bytes!("../assets/brand/grok.svg")),
    ("brand/xai.svg", include_bytes!("../assets/brand/xai.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = LOGOS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_kit::assets::Assets.list(path)?;
        names.extend(
            LOGOS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_has_an_embedded_svg_logo() {
        for provider in Provider::ALL {
            let bytes = Assets.load(logo_path(provider)).unwrap().expect("logo");
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.starts_with("<svg") && text.contains(r#"viewBox="0 0 24 24""#),
                "{provider:?}"
            );
            assert!(
                !text.contains("<script") && !text.contains("href="),
                "{provider:?}: plain shapes only"
            );
        }
    }

    #[test]
    fn gpui_kit_icons_still_load_and_logos_are_listed() {
        assert!(Assets.load("icons/info.svg").unwrap().is_some());
        let listed = Assets.list("brand/").unwrap();
        assert_eq!(listed.len(), Provider::ALL.len());
    }
}
