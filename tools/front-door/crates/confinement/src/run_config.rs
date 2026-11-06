//! `[domains.run]`: host-owned settings for worktree-write lanes: extra write
//! paths, and the repository paths a writer may change without forcing review.
use crate::permissions::{PathGrant, PathScope, is_credential_root};
use crate::review_exempt::{MAX_PATTERNS, ReviewExempt, valid_pattern};
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const MAX_WRITER_WRITES: usize = 16;
/// Default maximum prompt text passed as a single argument.
pub const DEFAULT_ARGV_PROMPT_LIMIT_BYTES: usize = 65_536;
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
push_conflict_markers = true
profile_limit_bytes = 524288
grant_limit = 128
write_grant_limit_bytes = 262144
argv_prompt_limit_bytes = 65536

[projects]
"#;

/// Shared run state kept outside the worktree, such as a lock directory a
/// repository script locks with `/usr/bin/lockf`, or a shared cargo target.
/// Each directory is read-write for writer lanes only; read-only lanes ignore
/// it. Plan compilation still refuses any overlap with protected roots.
///
/// `review_exempt` lists repository-relative paths a writer may leave
/// uncommitted without the lane ending `uncommitted`: `dir/` for a directory at the root, a bare name for a
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
    /// Refuse pushes whose added lines start with a literal conflict marker.
    #[serde(default = "default_true")]
    pub push_conflict_markers: bool,
    /// Default admission deadline in seconds before launch.
    #[serde(default)]
    pub admission_seconds: Option<u64>,
    /// Maximum rendered Seatbelt profile size, in bytes.
    #[serde(default = "default_profile_limit_bytes")]
    pub profile_limit_bytes: usize,
    /// Maximum entries in each profile family and path grants in the plan;
    /// authentication and native controls retain minimum caps of 16 and 64.
    #[serde(default = "default_grant_limit")]
    pub grant_limit: usize,
    /// Tokens a writer attempt may use before it is ended. 0 turns the budget
    /// off. `None` means the host did not set it, so the default applies and
    /// reports its source as `default`, even if the host later sets the same
    /// number.
    #[serde(default)]
    pub writer_token_budget: Option<u64>,
    /// Maximum encoded bytes in the applied write-grant ledger.
    #[serde(default = "default_write_grant_limit_bytes")]
    pub write_grant_limit_bytes: u64,
    /// Prompt bytes passed directly to argv before the file-backed fallback.
    #[serde(default = "default_argv_prompt_limit_bytes")]
    pub argv_prompt_limit_bytes: usize,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRunConfig>,
}
/// Default writer token budget.
pub const DEFAULT_WRITER_TOKEN_BUDGET: u64 = 500_000;
fn default_profile_limit_bytes() -> usize {
    512 * 1024
}
fn default_grant_limit() -> usize {
    128
}
fn default_write_grant_limit_bytes() -> u64 {
    provenant_records::run::MAX_WRITE_GRANT_BYTES
}
fn default_argv_prompt_limit_bytes() -> usize {
    DEFAULT_ARGV_PROMPT_LIMIT_BYTES
}
fn default_true() -> bool {
    true
}
/// One project's replacement for the host-wide settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRunConfig {
    #[serde(default)]
    pub writer_writes: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub review_exempt: Option<Vec<String>>,
    /// Default admission deadline in seconds before launch for this project.
    #[serde(default)]
    pub admission_seconds: Option<u64>,
    /// Replaces the host writer token budget for this project.
    #[serde(default)]
    pub writer_token_budget: Option<u64>,
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
            push_conflict_markers: true,
            admission_seconds: None,
            profile_limit_bytes: default_profile_limit_bytes(),
            grant_limit: default_grant_limit(),
            writer_token_budget: None,
            write_grant_limit_bytes: default_write_grant_limit_bytes(),
            argv_prompt_limit_bytes: default_argv_prompt_limit_bytes(),
            projects: BTreeMap::new(),
        }
    }
}
impl RunConfig {
    /// The project list replaces the host list when present, including when empty.
    pub fn writer_write_roots_for(&self, project: &str) -> impl Iterator<Item = &Path> {
        self.projects
            .get(project)
            .and_then(|s| s.writer_writes.as_ref())
            .unwrap_or(&self.writer_writes)
            .iter()
            .map(PathBuf::as_path)
    }

    /// Read `[domains.run]` from the state root's `config.toml`.
    pub fn read(state: &Path) -> std::io::Result<Self> {
        Self::read_with_keys(state).map(|(config, _)| config)
    }

    /// Read the settings and their explicit top-level keys from one snapshot.
    pub fn read_with_keys(
        state: &Path,
    ) -> std::io::Result<(Self, std::collections::BTreeSet<String>)> {
        let (config, keys): (Self, _) = provenant_config::read_domain_with_keys(state, "run")?;
        if let Some(error) = config.validate().into_iter().next() {
            return Err(std::io::Error::other(error));
        }
        Ok((config, keys))
    }

    /// The admission seconds that apply to `project`: its own setting when it
    /// has one, otherwise the host-wide setting.
    pub fn admission_seconds_for(&self, project: &str) -> Option<u64> {
        self.projects
            .get(project)
            .and_then(|settings| settings.admission_seconds)
            .or(self.admission_seconds)
    }

    /// The writer token budget for `project`, with where it came from. A
    /// per-run override wins over the project, then the host setting, then
    /// the default.
    pub fn writer_token_budget_for(&self, project: &str, per_run: Option<u64>) -> TokenBudget {
        let (tokens, source) = if let Some(tokens) = per_run {
            (tokens, TokenBudgetSource::Run)
        } else if let Some(tokens) = self
            .projects
            .get(project)
            .and_then(|settings| settings.writer_token_budget)
        {
            (tokens, TokenBudgetSource::Project)
        } else if let Some(tokens) = self.writer_token_budget {
            (tokens, TokenBudgetSource::Host)
        } else {
            (DEFAULT_WRITER_TOKEN_BUDGET, TokenBudgetSource::Default)
        };
        TokenBudget { tokens, source }
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
    /// exempts nothing, which may report uncommitted changes.
    pub fn review_exempt_at(state: &Path, project: &str) -> ReviewExempt {
        Self::read(state).map_or_else(
            |_| ReviewExempt::none(),
            |config| config.review_exempt(project),
        )
    }
}
/// A resolved writer token budget. `tokens` of 0 means no budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TokenBudget {
    pub tokens: u64,
    pub source: TokenBudgetSource,
}
/// Where a writer token budget was set, for `--explain` and receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenBudgetSource {
    Run,
    Project,
    Host,
    Default,
}
impl TokenBudgetSource {
    /// The name shown in `--explain` and receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Project => "project",
            Self::Host => "host",
            Self::Default => "default",
        }
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
            detail: None,
            field: field.into(),
            code: "range",
        });
    }
    for (index, pattern) in patterns.iter().enumerate() {
        if !valid_pattern(pattern) {
            errors.push(SectionError {
                detail: None,
                field: format!("{field}.{index}"),
                code: "path",
            });
        }
    }
}
fn valid_writer_writes(field: &str, paths: &[PathBuf], errors: &mut Vec<SectionError>) {
    if paths.len() > MAX_WRITER_WRITES {
        errors.push(SectionError {
            detail: None,
            field: field.into(),
            code: "range",
        });
    }
    for (index, path) in paths.iter().enumerate() {
        if !valid_writer_write(path) {
            errors.push(SectionError {
                detail: None,
                field: format!("{field}.{index}"),
                code: "path",
            });
        }
    }
}
impl DomainConfig for RunConfig {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if let Some(0) = self.admission_seconds {
            errors.push(SectionError {
                detail: None,
                field: "admission_seconds".into(),
                code: "range",
            });
        }
        for (field, value) in [
            ("profile_limit_bytes", self.profile_limit_bytes),
            ("grant_limit", self.grant_limit),
            ("argv_prompt_limit_bytes", self.argv_prompt_limit_bytes),
        ] {
            if value == 0 {
                errors.push(SectionError {
                    detail: None,
                    field: field.into(),
                    code: "range",
                });
            }
        }
        if !(1..=provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING)
            .contains(&self.write_grant_limit_bytes)
        {
            errors.push(SectionError {
                detail: None,
                field: "write_grant_limit_bytes".into(),
                code: "range",
            });
        }
        valid_patterns("review_exempt", &self.review_exempt, &mut errors);
        valid_writer_writes("writer_writes", &self.writer_writes, &mut errors);
        for (project, settings) in &self.projects {
            if !project_key(project) {
                errors.push(SectionError {
                    detail: None,
                    field: format!("projects.{project}"),
                    code: "key",
                });
            }
            if let Some(0) = settings.admission_seconds {
                errors.push(SectionError {
                    detail: None,
                    field: format!("projects.{project}.admission_seconds"),
                    code: "range",
                });
            }
            if let Some(paths) = &settings.writer_writes {
                valid_writer_writes(
                    &format!("projects.{project}.writer_writes"),
                    paths,
                    &mut errors,
                );
            }
            if let Some(patterns) = &settings.review_exempt {
                valid_patterns(
                    &format!("projects.{project}.review_exempt"),
                    patterns,
                    &mut errors,
                );
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
pub const FIELD_METADATA: &[FieldMetadata] = &[
    FieldMetadata::new("profile_limit_bytes", "int"),
    FieldMetadata::new("grant_limit", "int"),
    FieldMetadata::new("writer_token_budget", "int"),
    FieldMetadata::int(
        "write_grant_limit_bytes",
        1,
        provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING as i64,
    ),
    FieldMetadata::new("argv_prompt_limit_bytes", "int"),
    FieldMetadata::new("push_conflict_markers", "bool"),
    FieldMetadata::new("admission_seconds", "integer"),
    FieldMetadata::new("writer_writes", "array"),
    FieldMetadata::new("review_exempt", "array"),
    FieldMetadata::new("projects.{project}.writer_writes", "array"),
    FieldMetadata::new("projects.{project}.review_exempt", "array"),
    FieldMetadata::new("projects.{project}.admission_seconds", "integer"),
];
pub const SECTION: DomainSection = DomainSection::new::<RunConfig>("run", |_| DEFAULTS)
    .with_boundary_checks()
    .with_fields(FIELD_METADATA);

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
    fn launch_resource_defaults_can_be_raised() -> Result<(), Box<dyn std::error::Error>> {
        let defaults: RunConfig = toml_edit::de::from_str("")?;
        assert_eq!(defaults.grant_limit, 128);
        assert_eq!(
            defaults.write_grant_limit_bytes,
            provenant_records::run::MAX_WRITE_GRANT_BYTES
        );
        assert_eq!(
            defaults.argv_prompt_limit_bytes,
            DEFAULT_ARGV_PROMPT_LIMIT_BYTES
        );
        assert_eq!(defaults.profile_limit_bytes, 512 * 1024);
        let raised: RunConfig = toml_edit::de::from_str(
            "grant_limit = 2048\nprofile_limit_bytes = 2097152\nargv_prompt_limit_bytes = 131072\n",
        )?;
        assert!(raised.validate().is_empty());
        assert_eq!(raised.argv_prompt_limit_bytes, 131_072);
        assert_eq!(
            (raised.grant_limit, raised.profile_limit_bytes),
            (2048, 2097152)
        );
        let state = tempfile::tempdir()?;
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[domains.run]\ngrant_limit=2048\nprofile_limit_bytes=2097152\nargv_prompt_limit_bytes=131072\n",
        )?;
        let loaded = RunConfig::read(state.path())?;
        assert_eq!(loaded.argv_prompt_limit_bytes, 131_072);
        assert_eq!(
            (loaded.grant_limit, loaded.profile_limit_bytes),
            (2048, 2097152)
        );
        let configured: RunConfig = toml_edit::de::from_str("write_grant_limit_bytes = 1048576")?;
        assert!(configured.validate().is_empty());
        assert_eq!(configured.write_grant_limit_bytes, 1048576);
        let invalid: RunConfig = toml_edit::de::from_str("write_grant_limit_bytes = 8388609")?;
        assert!(
            invalid
                .validate()
                .iter()
                .any(|error| error.field == "write_grant_limit_bytes")
        );
        let invalid_argv: RunConfig = toml_edit::de::from_str("argv_prompt_limit_bytes = 0")?;
        assert!(
            invalid_argv
                .validate()
                .iter()
                .any(|error| error.field == "argv_prompt_limit_bytes")
        );
        let invalid: RunConfig = toml_edit::de::from_str("grant_limit = 0")?;
        assert!(
            invalid
                .validate()
                .iter()
                .any(|error| error.field == "grant_limit")
        );
        Ok(())
    }
    #[test]
    fn a_host_setting_equal_to_the_default_still_reports_host() {
        let unset = RunConfig::default().writer_token_budget_for("p", None);
        assert_eq!(
            (unset.tokens, unset.source),
            (DEFAULT_WRITER_TOKEN_BUDGET, TokenBudgetSource::Default)
        );
        let host: RunConfig = toml_edit::de::from_str("writer_token_budget = 500000\n").unwrap();
        let budget = host.writer_token_budget_for("p", None);
        assert_eq!(
            (budget.tokens, budget.source),
            (500_000, TokenBudgetSource::Host)
        );
        let off: RunConfig = toml_edit::de::from_str("writer_token_budget = 0\n").unwrap();
        assert_eq!(off.writer_token_budget_for("p", None).tokens, 0);
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
    fn conflict_marker_check_defaults_on_and_can_be_turned_off() {
        assert!(RunConfig::default().push_conflict_markers);
        let config: RunConfig =
            toml_edit::de::from_str("push_conflict_markers = false\n").expect("parse");
        assert!(!config.push_conflict_markers);
        assert!(config.validate().is_empty());
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
    fn project_writer_writes_replace_host_and_validate_identically() {
        let config: RunConfig = toml_edit::de::from_str(
            r#"
writer_writes = ["/host/locks"]
[projects.a]
writer_writes = ["/a/locks"]
[projects.empty]
writer_writes = []
[projects.inherit]
"#,
        )
        .expect("parse");
        let paths = |project| {
            config
                .writer_write_roots_for(project)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        };
        assert_eq!(paths("a"), [PathBuf::from("/a/locks")]);
        assert!(paths("empty").is_empty());
        assert_eq!(paths("inherit"), [PathBuf::from("/host/locks")]);
        assert_eq!(paths("other"), [PathBuf::from("/host/locks")]);
        assert!(config.validate().is_empty());
        for path in [
            "relative",
            "/a/b c",
            "/a/$HOME",
            "/a/../b",
            "/Users/a/.ssh/x",
        ] {
            let mut bad = config.clone();
            bad.projects.get_mut("a").unwrap().writer_writes = Some(vec![path.into()]);
            assert_eq!(
                bad.validate()
                    .iter()
                    .map(|e| e.field.as_str())
                    .collect::<Vec<_>>(),
                ["projects.a.writer_writes.0"]
            );
        }
        let mut bad = config;
        bad.projects.get_mut("a").unwrap().writer_writes =
            Some(vec![PathBuf::from("/a/locks"); MAX_WRITER_WRITES + 1]);
        assert_eq!(bad.validate()[0].field, "projects.a.writer_writes");
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
                        ..Default::default()
                    },
                ),
                (
                    "good".to_owned(),
                    ProjectRunConfig {
                        review_exempt: Some(vec!["x/".into()]),
                        ..Default::default()
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
        write("schema_version = 1\n[domains.run]\nfuture=true\nreview_exempt = ['a/']\n")?;
        assert_eq!(RunConfig::read(state.path())?.review_exempt, ["a/"]);
        write("schema_version = 1\n[domains.run.projects.p]\nreview_exempt = false\n")?;
        assert!(RunConfig::read(state.path()).is_err());
        write("schema_version = 1\n[domains.run]\nreview_exempt = ['a/']\n")?;
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

    #[test]
    fn admission_seconds_config_resolution_and_validation() {
        let mut config = RunConfig {
            admission_seconds: Some(1800),
            projects: [
                (
                    "proj-override".to_owned(),
                    ProjectRunConfig {
                        admission_seconds: Some(600),
                        ..Default::default()
                    },
                ),
                ("proj-default".to_owned(), ProjectRunConfig::default()),
            ]
            .into(),
            ..RunConfig::default()
        };
        assert_eq!(config.admission_seconds_for("proj-override"), Some(600));
        assert_eq!(config.admission_seconds_for("proj-default"), Some(1800));
        assert_eq!(config.admission_seconds_for("proj-other"), Some(1800));
        assert!(config.validate().is_empty());

        // Zero seconds is refused by range
        config.admission_seconds = Some(0);
        let fields: Vec<_> = config.validate().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["admission_seconds"]);

        config.admission_seconds = Some(1800);
        config
            .projects
            .get_mut("proj-override")
            .unwrap()
            .admission_seconds = Some(0);
        let fields: Vec<_> = config.validate().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["projects.proj-override.admission_seconds"]);
    }
}
