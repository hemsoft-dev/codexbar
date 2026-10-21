//! CodexBar's version as the title bar, the window title and Settings › About show it (#244): the package scheme
//! `package.ps1` uses (`version_scheme`), for every build.

/// The version this executable was built as: what `package.ps1` stamped into it, else the package scheme at the
/// build's commit (see `build.rs`).
pub const BUILT: &str = env!("CODEXBAR_VERSION");

/// The version to show: the installed package's own, which a rollback or `-Revision` can set, else the built one.
pub fn current() -> &'static str {
    crate::package::installed().map_or(BUILT, |installed| installed.version.as_str())
}

/// "CodexBar 1.1.0.312", for the window title.
pub fn title() -> String {
    format!("CodexBar {}", current())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version_scheme::package_style;

    #[test]
    fn the_built_version_follows_the_package_scheme() {
        let base = package_style(env!("CARGO_PKG_VERSION"), None);
        let commits = BUILT.strip_prefix(&format!("{base}."));
        assert!(
            BUILT == base || commits.is_some_and(|n| n.parse::<u64>().is_ok()),
            "{BUILT} is {base} or {base}.<commits>"
        );
    }

    #[test]
    fn the_title_names_the_app_and_its_version() {
        assert_eq!(title(), format!("CodexBar {}", current()));
    }
}
