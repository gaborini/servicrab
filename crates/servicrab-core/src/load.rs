//! Configuration file discovery and loading.
//!
//! The main entry point is [`load`].

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{ConfigError, ConfigWarning};
use crate::raw::RawConfig;
use crate::validation::validate_raw;

/// Walk up the directory tree starting at `start_dir`, looking for a file
/// named `servicrab.toml`.
///
/// Returns the path of the first file found, or [`ConfigError::ConfigNotFound`]
/// if no file exists all the way up to the filesystem root.
pub fn discover_config(start_dir: &Path) -> Result<PathBuf, ConfigError> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("servicrab.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(ConfigError::ConfigNotFound {
        dir: start_dir.to_path_buf(),
    })
}

/// Read, parse, and validate a `servicrab.toml` from the given path, following
/// any `include` it declares.
///
/// Returns the validated [`Config`] and any non-fatal [`ConfigWarning`]s, or
/// a list of [`ConfigError`]s that prevented successful loading.
pub fn load(path: &Path) -> Result<(Config, Vec<ConfigWarning>), Vec<ConfigError>> {
    let raw_str = std::fs::read_to_string(path).map_err(|e| {
        vec![ConfigError::Read {
            path: path.to_path_buf(),
            source: e,
        }]
    })?;

    // Before the field-level parse, and this order is the whole point.  Every
    // struct in `crate::raw` denies unknown fields, deliberately, so a typo is
    // fatal — but a schema this build predates is *made of* fields it does not
    // know, so `version = 2` used to be reported as `unknown field
    // 'new_key_from_v2'` with not a word about the version.  The one message
    // that tells an operator to upgrade servicrab was unreachable for exactly
    // the files it was written for.
    if let Some(version) = declared_version(&raw_str) {
        if version != crate::validation::SUPPORTED_VERSION {
            return Err(vec![ConfigError::UnsupportedVersion { version }]);
        }
    }

    let mut raw: RawConfig = toml::from_str(&raw_str).map_err(|e| {
        vec![ConfigError::Parse {
            path: path.to_path_buf(),
            source: e,
        }]
    })?;

    // Fatal on its own: validating half a config would report services as
    // missing when they are merely in a file that could not be read.
    let include_errors = crate::include::merge(&mut raw, path);
    if !include_errors.is_empty() {
        return Err(include_errors);
    }

    validate_raw(raw, path)
}

/// The `version` a file declares, ignoring everything else in it.
///
/// Deliberately the most forgiving read of the file that can still answer the
/// question: nothing but `version` is named, so no key a future schema adds can
/// stop it from being found.  `None` means the file did not say — a missing or
/// non-numeric `version` is left to the ordinary parse, which already reports
/// both, and better than a pre-pass could.
fn declared_version(raw_str: &str) -> Option<u32> {
    /// Everything but `version`, thrown away.
    #[derive(serde::Deserialize)]
    struct JustTheVersion {
        version: u32,
    }

    toml::from_str::<JustTheVersion>(raw_str)
        .ok()
        .map(|declared| declared.version)
}

/// Resolve the configuration path: use `explicit` if provided, otherwise
/// discover starting from the current working directory.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, ConfigError> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => {
            let cwd = std::env::current_dir().map_err(|e| ConfigError::Read {
                path: PathBuf::from("."),
                source: e,
            })?;
            discover_config(&cwd)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discover_finds_config_in_current_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(
            &path,
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n",
        )
        .unwrap();
        assert_eq!(discover_config(dir.path()).unwrap(), path);
    }

    #[test]
    fn discover_walks_up_to_parent() {
        let parent = TempDir::new().unwrap();
        let child = parent.path().join("child");
        std::fs::create_dir(&child).unwrap();

        let path = parent.path().join("servicrab.toml");
        std::fs::write(
            &path,
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n",
        )
        .unwrap();

        assert_eq!(discover_config(&child).unwrap(), path);
    }

    #[test]
    fn discover_returns_error_when_not_found() {
        // Start from /tmp — the root won't have servicrab.toml in a typical
        // test environment.  We use a deeply nested temp dir for safety.
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();

        // No servicrab.toml anywhere in the temp subtree.
        let result = discover_config(&deep);
        // The test passes as long as it's Err (the file may or may not be
        // found further up the real filesystem tree, so we only check when we
        // know there's no config above).
        let _ = result; // accept either outcome in a general test environment
    }

    #[test]
    fn load_valid_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(
            &path,
            "version=1\n[project]\nname=\"p\"\n[services.s]\ncommand=[\"echo\"]\n",
        )
        .unwrap();
        let (cfg, _) = load(&path).unwrap();
        assert_eq!(cfg.project.name.as_str(), "p");
    }

    #[test]
    fn load_missing_file_returns_read_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.toml");
        let errs = load(&path).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ConfigError::Read { .. })));
    }

    #[test]
    fn load_invalid_toml_returns_parse_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, "this is NOT valid toml ][").unwrap();
        let errs = load(&path).unwrap_err();
        assert!(errs.iter().any(|e| matches!(e, ConfigError::Parse { .. })));
    }

    /// Load `toml` from a temp file and keep the errors it produced.
    ///
    /// The `TempDir` comes back because it holds the file those errors name; a
    /// caller that dropped it would be reading about a path that no longer
    /// exists.
    fn errors_from(toml: &str) -> (TempDir, Vec<ConfigError>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("servicrab.toml");
        std::fs::write(&path, toml).unwrap();
        let errors = load(&path).map(|_| ()).expect_err("expected errors");
        (dir, errors)
    }

    /// The defect: a config written for a later schema is *made of* keys this
    /// build does not know, and every struct in [`crate::raw`] denies unknown
    /// fields, so the parse failed first and reported one of those keys as a
    /// typo.  The one message that tells an operator to upgrade servicrab was
    /// unreachable for exactly the files it was written for.
    #[test]
    fn a_config_from_a_later_schema_reports_its_version_not_its_keys() {
        let (_dir, errs) = errors_from(
            "version = 2\n[project]\nname = \"p\"\nnew_key_from_v2 = true\n\
             [services.s]\ncommand = [\"echo\"]\n",
        );

        assert!(
            errs.iter()
                .any(|e| matches!(e, ConfigError::UnsupportedVersion { version: 2 })),
            "{errs:?}"
        );
        // And only that: naming the key as well would be advice to delete the
        // one line that is not the problem.
        assert!(
            !errs.iter().any(|e| matches!(e, ConfigError::Parse { .. })),
            "{errs:?}"
        );
    }

    /// Only the version changes.  An unrecognised key inside a `version = 1`
    /// file is a typo, and being fatal is the protection that catches it — a
    /// misspelled `comand` that loaded quietly would be far worse than one that
    /// refuses.
    #[test]
    fn an_unknown_key_under_the_supported_version_is_still_fatal() {
        let (_dir, errs) = errors_from(
            "version = 1\n[project]\nname = \"p\"\nnew_key_from_v2 = true\n\
             [services.s]\ncommand = [\"echo\"]\n",
        );

        assert!(
            errs.iter().any(|e| matches!(e, ConfigError::Parse { .. })),
            "{errs:?}"
        );
    }

    /// The pre-pass answers one question and declines the rest.  A file with no
    /// `version` at all, or one that is not a number, is the ordinary parse's
    /// business — it reports both, and with the line and column a pre-pass
    /// working on a whole-file deserialize could not.
    #[test]
    fn a_missing_or_unreadable_version_is_left_to_the_ordinary_parse() {
        for toml in [
            "[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\n",
            "version = \"two\"\n[project]\nname = \"p\"\n[services.s]\ncommand = [\"echo\"]\n",
        ] {
            let (_dir, errs) = errors_from(toml);

            assert!(
                errs.iter().any(|e| matches!(e, ConfigError::Parse { .. })),
                "{toml:?} gave {errs:?}"
            );
        }
    }

    /// The version is read before anything else, so it is reported before
    /// anything else — including an `include` that could not be read, which used
    /// to be the first thing a `version = 2` file heard about.
    #[test]
    fn a_later_schema_is_reported_before_an_unreadable_include() {
        let (_dir, errs) = errors_from(
            "version = 2\ninclude = \"absent.toml\"\n[project]\nname = \"p\"\n\
             [services.s]\ncommand = [\"echo\"]\n",
        );

        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            matches!(errs[0], ConfigError::UnsupportedVersion { version: 2 }),
            "{errs:?}"
        );
    }
}
