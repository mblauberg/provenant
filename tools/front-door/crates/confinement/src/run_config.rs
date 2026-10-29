//! `[domains.run]`: host-owned settings for worktree-write lanes: extra write
//! paths, and the repository paths a writer may change without forcing review.
use crate::permissions::{PathGrant, PathScope, is_credential_root};
use crate::review_exempt::{MAX_PATTERNS, ReviewExempt, valid_pattern};
use provenant_config::{DomainConfig, DomainSection, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const MAX_WRITER_WRITES: usize = 16;
/// Paths every implementation legitimately changes: release-note fragments,
/// specs, lock files and the tracker's input file. None is a control file.
pub const DEFAULT_REVIEW_EXEMPT: [&str; 4] = [
    "docs/release-notes/next.d/",
    "docs/specs/",
    "Cargo.lock",
    ".tracker-input",
];
const DEFAULTS: &str = r#"writer_writes = []
review_exempt = ["docs/release-notes/next.d/", "docs/specs/", "Cargo.lock", ".tracker-input"]

[projects]
"#;

/// Shared run state kept outside the worktree, such as a lock directory a
/// repository script locks with `/usr/bin/lockf`, or a shared cargo target.
/// Each directory is read-write for writer lanes only; read-only lanes ignore
/// it. Plan compilation still refuses any overlap with protected roots.
///
/// `review_exempt` lists repository-relative paths a writer may commit without
/// the lane ending `review_required`, and may leave uncommitted without it
/// ending `uncommitted`: `dir/` for a directory at the root, a bare name for a
/// file at any depth, otherwise one exact path. A project can replace the list
/// under `[domains.run.projects."PROJECT_ID"]`. Git metadata, hooks and
/// provider control roots can never be listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    #[serde(default)]
    pub writer_writes: Vec<PathBuf>,
    #[serde(default = "default_review_exempt")]
    pub review_exempt: Vec<String>,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRunConfig>,
}
/// One project's replacement for the host-wide settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRunConfig {
    #[serde(default)]
    pub review_exempt: Option<Vec<String>>,
}
fn default_review_exempt() -> Vec<String> {
    DEFAULT_REVIEW_EXEMPT
        .iter()
        .map(|p| (*p).to_owned())
        .collect()
}
impl Default for RunConfig {
    fn default() -> Self {
        Self {
            writer_writes: Vec::new(),
            review_exempt: default_review_exempt(),
            projects: BTreeMap::new(),
        }
    }
}
impl RunConfig {
    pub fn writer_write_roots(&self) -> impl Iterator<Item = &Path> {
        self.writer_writes.iter().map(PathBuf::as_path)
    }

    /// Read `[domains.run]` from the state root's `config.toml`.
    pub fn read(state: &Path) -> std::io::Result<Self> {
        let config: Self = provenant_config::read_domain(state, "run")?;
        if let Some(error) = config.validate().into_iter().next() {
            return Err(std::io::Error::other(error));
        }
        Ok(config)
    }

    /// The review-exempt paths that apply to `project`: its own list when it
    /// has one, otherwise the host-wide list.
    pub fn review_exempt_for(&self, project: &str) -> Vec<String> {
        self.projects
            .get(project)
            .and_then(|settings| settings.review_exempt.clone())
            .unwrap_or_else(|| self.review_exempt.clone())
    }

    /// The compiled matcher for `project`.
    pub fn review_exempt(&self, project: &str) -> ReviewExempt {
        ReviewExempt::new(&self.review_exempt_for(project))
    }

    /// The matcher a writer settlement uses. A config that cannot be read
    /// exempts nothing, which only adds review.
    pub fn review_exempt_at(state: &Path, project: &str) -> ReviewExempt {
        Self::read(state).map_or_else(
            |_| ReviewExempt::none(),
            |config| config.review_exempt(project),
        )
    }
}
fn valid_writer_write(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            value.len() <= 4096
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        })
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        && PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        }
        .validate()
        .is_ok()
        && !is_credential_root(path)
}
fn valid_patterns(field: &str, patterns: &[String], errors: &mut Vec<SectionError>) {
    if patterns.len() > MAX_PATTERNS {
        errors.push(SectionError {
            field: field.into(),
            code: "range",
        });
    }
    for (index, pattern) in patterns.iter().enumerate() {
        if !valid_pattern(pattern) {
            errors.push(SectionError {
                field: format!("{field}.{index}"),
                code: "path",
            });
        }
    }
}
impl DomainConfig for RunConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        valid_patterns("review_exempt", &self.review_exempt, &mut errors);
        for (project, settings) in &self.projects {
            if !project_key(project) {
                errors.push(SectionError {
                    field: format!("projects.{project}"),
                    code: "key",
                });
            }
            if let Some(patterns) = &settings.review_exempt {
                valid_patterns(
                    &format!("projects.{project}.review_exempt"),
                    patterns,
                    &mut errors,
                );
            }
        }
        if self.writer_writes.len() > MAX_WRITER_WRITES {
            errors.push(SectionError {
                field: "writer_writes".into(),
                code: "range",
            });
        }
        for (index, path) in self.writer_writes.iter().enumerate() {
            if !valid_writer_write(path) {
                errors.push(SectionError {
                    field: format!("writer_writes.{index}"),
                    code: "path",
                });
            }
        }
        errors
    }
}
/// A project ID: letters, digits and `._-`, at most 64 bytes.
fn project_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
pub const SECTION: DomainSection = DomainSection::new::<RunConfig>("run", |_| DEFAULTS);

#[cfg(test)]
mod tests {
    use super::*;
    fn fields(paths: &[&str]) -> Vec<String> {
        RunConfig {
            writer_writes: paths.iter().map(PathBuf::from).collect(),
            ..RunConfig::default()
        }
        .validate()
        .into_iter()
        .map(|error| error.field)
        .collect()
    }
    #[test]
    fn accepts_absolute_directories() {
        assert!(
            fields(&[
                "/Users/a/p/.agent-run/locks",
                "/Volumes/x/.build/cargo-wave"
            ])
            .is_empty()
        );
        assert!(fields(&[]).is_empty());
    }
    #[test]
    fn refuses_relative_unsafe_and_credential_paths() {
        assert_eq!(fields(&["locks"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/b c"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/$HOME"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/a/../b"]), ["writer_writes.0"]);
        assert_eq!(fields(&["/Users/a"]), ["writer_writes.0"]);
        assert_eq!(
            fields(&["/Users/a/.ssh/x", "/Users/a/p"]),
            ["writer_writes.0"]
        );
        assert_eq!(fields(&["/Users/a/.aws"]), ["writer_writes.0"]);
        assert_eq!(
            fields(&["/Users/a/Archives/git-preservation"]),
            ["writer_writes.0"]
        );
    }
    #[test]
    fn defaults_match_the_section_asset_and_the_compiled_list() {
        let table: RunConfig = toml_edit::de::from_str(DEFAULTS).expect("defaults parse");
        assert_eq!(table, RunConfig::default());
        assert_eq!(
            RunConfig::default().review_exempt,
            DEFAULT_REVIEW_EXEMPT.map(String::from)
        );
        assert!(RunConfig::default().validate().is_empty());
    }

    #[test]
    fn a_project_list_replaces_the_host_list_and_an_empty_one_turns_it_off() {
        let config: RunConfig = toml_edit::de::from_str(
            r#"
review_exempt = ["Cargo.lock"]
[projects.p1]
review_exempt = ["docs/"]
[projects.p2]
review_exempt = []
[projects.p3]
"#,
        )
        .expect("parse");
        assert!(config.validate().is_empty());
        assert_eq!(config.review_exempt_for("p1"), ["docs/"]);
        assert!(config.review_exempt_for("p2").is_empty());
        assert_eq!(config.review_exempt_for("p3"), ["Cargo.lock"]);
        assert_eq!(config.review_exempt_for("other"), ["Cargo.lock"]);
        assert!(config.review_exempt("p1").covers(b"docs/x.md"));
        assert!(!config.review_exempt("p2").covers(b"Cargo.lock"));
    }

    #[test]
    fn bad_patterns_and_project_keys_are_refused_by_field() {
        let config = RunConfig {
            review_exempt: vec!["ok.lock".into(), "../x".into(), "*.md".into()],
            projects: [
                (
                    "bad key".to_owned(),
                    ProjectRunConfig {
                        review_exempt: Some(vec!["/abs".into()]),
                    },
                ),
                (
                    "good".to_owned(),
                    ProjectRunConfig {
                        review_exempt: Some(vec!["x/".into()]),
                    },
                ),
            ]
            .into(),
            ..RunConfig::default()
        };
        let fields: Vec<_> = config.validate().into_iter().map(|e| e.field).collect();
        assert_eq!(
            fields,
            [
                "review_exempt.1",
                "review_exempt.2",
                "projects.bad key",
                "projects.bad key.review_exempt.0"
            ]
        );
    }

    #[test]
    fn read_uses_the_state_config_and_fails_closed_to_no_exemption() -> std::io::Result<()> {
        let state = tempfile::tempdir()?;
        assert_eq!(RunConfig::read(state.path())?, RunConfig::default());
        let write = |text: &str| std::fs::write(state.path().join("config.toml"), text);
        write("schema_version = 1\n[domains.run]\nreview_exempt = ['a/']\n")?;
        assert_eq!(RunConfig::read(state.path())?.review_exempt, ["a/"]);
        assert!(RunConfig::review_exempt_at(state.path(), "p").covers(b"a/b"));
        // Other domains are not this reader's business.
        write("schema_version = 1\n[domains.guard]\ndeny = []\n")?;
        assert_eq!(RunConfig::read(state.path())?, RunConfig::default());
        write("schema_version = 1\n[domains.run]\nreview_exempt = ['../x']\n")?;
        assert!(RunConfig::read(state.path()).is_err());
        assert!(RunConfig::review_exempt_at(state.path(), "p").is_empty());
        write("not toml [")?;
        assert!(RunConfig::review_exempt_at(state.path(), "p").is_empty());
        Ok(())
    }
}
