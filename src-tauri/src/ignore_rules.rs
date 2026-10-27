//! Ignore rules shared by directory uploads and downloads.
//!
//! Git ignore files are parsed with the same implementation used by ripgrep so
//! negation, anchoring, `**`, and directory-only patterns keep their usual
//! semantics. A small conservative set of temporary artifacts is always
//! ignored, including when a project has no `.gitignore`.

use std::path::Path;
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

const BUILTIN_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".hypothesis",
    ".tox",
    ".nox",
];

const BUILTIN_IGNORED_FILES: &[&str] = &[
    ".ds_store",
    "thumbs.db",
    "desktop.ini",
    ".coverage",
    ".coverage.xml",
];

const BUILTIN_IGNORED_SUFFIXES: &[&str] = &[
    ".pyc",
    ".pyo",
    ".pyd",
    ".class",
    ".o",
    ".obj",
    ".swp",
    ".swo",
    ".tmp",
    ".temp",
    ".part",
    ".crdownload",
];

/// One parsed `.gitignore` and the remote directory it applies to.
#[derive(Clone)]
pub(crate) struct IgnoreLayer {
    pub base: String,
    pub matcher: Arc<Gitignore>,
}

pub(crate) fn build_gitignore(content: &str) -> Option<Arc<Gitignore>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut builder = GitignoreBuilder::new("");
    for line in content.lines() {
        if let Err(error) = builder.add_line(None, line) {
            log::warn!("ignoring an invalid .gitignore pattern {line:?}: {error}");
        }
    }
    match builder.build() {
        Ok(matcher) => Some(Arc::new(matcher)),
        Err(error) => {
            log::warn!("failed to build .gitignore rules: {error}");
            None
        }
    }
}

pub(crate) fn is_builtin_ignored(name: &str, is_dir: bool) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_dir && (BUILTIN_IGNORED_DIRS.contains(&lower.as_str()) || lower.ends_with(".egg-info")) {
        return true;
    }
    if BUILTIN_IGNORED_FILES.contains(&lower.as_str()) {
        return true;
    }
    if lower.starts_with(".coverage.") || name.ends_with('~') {
        return true;
    }
    !is_dir
        && BUILTIN_IGNORED_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
}

/// Applies non-overridable built-in defaults, then every `.gitignore` from the
/// outer directory down so the closest file wins, matching Git's precedence.
pub(crate) fn is_path_ignored(
    path: &str,
    name: &str,
    is_dir: bool,
    layers: &[IgnoreLayer],
) -> bool {
    if is_builtin_ignored(name, is_dir) {
        return true;
    }
    let mut ignored = false;
    for layer in layers {
        let Some(relative) = relative_to(&layer.base, path) else {
            continue;
        };
        match layer.matcher.matched(Path::new(relative), is_dir) {
            ignore::Match::Ignore(_) => ignored = true,
            ignore::Match::Whitelist(_) => ignored = false,
            ignore::Match::None => {}
        }
    }
    ignored
}

fn relative_to<'a>(base: &str, path: &'a str) -> Option<&'a str> {
    if base == path {
        return Some("");
    }
    if base == "/" {
        return path.strip_prefix('/');
    }
    path.strip_prefix(base)?.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_cover_common_temporary_artifacts() {
        assert!(is_builtin_ignored("__pycache__", true));
        assert!(is_builtin_ignored("module.pyc", false));
        assert!(is_builtin_ignored("Thumbs.db", false));
        assert!(is_builtin_ignored("project.egg-info", true));
        assert!(!is_builtin_ignored("notes.txt", false));
        assert!(!is_builtin_ignored("src", true));
    }

    #[test]
    fn gitignore_layers_follow_nested_precedence_and_negation() {
        let layers = vec![
            IgnoreLayer {
                base: "/project".to_owned(),
                matcher: build_gitignore("*.log\n*.pyc\nbuild/\n!keep.log\n").expect("root rules"),
            },
            IgnoreLayer {
                base: "/project/sub".to_owned(),
                matcher: build_gitignore("!module.log\n").expect("nested rules"),
            },
        ];

        assert!(is_path_ignored(
            "/project/sub/other.pyc",
            "other.pyc",
            false,
            &layers
        ));
        assert!(is_path_ignored(
            "/project/sub/drop.log",
            "drop.log",
            false,
            &layers
        ));
        assert!(!is_path_ignored(
            "/project/sub/module.log",
            "module.log",
            false,
            &layers
        ));
        assert!(!is_path_ignored(
            "/project/keep.log",
            "keep.log",
            false,
            &layers
        ));
        assert!(is_path_ignored("/project/build", "build", true, &layers));
        assert!(is_builtin_ignored("module.pyc", false));
    }

    #[test]
    fn builtin_temporary_rules_cannot_be_reincluded() {
        let layers = vec![IgnoreLayer {
            base: "/project".to_owned(),
            matcher: build_gitignore("!fixture.pyc\n").expect("rules"),
        }];
        assert!(is_path_ignored(
            "/project/fixture.pyc",
            "fixture.pyc",
            false,
            &layers
        ));
    }

    #[test]
    fn anchored_gitignore_patterns_stay_relative_to_their_directory() {
        let layers = vec![IgnoreLayer {
            base: "/project".to_owned(),
            matcher: build_gitignore("/anchored.txt\nsub/*.log\n").expect("rules"),
        }];
        assert!(is_path_ignored(
            "/project/anchored.txt",
            "anchored.txt",
            false,
            &layers
        ));
        assert!(!is_path_ignored(
            "/project/nested/anchored.txt",
            "anchored.txt",
            false,
            &layers
        ));
        assert!(is_path_ignored(
            "/project/sub/drop.log",
            "drop.log",
            false,
            &layers
        ));
    }
}
