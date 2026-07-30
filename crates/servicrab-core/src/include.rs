//! `include` — one config spread over several files.
//!
//! An included file is a *fragment*: `[services.<name>]` tables, and possibly
//! an `include` of its own.  `version` and `[project]` stay in the root config,
//! which is the only file the daemon is ever pointed at.
//!
//! Relative paths inside a fragment — its own `include`, and every `cwd` and
//! `env_file` it declares — resolve against the fragment's own directory, so a
//! fragment can live next to the code it describes and be moved with it.  That
//! is why each service remembers the file it came from in
//! [`RawService::origin`].
//!
//! Merging is not overriding: two files declaring the same service is an error.
//! An `include` that silently replaced a service would be a fine way to spend
//! an afternoon wondering which file is in charge.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::raw::{RawConfig, RawFragment, RawInclude, RawService};

/// Read every file `raw` includes, directly or transitively, and merge their
/// services into it.
///
/// `root` is the path `raw` was parsed from.  All errors are collected: a typo
/// in one fragment should not hide a typo in the next.
pub(crate) fn merge(raw: &mut RawConfig, root: &Path) -> Vec<ConfigError> {
    for service in raw.services.values_mut() {
        service.origin = Some(root.to_path_buf());
    }

    let mut loader = Loader {
        services: std::mem::take(&mut raw.services),
        chain: vec![Ancestor {
            identity: identity(root),
            path: root.to_path_buf(),
        }],
        merged: BTreeMap::new(),
        errors: Vec::new(),
    };
    loader.include(raw.include.take().as_ref(), root);

    raw.services = loader.services;
    loader.errors
}

struct Loader {
    /// Every service seen so far, each remembering which file declared it.
    services: BTreeMap<String, RawService>,
    /// The files being read right now, outermost first.
    chain: Vec<Ancestor>,
    /// Every file already merged, and what included it.
    merged: BTreeMap<PathBuf, PathBuf>,
    errors: Vec<ConfigError>,
}

/// A file on the current include path.
struct Ancestor {
    /// What the file *is*, for comparison.
    identity: PathBuf,
    /// What the file was called, for the error message.
    path: PathBuf,
}

impl Loader {
    /// Follow the `include` of the file at `from`.
    fn include(&mut self, include: Option<&RawInclude>, from: &Path) {
        let dir = from.parent().unwrap_or(Path::new("."));
        for relative in include.iter().flat_map(|include| include.paths()) {
            self.one(&dir.join(relative), from);
        }
    }

    /// Merge one included file, then whatever it includes in turn.
    fn one(&mut self, path: &Path, included_by: &Path) {
        let identity = identity(path);

        if let Some(at) = self.chain.iter().position(|a| a.identity == identity) {
            let mut cycle: Vec<String> = self.chain[at..]
                .iter()
                .map(|a| a.path.display().to_string())
                .collect();
            cycle.push(path.display().to_string());
            self.errors.push(ConfigError::IncludeCycle {
                cycle: cycle.join(" -> "),
            });
            return;
        }

        if let Some(first) = self.merged.get(&identity) {
            self.errors.push(ConfigError::IncludeTwice {
                path: path.to_path_buf(),
                first: first.clone(),
                second: included_by.to_path_buf(),
            });
            return;
        }

        let Some(fragment) = self.read(path, included_by) else {
            return;
        };
        self.merged
            .insert(identity.clone(), included_by.to_path_buf());

        if fragment.version.is_some() {
            self.not_a_fragment(path, included_by, "version");
        }
        if fragment.project.is_some() {
            self.not_a_fragment(path, included_by, "[project]");
        }

        for (name, mut service) in fragment.services {
            service.origin = Some(path.to_path_buf());
            match self.services.entry(name) {
                Entry::Vacant(entry) => {
                    entry.insert(service);
                }
                Entry::Occupied(entry) => {
                    // Every service in the map carries an origin by now: the
                    // root's were stamped before the walk started.
                    let first = entry.get().origin.clone().unwrap_or_default();
                    self.errors.push(ConfigError::DuplicateService {
                        service: entry.key().clone(),
                        first,
                        second: path.to_path_buf(),
                    });
                }
            }
        }

        self.chain.push(Ancestor {
            identity,
            path: path.to_path_buf(),
        });
        self.include(fragment.include.as_ref(), path);
        self.chain.pop();
    }

    fn read(&mut self, path: &Path, included_by: &Path) -> Option<RawFragment> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) => {
                self.errors.push(ConfigError::IncludeRead {
                    included_by: included_by.to_path_buf(),
                    path: path.to_path_buf(),
                    source,
                });
                return None;
            }
        };

        match toml::from_str(&text) {
            Ok(fragment) => Some(fragment),
            Err(source) => {
                self.errors.push(ConfigError::Parse {
                    path: path.to_path_buf(),
                    source,
                });
                None
            }
        }
    }

    fn not_a_fragment(&mut self, path: &Path, included_by: &Path, field: &str) {
        self.errors.push(ConfigError::IncludeNotAFragment {
            path: path.to_path_buf(),
            included_by: included_by.to_path_buf(),
            field: field.to_string(),
        });
    }
}

/// What a path *is*, so that `./a.toml`, `a.toml` and `b/../a.toml` are one
/// file and a cycle through a symlink is still a cycle.
///
/// Falls back to the path as written when it cannot be resolved — a file that
/// does not exist is about to be reported as unreadable anyway.
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write `files` into a fresh directory and merge the includes of the first
    /// one, which is the root config.
    fn merge_files(files: &[(&str, &str)]) -> (TempDir, RawConfig, Vec<ConfigError>) {
        let dir = TempDir::new().unwrap();
        for (name, contents) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }

        let root = dir.path().join(files[0].0);
        let mut raw: RawConfig = toml::from_str(files[0].1).expect("the root config parses");
        let errors = merge(&mut raw, &root);
        (dir, raw, errors)
    }

    /// Merge, expecting success.
    fn ok(files: &[(&str, &str)]) -> (TempDir, RawConfig) {
        let (dir, raw, errors) = merge_files(files);
        assert!(
            errors.is_empty(),
            "{:?}",
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        (dir, raw)
    }

    /// Merge, expecting exactly one error.
    fn one_error(files: &[(&str, &str)]) -> ConfigError {
        let (_dir, _raw, mut errors) = merge_files(files);
        assert_eq!(
            errors.len(),
            1,
            "{:?}",
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        errors.remove(0)
    }

    const ROOT: &str = r#"
version = 1
include = ["services/db.toml"]
[project]
name = "demo"
"#;

    const DB: &str = r#"
[services.db]
command = ["postgres"]
"#;

    #[test]
    fn an_included_service_joins_the_config() {
        let (dir, raw) = ok(&[("servicrab.toml", ROOT), ("services/db.toml", DB)]);

        assert_eq!(raw.services.keys().collect::<Vec<_>>(), ["db"]);
        assert_eq!(
            raw.services["db"].origin.as_deref(),
            Some(dir.path().join("services/db.toml").as_path()),
            "the service should remember the file it came from"
        );
        assert!(
            raw.include.is_none(),
            "the include should be spent, not left for validation to trip over"
        );
    }

    #[test]
    fn a_single_path_needs_no_list() {
        let root = "version = 1\ninclude = \"services/db.toml\"\n[project]\nname = \"demo\"\n";
        let (_dir, raw) = ok(&[("servicrab.toml", root), ("services/db.toml", DB)]);

        assert_eq!(raw.services.keys().collect::<Vec<_>>(), ["db"]);
    }

    #[test]
    fn a_fragment_may_include_further_files() {
        let db = "include = [\"cache.toml\"]\n[services.db]\ncommand = [\"postgres\"]\n";
        let cache = "[services.cache]\ncommand = [\"redis-server\"]\n";
        let (dir, raw) = ok(&[
            ("servicrab.toml", ROOT),
            ("services/db.toml", db),
            ("services/cache.toml", cache),
        ]);

        assert_eq!(raw.services.keys().collect::<Vec<_>>(), ["cache", "db"]);
        assert_eq!(
            raw.services["cache"].origin.as_deref(),
            Some(dir.path().join("services/cache.toml").as_path()),
            "a nested include resolves against the fragment, not the root"
        );
    }

    #[test]
    fn the_root_keeps_its_own_services() {
        let root = format!("{ROOT}[services.api]\ncommand = [\"api\"]\n");
        let (dir, raw) = ok(&[("servicrab.toml", &root), ("services/db.toml", DB)]);

        assert_eq!(raw.services.keys().collect::<Vec<_>>(), ["api", "db"]);
        assert_eq!(
            raw.services["api"].origin.as_deref(),
            Some(dir.path().join("servicrab.toml").as_path())
        );
    }

    #[test]
    fn two_files_may_not_declare_the_same_service() {
        let root = format!("{ROOT}[services.db]\ncommand = [\"postgres\"]\n");
        let err = one_error(&[("servicrab.toml", &root), ("services/db.toml", DB)]);

        assert!(
            matches!(&err, ConfigError::DuplicateService { service, first, second }
                if service == "db"
                    && first.ends_with("servicrab.toml")
                    && second.ends_with("services/db.toml")),
            "{err}"
        );
    }

    #[test]
    fn a_missing_include_names_who_asked_for_it() {
        let err = one_error(&[("servicrab.toml", ROOT)]);

        assert!(
            matches!(&err, ConfigError::IncludeRead { included_by, path, .. }
                if included_by.ends_with("servicrab.toml")
                    && path.ends_with("services/db.toml")),
            "{err}"
        );
    }

    #[test]
    fn a_fragment_with_broken_toml_is_reported_against_the_fragment() {
        let err = one_error(&[
            ("servicrab.toml", ROOT),
            ("services/db.toml", "not ][ toml"),
        ]);

        assert!(
            matches!(&err, ConfigError::Parse { path, .. } if path.ends_with("services/db.toml")),
            "{err}"
        );
    }

    #[test]
    fn a_fragment_may_not_declare_the_project() {
        let db = format!("[project]\nname = \"other\"\n{DB}");
        let err = one_error(&[("servicrab.toml", ROOT), ("services/db.toml", &db)]);

        assert!(
            matches!(&err, ConfigError::IncludeNotAFragment { field, path, .. }
                if field == "[project]" && path.ends_with("services/db.toml")),
            "{err}"
        );
    }

    #[test]
    fn a_fragment_may_not_declare_a_version() {
        let db = format!("version = 1\n{DB}");
        let err = one_error(&[("servicrab.toml", ROOT), ("services/db.toml", &db)]);

        assert!(
            matches!(&err, ConfigError::IncludeNotAFragment { field, .. } if field == "version"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_field_in_a_fragment_is_still_a_typo() {
        let err = one_error(&[
            ("servicrab.toml", ROOT),
            ("services/db.toml", "servcies = {}\n"),
        ]);

        let ConfigError::Parse { source, .. } = &err else {
            panic!("expected a parse error, got {err}");
        };
        assert!(source.to_string().contains("servcies"), "{source}");
    }

    #[test]
    fn a_file_that_includes_itself_is_a_cycle() {
        let db = format!("include = [\"db.toml\"]\n{DB}");
        let err = one_error(&[("servicrab.toml", ROOT), ("services/db.toml", &db)]);

        let ConfigError::IncludeCycle { cycle } = &err else {
            panic!("expected a cycle, got {err}");
        };
        assert_eq!(cycle.matches("db.toml").count(), 2, "{cycle}");
    }

    #[test]
    fn a_cycle_back_to_the_root_is_a_cycle() {
        let db = format!("include = [\"../servicrab.toml\"]\n{DB}");
        let err = one_error(&[("servicrab.toml", ROOT), ("services/db.toml", &db)]);

        let ConfigError::IncludeCycle { cycle } = &err else {
            panic!("expected a cycle, got {err}");
        };
        assert!(cycle.contains("servicrab.toml"), "{cycle}");
        assert!(cycle.contains("db.toml"), "{cycle}");
    }

    #[test]
    fn a_file_reached_twice_is_reported_once_and_clearly() {
        // A diamond: two fragments include the same third file.  Its services
        // would otherwise collide with themselves.
        let root = "version = 1\ninclude = [\"a.toml\", \"b.toml\"]\n[project]\nname = \"demo\"\n";
        let side = "include = [\"shared.toml\"]\n";
        let err = one_error(&[
            ("servicrab.toml", root),
            ("a.toml", side),
            ("b.toml", side),
            ("shared.toml", DB),
        ]);

        assert!(
            matches!(&err, ConfigError::IncludeTwice { path, first, second }
                if path.ends_with("shared.toml")
                    && first.ends_with("a.toml")
                    && second.ends_with("b.toml")),
            "{err}"
        );
    }

    #[test]
    fn every_broken_fragment_is_reported_at_once() {
        // `b.toml` is missing entirely, which must not hide the typo in
        // `a.toml`: one `servicrab check` should list both.
        let root = "version = 1\ninclude = [\"a.toml\", \"b.toml\"]\n[project]\nname = \"demo\"\n";
        let (_dir, _raw, errors) = merge_files(&[("servicrab.toml", root), ("a.toml", "not ][")]);

        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            matches!(errors[0], ConfigError::Parse { .. }),
            "{messages:?}"
        );
        assert!(
            matches!(errors[1], ConfigError::IncludeRead { .. }),
            "{messages:?}"
        );
    }
}
