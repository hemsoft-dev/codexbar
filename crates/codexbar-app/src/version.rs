//! CodexBar's version as the title bar, the window title and Settings › About show it (#244): the package scheme
//! `package.ps1` uses (`version_scheme`), for every build.

use crate::package::Installed;

/// The version this executable was built as: what `package.ps1` stamped into it, else the package scheme at the
/// build's commit (see `build.rs`).
pub const BUILT: &str = env!("CODEXBAR_VERSION");

/// The version to show: the installed package's own, which a rollback or `-Revision` can set, else the built one.
pub fn current() -> &'static str {
    shown(crate::package::installed())
}

fn shown(installed: Option<&Installed>) -> &str {
    installed.map_or(BUILT, |installed| installed.version.as_str())
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
    fn the_built_version_is_the_package_scheme_at_this_commit() {
        let cargo = env!("CARGO_PKG_VERSION");
        match env!("CODEXBAR_VERSION_SOURCE") {
            "git" => {
                let output = std::process::Command::new("git")
                    .args(["rev-list", "--count", "HEAD"])
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .expect("git ran at build time, so it runs now");
                let commits = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse()
                    .expect("a commit count");
                assert_eq!(BUILT, package_style(cargo, Some(commits)));
            }
            "no-git" => assert_eq!(BUILT, package_style(cargo, None)),
            // package.ps1's own version, whatever -Revision gave it.
            source => assert_eq!(source, "stamped"),
        }
    }

    #[test]
    fn an_installed_package_shows_its_own_version() {
        let installed = Installed {
            version: "1.1.0.150".into(),
            family: "HemSoft.CodexBar_abc123".into(),
            app_installer: None,
        };
        assert_eq!(
            shown(Some(&installed)),
            "1.1.0.150",
            "a rollback shows the version it went back to"
        );
        assert_eq!(shown(None), BUILT, "unpackaged builds show the built version");
    }

    #[test]
    fn the_title_names_the_app_and_its_version() {
        assert_eq!(title(), format!("CodexBar {}", current()));
    }
}
