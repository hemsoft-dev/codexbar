//! The package version scheme `package.ps1` uses (#93), shared by `build.rs` and the app (#244).

/// The package-style version: Cargo's major part plus one (App Installer rejects a zero major part), its minor and
/// patch, then the commit count when there is one. Cargo `0.1.0` at commit 312 is `1.1.0.312`; without git it is
/// `1.1.0`. A Cargo version that isn't `major.minor.patch` is returned unchanged.
pub fn package_style(cargo: &str, commits: Option<u64>) -> String {
    let parts: Vec<&str> = cargo.split('.').collect();
    let [major, minor, patch] = parts.as_slice() else {
        return cargo.to_owned();
    };
    let (Ok(major), Ok(minor), Ok(patch)) = (major.parse::<u64>(), minor.parse::<u64>(), patch.parse::<u64>()) else {
        return cargo.to_owned();
    };
    let base = format!("{}.{minor}.{patch}", major + 1);
    match commits {
        Some(commits) => format!("{base}.{commits}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_style_adds_one_to_the_major_part_and_appends_the_commit_count() {
        assert_eq!(package_style("0.1.0", Some(312)), "1.1.0.312");
        assert_eq!(package_style("2.4.7", Some(9)), "3.4.7.9");
    }

    #[test]
    fn package_style_without_git_leaves_out_the_commit_count() {
        assert_eq!(package_style("0.1.0", None), "1.1.0");
    }

    #[test]
    fn package_style_keeps_a_version_it_cannot_read() {
        assert_eq!(package_style("0.1.0-beta.1", Some(3)), "0.1.0-beta.1");
        assert_eq!(package_style("1.2", Some(3)), "1.2");
    }
}
