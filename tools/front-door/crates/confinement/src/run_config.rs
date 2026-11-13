//! `[domains.run]`: host-owned settings for worktree-write lanes: extra write
//! paths, and the repository paths a writer may change without forcing review.
use crate::permissions::{
    HostPolicySettings, LanePolicy, MandatoryRoots, PathGrant, PathScope, is_credential_root,
};
use crate::review_exempt::{MAX_PATTERNS, ReviewExempt, valid_pattern};
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use provenant_records::{BudgetAction, BudgetGrace, Limit, LimitSource, LimitStrength};
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
continuation_copy_limit_bytes = 4294967296
continuation_copy_entries = 100000
continuation_copy_depth = 32
source_view_seconds = 300
source_view_entries = 100000
source_view_bytes = 536870912
sandboxed_routes = []
sandbox_cache_reads = []

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
    /// Optional host denials. Invalid entries warn during launch resolution;
    /// they never invalidate the run section or change mandatory boundaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_policy: Option<HostPolicySettings>,
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
    /// Warning threshold for rendered Seatbelt profile size, in bytes.
    #[serde(default = "default_profile_limit_bytes")]
    pub profile_limit_bytes: usize,
    /// Warning threshold for profile family entries and plan path grants.
    #[serde(default = "default_grant_limit")]
    pub grant_limit: usize,
    /// Tokens a writer attempt may use before it is ended. 0 turns the budget
    /// off. `None` means the host did not set it, so the default applies and
    /// reports its source as `default`, even if the host later sets the same
    /// number.
    #[serde(default)]
    pub writer_token_budget: Option<u64>,
    /// Per-routing-tier writer token budget overrides.
    #[serde(default)]
    pub writer_budget_tiers: BTreeMap<String, WriterBudgetTier>,
    /// Default response when a writer passes its token budget.
    #[serde(default)]
    pub writer_budget_action: Option<BudgetAction>,
    /// Fraction above the token budget allowed before the host acts.
    #[serde(default)]
    pub writer_budget_grace: Option<BudgetGrace>,
    /// Warning threshold for encoded bytes in the applied write-grant ledger.
    #[serde(default = "default_write_grant_limit_bytes")]
    pub write_grant_limit_bytes: u64,
    /// Prompt bytes passed directly to argv before the file-backed fallback.
    #[serde(default = "default_argv_prompt_limit_bytes")]
    pub argv_prompt_limit_bytes: usize,
    /// Maximum bytes copied from retained provider state into a continuation.
    #[serde(default = "default_continuation_copy_limit_bytes")]
    pub continuation_copy_limit_bytes: u64,
    /// Maximum entries searched or copied from retained provider state.
    #[serde(default = "default_continuation_copy_entries")]
    pub continuation_copy_entries: u32,
    /// Maximum directory depth searched or copied from retained provider state.
    #[serde(default = "default_continuation_copy_depth")]
    pub continuation_copy_depth: u32,
    /// Total seconds for building one pinned source view.
    #[serde(default = "default_source_view_seconds")]
    pub source_view_seconds: u64,
    /// Maximum files in one pinned source view.
    #[serde(default = "default_source_view_entries")]
    pub source_view_entries: u32,
    /// Maximum total file bytes in one pinned source view.
    #[serde(default = "default_source_view_bytes")]
    pub source_view_bytes: u64,
    /// Route prefixes (`adapter/model`) whose lanes run sandboxed under default
    /// configuration. Empty by default; users can add routes (e.g. OpenCode Zen tier
    /// `opencode/opencode/`). An explicit `lanes.sandbox = "off"` / `--no-sandbox`
    /// overrides this list with a warning.
    #[serde(default = "default_sandboxed_routes")]
    pub sandboxed_routes: Vec<String>,
    /// Host cache directories a sandboxed lane may read but never write, such
    /// as a compiler cache. An unsandboxed lane has ordinary access to them.
    #[serde(default)]
    pub sandbox_cache_reads: Vec<PathBuf>,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRunConfig>,
}
/// Set in every lane's environment to `on` or `off`, so a lane's own tools
/// can choose, for example, a private cache when they cannot write a shared one.
pub const SANDBOX_ENV: &str = "PROVENANT_SANDBOX";
/// Default sandboxed route prefixes. Empty by default.
pub const DEFAULT_SANDBOXED_ROUTES: [&str; 0] = [];
const MAX_SANDBOXED_ROUTES: usize = 64;
fn default_sandboxed_routes() -> Vec<String> {
    Vec::new()
}
/// Why a lane runs with or without the sandbox, strongest reason first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSource {
    /// The lane was started, or added or replaced in a graph, by a lane
    /// sandboxed by its route, its own parent or the knob.
    Parent,
    /// The route matches `domains.run.sandboxed_routes`.
    Route,
    /// The lane's network is narrower than open; only the sandbox holds it.
    Network,
    /// The platform has no unsandboxed launch: Linux keeps refusing lanes.
    Platform,
    /// The `lanes.sandbox` knob, from the run, graph node or host `[knobs]`.
    Knob,
    /// Default resolution: lanes run unsandboxed like the CLI started by hand.
    Default,
}
/// What decides one attempt's sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SandboxInputs<'a> {
    /// The route id, `adapter/model` (`command/NAME` for a configured command).
    pub route: &'a str,
    /// The sealed `lanes.sandbox` value, if any.
    pub knob: Option<&'a str>,
    /// The host found the submitting lane sandboxed (or could not tell).
    pub parent_sandboxed: bool,
    /// The lane's effective network is `provider` or `local`.
    pub network_narrowed: bool,
    /// The lane cannot write its checkout.
    pub read_only: bool,
    /// The current process is already inside an operating-system sandbox (e.g. Seatbelt).
    pub os_sandboxed: bool,
    /// Whether the host platform supports sandbox confinement (true on macOS).
    pub platform_supported: bool,
}
/// The resolved sandbox mode for one attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxMode {
    /// Whether the user explicitly requested a sandbox (e.g. `--sandbox` or `knob="on"`).
    pub requested: bool,
    /// Whether the lane effectively runs under a sandbox.
    pub effective: bool,
    /// Why the lane runs with or without the sandbox.
    pub source: SandboxSource,
    /// Warning emitted when an explicit request overrides a restriction or cannot be honored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
impl SandboxMode {
    /// The `PROVENANT_SANDBOX` value.
    pub fn env_value(&self) -> &'static str {
        if self.effective { "on" } else { "off" }
    }
}

/// Whether the calling process is already running inside an operating-system sandbox (Seatbelt).
pub fn current_process_is_sandboxed() -> bool {
    provenant_os::sys::profile::current_process_is_sandboxed()
}
/// Default writer token budget.
pub const DEFAULT_WRITER_TOKEN_BUDGET: u64 = 1_000_000;
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
fn default_continuation_copy_limit_bytes() -> u64 {
    4 << 30
}
fn default_continuation_copy_entries() -> u32 {
    100_000
}
fn default_continuation_copy_depth() -> u32 {
    32
}
fn default_source_view_seconds() -> u64 {
    300
}
fn default_source_view_entries() -> u32 {
    100_000
}
fn default_source_view_bytes() -> u64 {
    512 << 20
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
    /// Replaces the host writer budget action for this project.
    #[serde(default)]
    pub writer_budget_action: Option<BudgetAction>,
    /// Replaces the host writer budget grace for this project.
    #[serde(default)]
    pub writer_budget_grace: Option<BudgetGrace>,
    /// Replaces the host `sandbox_cache_reads` for this project.
    #[serde(default)]
    pub sandbox_cache_reads: Option<Vec<PathBuf>>,
}
/// Host defaults for a writer's token budget at one routing tier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WriterBudgetTier {
    pub token_budget: Option<u64>,
    pub on_budget: Option<BudgetAction>,
    pub budget_grace: Option<BudgetGrace>,
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
            host_policy: None,
            writer_writes: Vec::new(),
            review_exempt: default_review_exempt(),
            push_conflict_markers: true,
            admission_seconds: None,
            profile_limit_bytes: default_profile_limit_bytes(),
            grant_limit: default_grant_limit(),
            writer_token_budget: None,
            writer_budget_tiers: BTreeMap::new(),
            writer_budget_action: None,
            writer_budget_grace: None,
            write_grant_limit_bytes: default_write_grant_limit_bytes(),
            argv_prompt_limit_bytes: default_argv_prompt_limit_bytes(),
            continuation_copy_limit_bytes: default_continuation_copy_limit_bytes(),
            continuation_copy_entries: default_continuation_copy_entries(),
            continuation_copy_depth: default_continuation_copy_depth(),
            source_view_seconds: default_source_view_seconds(),
            source_view_entries: default_source_view_entries(),
            source_view_bytes: default_source_view_bytes(),
            sandboxed_routes: default_sandboxed_routes(),
            sandbox_cache_reads: Vec::new(),
            projects: BTreeMap::new(),
        }
    }
}
impl RunConfig {
    /// Resolve the host policy once per launch, then report its source and
    /// warnings with `LanePolicy::report_launch`. The renderer consumes the
    /// resolved contract; this does not change the current renderer.
    pub fn policy_for_launch(&self, state: &Path, mandatory: MandatoryRoots) -> LanePolicy {
        HostPolicySettings::resolve(self.host_policy.as_ref(), state, mandatory)
    }

    /// The project list replaces the host list when present, including when empty.
    pub fn writer_write_roots_for(&self, project: &str) -> impl Iterator<Item = &Path> {
        self.projects
            .get(project)
            .and_then(|s| s.writer_writes.as_ref())
            .unwrap_or(&self.writer_writes)
            .iter()
            .map(PathBuf::as_path)
    }

    /// The cache directories a sandboxed lane in `project` may read; the
    /// project list replaces the host list when present.
    pub fn sandbox_cache_reads_for(&self, project: &str) -> impl Iterator<Item = &Path> {
        self.projects
            .get(project)
            .and_then(|s| s.sandbox_cache_reads.as_ref())
            .unwrap_or(&self.sandbox_cache_reads)
            .iter()
            .map(PathBuf::as_path)
    }

    /// Resolve one attempt's sandbox mode.
    ///
    /// By default (`lanes.sandbox` unset or `"auto"`), every lane kind (including read-only)
    /// runs unsandboxed.
    ///
    /// An explicit `--no-sandbox` / knob `"off"` takes precedence over the route floor
    /// (`sandboxed_routes`), narrowed-network floor, and parent floor where Provenant decides it,
    /// with a one-line warning naming the dropped restriction(s).
    ///
    /// An OS-inherited sandbox (the process is already inside Seatbelt) cannot be removed by
    /// a child process and is always reported honestly as effective (`effective: true`, `source: Parent`).
    ///
    /// Off macOS (or when platform confinement is unsupported), every lane resolves as sandboxed
    /// (`source: Platform`).
    pub fn sandbox_for(&self, inputs: SandboxInputs<'_>) -> SandboxMode {
        let listed = self
            .sandboxed_routes
            .iter()
            .any(|prefix| !prefix.is_empty() && inputs.route.starts_with(prefix.as_str()));

        let requested = match inputs.knob {
            Some("off") => false,
            Some("on") => true,
            None | Some("auto") => false,
            Some(_) => true,
        };

        if !inputs.platform_supported {
            let warning = if inputs.knob == Some("off") {
                Some(
                    "platform does not support unsandboxed launch; lane remains sandboxed"
                        .to_string(),
                )
            } else {
                None
            };
            return SandboxMode {
                requested,
                effective: true,
                source: SandboxSource::Platform,
                warning,
            };
        }

        if inputs.os_sandboxed {
            let warning = if inputs.knob == Some("off") {
                Some("process is already inside an operating-system sandbox; inherited sandbox remains effective".to_string())
            } else {
                None
            };
            return SandboxMode {
                requested,
                effective: true,
                source: SandboxSource::Parent,
                warning,
            };
        }

        if inputs.knob == Some("off") {
            let mut dropped = Vec::new();
            if listed {
                dropped.push(format!("route '{}'", inputs.route));
            }
            if inputs.network_narrowed {
                dropped.push("network restriction".to_string());
            }
            if inputs.parent_sandboxed {
                dropped.push("parent sandbox floor".to_string());
            }
            let warning = if dropped.is_empty() {
                None
            } else {
                Some(format!(
                    "explicit --no-sandbox drops restriction: {}",
                    dropped.join(", ")
                ))
            };
            return SandboxMode {
                requested: false,
                effective: false,
                source: SandboxSource::Knob,
                warning,
            };
        }

        if matches!(inputs.knob, Some(k) if k != "auto") {
            return SandboxMode {
                requested: true,
                effective: true,
                source: SandboxSource::Knob,
                warning: None,
            };
        }

        if inputs.parent_sandboxed {
            SandboxMode {
                requested: false,
                effective: true,
                source: SandboxSource::Parent,
                warning: None,
            }
        } else if listed {
            SandboxMode {
                requested: false,
                effective: true,
                source: SandboxSource::Route,
                warning: None,
            }
        } else if inputs.network_narrowed {
            SandboxMode {
                requested: false,
                effective: true,
                source: SandboxSource::Network,
                warning: None,
            }
        } else {
            SandboxMode {
                requested: false,
                effective: false,
                source: SandboxSource::Default,
                warning: None,
            }
        }
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

    /// The writer token budget for `project`, 0 meaning none: the project
    /// setting, then the host setting, then the default.
    pub fn writer_token_budget_for(&self, project: &str) -> Limit {
        let layers = [
            Some((LimitSource::Default, DEFAULT_WRITER_TOKEN_BUDGET)),
            self.writer_token_budget
                .map(|tokens| (LimitSource::Host, tokens)),
            self.projects
                .get(project)
                .and_then(|settings| settings.writer_token_budget)
                .map(|tokens| (LimitSource::Project, tokens)),
        ];
        Limit::resolve(layers.into_iter().flatten(), LimitStrength::Default)
            .unwrap_or_else(|| Limit::new(0, LimitSource::Default, LimitStrength::Default))
    }

    /// The action and its source for `project`, with project taking precedence
    /// over host and the compiled `wrap_up` default.
    pub fn writer_budget_action_for(&self, project: &str) -> (BudgetAction, LimitSource) {
        if let Some(action) = self
            .projects
            .get(project)
            .and_then(|settings| settings.writer_budget_action)
        {
            (action, LimitSource::Project)
        } else if let Some(action) = self.writer_budget_action {
            (action, LimitSource::Host)
        } else {
            (BudgetAction::default(), LimitSource::Default)
        }
    }

    /// The action grace and its source for `project`, with project taking
    /// precedence over host and the compiled 0.10 default.
    pub fn writer_budget_grace_for(&self, project: &str) -> (BudgetGrace, LimitSource) {
        if let Some(grace) = self
            .projects
            .get(project)
            .and_then(|settings| settings.writer_budget_grace)
        {
            (grace, LimitSource::Project)
        } else if let Some(grace) = self.writer_budget_grace {
            (grace, LimitSource::Host)
        } else {
            (BudgetGrace::default(), LimitSource::Default)
        }
    }

    /// The explicitly configured host override for `tier`, if one exists.
    pub fn writer_budget_tier_for(&self, tier: &str) -> Option<&WriterBudgetTier> {
        self.writer_budget_tiers.get(tier)
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
        for (field, value) in [
            (
                "continuation_copy_limit_bytes",
                self.continuation_copy_limit_bytes,
            ),
            (
                "continuation_copy_entries",
                u64::from(self.continuation_copy_entries),
            ),
            (
                "continuation_copy_depth",
                u64::from(self.continuation_copy_depth),
            ),
            ("source_view_seconds", self.source_view_seconds),
            ("source_view_entries", u64::from(self.source_view_entries)),
            ("source_view_bytes", self.source_view_bytes),
        ] {
            if value == 0 {
                errors.push(SectionError {
                    detail: None,
                    field: field.into(),
                    code: "range",
                });
            }
        }
        valid_patterns("review_exempt", &self.review_exempt, &mut errors);
        valid_writer_writes("writer_writes", &self.writer_writes, &mut errors);
        valid_writer_writes(
            "sandbox_cache_reads",
            &self.sandbox_cache_reads,
            &mut errors,
        );
        if self.sandboxed_routes.len() > MAX_SANDBOXED_ROUTES {
            errors.push(SectionError {
                detail: None,
                field: "sandboxed_routes".into(),
                code: "range",
            });
        }
        for (index, route) in self.sandboxed_routes.iter().enumerate() {
            if route.is_empty()
                || route.len() > 256
                || !route.bytes().all(|byte| byte.is_ascii_graphic())
            {
                errors.push(SectionError {
                    detail: None,
                    field: format!("sandboxed_routes.{index}"),
                    code: "value",
                });
            }
        }
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
            if let Some(paths) = &settings.sandbox_cache_reads {
                valid_writer_writes(
                    &format!("projects.{project}.sandbox_cache_reads"),
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
    FieldMetadata::new("host_policy.deny_read", "array"),
    FieldMetadata::new("host_policy.deny_write", "array"),
    FieldMetadata::enumeration("host_policy.network", &["open", "local", "provider"]),
    FieldMetadata::new("profile_limit_bytes", "int"),
    FieldMetadata::new("grant_limit", "int"),
    FieldMetadata::new("writer_token_budget", "int"),
    FieldMetadata::new("writer_budget_tiers.{tier}.token_budget", "int"),
    FieldMetadata::enumeration(
        "writer_budget_tiers.{tier}.on_budget",
        &["wrap_up", "warn", "stop"],
    ),
    FieldMetadata::new("writer_budget_tiers.{tier}.budget_grace", "fraction"),
    FieldMetadata::enumeration("writer_budget_action", &["wrap_up", "warn", "stop"]),
    FieldMetadata::new("writer_budget_grace", "fraction"),
    FieldMetadata::int(
        "write_grant_limit_bytes",
        1,
        provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING as i64,
    ),
    FieldMetadata::new("argv_prompt_limit_bytes", "int"),
    FieldMetadata::new("continuation_copy_limit_bytes", "int"),
    FieldMetadata::new("continuation_copy_entries", "int"),
    FieldMetadata::new("continuation_copy_depth", "int"),
    FieldMetadata::new("source_view_seconds", "int"),
    FieldMetadata::new("source_view_entries", "int"),
    FieldMetadata::new("source_view_bytes", "int"),
    FieldMetadata::new("push_conflict_markers", "bool"),
    FieldMetadata::new("admission_seconds", "integer"),
    FieldMetadata::new("writer_writes", "array"),
    FieldMetadata::new("review_exempt", "array"),
    FieldMetadata::new("projects.{project}.writer_writes", "array"),
    FieldMetadata::new("projects.{project}.review_exempt", "array"),
    FieldMetadata::new("projects.{project}.admission_seconds", "integer"),
    FieldMetadata::new("sandboxed_routes", "array"),
    FieldMetadata::new("sandbox_cache_reads", "array"),
    FieldMetadata::new("projects.{project}.sandbox_cache_reads", "array"),
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
        assert_eq!(defaults.continuation_copy_limit_bytes, 4 << 30);
        assert_eq!(defaults.continuation_copy_entries, 100_000);
        assert_eq!(defaults.continuation_copy_depth, 32);
        for key in [
            "continuation_copy_limit_bytes",
            "continuation_copy_entries",
            "continuation_copy_depth",
        ] {
            let invalid: RunConfig = toml_edit::de::from_str(&format!("{key} = 0"))?;
            assert!(invalid.validate().iter().any(|error| error.field == key));
        }
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
    fn lane_input(route: &str) -> SandboxInputs<'_> {
        SandboxInputs {
            route,
            knob: None,
            parent_sandboxed: false,
            network_narrowed: false,
            read_only: false,
            os_sandboxed: false,
            platform_supported: true,
        }
    }
    fn writer(route: &str) -> SandboxInputs<'_> {
        lane_input(route)
    }

    #[test]
    fn precedence_table_invariants() {
        let config_with_routes: RunConfig =
            toml_edit::de::from_str("sandboxed_routes = [\"opencode/opencode/\"]").unwrap();
        let default_config = RunConfig::default();

        // 1. Invariant: Default is off for every lane kind when no floor applies.
        for read_only in [false, true] {
            let input = SandboxInputs {
                read_only,
                ..lane_input("codex/gpt-5.5")
            };
            let res = default_config.sandbox_for(input);
            assert_eq!(
                (res.requested, res.effective, res.source),
                (false, false, SandboxSource::Default),
                "default off for read_only={read_only}"
            );
            assert_eq!(res.env_value(), "off");
        }

        // 2. Invariant: Explicit knob beats default and floors (property: explicit beats default).
        for config in [&default_config, &config_with_routes] {
            for route in ["codex/gpt-5.5", "opencode/opencode/glm-5.3-flash-free"] {
                for network_narrowed in [false, true] {
                    for parent_sandboxed in [false, true] {
                        for read_only in [false, true] {
                            let base = SandboxInputs {
                                route,
                                knob: None,
                                parent_sandboxed,
                                network_narrowed,
                                read_only,
                                os_sandboxed: false,
                                platform_supported: true,
                            };

                            // Explicit off beats all Provenant floors:
                            let off_input = SandboxInputs {
                                knob: Some("off"),
                                ..base
                            };
                            let off_mode = config.sandbox_for(off_input);
                            assert!(
                                !off_mode.effective,
                                "explicit off must always be unsandboxed on macos"
                            );
                            assert_eq!(off_mode.source, SandboxSource::Knob);
                            assert!(!off_mode.requested);
                            assert_eq!(off_mode.env_value(), "off");

                            let listed =
                                config.sandboxed_routes.iter().any(|p| route.starts_with(p));
                            let had_restriction = listed || network_narrowed || parent_sandboxed;
                            assert_eq!(
                                off_mode.warning.is_some(),
                                had_restriction,
                                "warning expected when restrictions are dropped"
                            );

                            // Explicit on always sandboxes:
                            let on_input = SandboxInputs {
                                knob: Some("on"),
                                ..base
                            };
                            let on_mode = config.sandbox_for(on_input);
                            assert!(on_mode.effective, "explicit on must always sandbox");
                            assert_eq!(on_mode.source, SandboxSource::Knob);
                            assert!(on_mode.requested);
                            assert_eq!(on_mode.env_value(), "on");

                            // Default behavior (knob is None or auto):
                            for knob in [None, Some("auto")] {
                                let def_input = SandboxInputs { knob, ..base };
                                let def_mode = config.sandbox_for(def_input);
                                assert!(!def_mode.requested);
                                if parent_sandboxed {
                                    assert!(def_mode.effective);
                                    assert_eq!(def_mode.source, SandboxSource::Parent);
                                } else if listed {
                                    assert!(def_mode.effective);
                                    assert_eq!(def_mode.source, SandboxSource::Route);
                                } else if network_narrowed {
                                    assert!(def_mode.effective);
                                    assert_eq!(def_mode.source, SandboxSource::Network);
                                } else {
                                    assert!(!def_mode.effective);
                                    assert_eq!(def_mode.source, SandboxSource::Default);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Invariant: Inherited OS sandbox is always reported effective (property: inherited-OS is always reported effective).
        for knob in [None, Some("auto"), Some("off"), Some("on")] {
            for parent_sandboxed in [false, true] {
                let input = SandboxInputs {
                    knob,
                    parent_sandboxed,
                    os_sandboxed: true,
                    ..lane_input("codex/gpt-5.5")
                };
                let mode = default_config.sandbox_for(input);
                assert!(
                    mode.effective,
                    "inherited OS sandbox must always be reported effective"
                );
                assert_eq!(mode.source, SandboxSource::Parent);
                if knob == Some("off") {
                    assert!(
                        mode.warning.is_some(),
                        "honest warning when --no-sandbox cannot drop OS sandbox"
                    );
                }
            }
        }

        // 4. Invariant: Unsupported platform always resolves sandboxed.
        for knob in [None, Some("auto"), Some("off"), Some("on")] {
            let input = SandboxInputs {
                knob,
                platform_supported: false,
                ..lane_input("codex/gpt-5.5")
            };
            let mode = default_config.sandbox_for(input);
            assert!(
                mode.effective,
                "unsupported platform must always resolve sandboxed"
            );
            assert_eq!(mode.source, SandboxSource::Platform);
        }
    }
    #[test]
    fn a_recorded_mode_names_effectiveness_once() -> Result<(), Box<dyn std::error::Error>> {
        let mode = RunConfig::default().sandbox_for(writer("codex/gpt-6-luna"));
        let value = serde_json::to_value(&mode)?;
        let fields: Vec<_> = value
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
            .collect();
        assert!(fields.contains(&&"effective".to_string()), "{value}");
        assert!(!fields.contains(&&"sandboxed".to_string()), "{value}");
        Ok(())
    }
    #[test]
    fn sandboxed_routes_and_cache_reads_are_configurable_and_validated()
    -> Result<(), Box<dyn std::error::Error>> {
        let open: RunConfig = toml_edit::de::from_str("sandboxed_routes = []")?;
        assert!(open.validate().is_empty());
        assert!(
            !open
                .sandbox_for(writer("opencode/opencode/glm-5.3-flash-free"))
                .effective
        );
        let wider: RunConfig =
            toml_edit::de::from_str("sandboxed_routes = [\"opencode/\", \"cursor/\"]")?;
        assert!(
            wider
                .sandbox_for(writer("opencode/opencode-go/x"))
                .effective
        );
        assert!(wider.sandbox_for(writer("cursor/grok-4.7")).effective);
        let bad: RunConfig = toml_edit::de::from_str("sandboxed_routes = [\"\", \"has space/\"]")?;
        let fields: Vec<_> = bad.validate().into_iter().map(|e| e.field).collect();
        assert_eq!(fields, ["sandboxed_routes.0", "sandboxed_routes.1"]);
        let caches: RunConfig = toml_edit::de::from_str(
            "sandbox_cache_reads = [\"/var/cache/sccache\"]\n[projects.p]\nsandbox_cache_reads = []\n",
        )?;
        assert!(caches.validate().is_empty());
        assert_eq!(
            caches.sandbox_cache_reads_for("other").collect::<Vec<_>>(),
            [Path::new("/var/cache/sccache")]
        );
        assert_eq!(caches.sandbox_cache_reads_for("p").count(), 0);
        let relative: RunConfig = toml_edit::de::from_str("sandbox_cache_reads = [\"cache\"]")?;
        assert!(
            relative
                .validate()
                .iter()
                .any(|error| error.field.starts_with("sandbox_cache_reads"))
        );
        Ok(())
    }
    #[test]
    fn a_host_setting_equal_to_the_default_still_reports_host() {
        let unset = RunConfig::default().writer_token_budget_for("p");
        assert_eq!(
            (unset.value, unset.source),
            (DEFAULT_WRITER_TOKEN_BUDGET, LimitSource::Default)
        );
        let host: RunConfig = toml_edit::de::from_str(&format!(
            "writer_token_budget = {DEFAULT_WRITER_TOKEN_BUDGET}\n"
        ))
        .unwrap();
        let budget = host.writer_token_budget_for("p");
        assert_eq!(
            (budget.value, budget.source),
            (DEFAULT_WRITER_TOKEN_BUDGET, LimitSource::Host)
        );
        let off: RunConfig = toml_edit::de::from_str("writer_token_budget = 0\n").unwrap();
        assert_eq!(off.writer_token_budget_for("p").value, 0);
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
        write("schema_version = 1\n[domains.other]\nfuture = true\n")?;
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::permissions::{NetworkAccess, OptionalPolicy, PolicySource};

    fn mandatory() -> MandatoryRoots {
        let mut roots = MandatoryRoots::new(
            Path::new("/fixture/home"),
            Path::new("/fixture/state"),
            Path::new("/fixture/project/source"),
        )
        .expect("mandatory roots");
        for git in ["/fixture/project/common.git", "/fixture/project/linked.git"] {
            roots
                .add_git(PathGrant {
                    path: git.into(),
                    scope: PathScope::Subtree,
                })
                .expect("Git root");
        }
        roots
            .add_credentials(PathGrant {
                path: "/fixture/identities/agy-second".into(),
                scope: PathScope::Subtree,
            })
            .expect("identity");
        roots
    }

    // Medium: real configuration file; no process or native sandbox.
    #[test]
    fn host_policy_defaults_open_and_reports_explicit_source_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = tempfile::tempdir()?;
        let ambient = RunConfig::read(state.path())?.policy_for_launch(state.path(), mandatory());
        assert_eq!(ambient.optional(), &OptionalPolicy::default());
        assert_eq!(ambient.source(), &PolicySource::Ambient);
        assert!(ambient.warnings().is_empty());
        let mut report = Vec::new();
        ambient.report_launch(&mut report)?;
        assert_eq!(String::from_utf8(report)?.lines().count(), 1);
        for settings in [
            "",
            "network = 'open'\ndeny_read = []\ndeny_write = []",
            "network = 'local'\ndeny_read = ['/fixture/private']\ndeny_write = ['/fixture/archive']",
        ] {
            std::fs::write(
                state.path().join("config.toml"),
                format!("schema_version=1\n[domains.run.host_policy]\n{settings}\n"),
            )?;
            let configured =
                RunConfig::read(state.path())?.policy_for_launch(state.path(), mandatory());
            assert_eq!(
                configured.source(),
                &PolicySource::Host(state.path().join("config.toml"))
            );
            assert!(configured.warnings().is_empty());
            let mut report = Vec::new();
            configured.report_launch(&mut report)?;
            let report = String::from_utf8(report)?;
            assert_eq!(report.lines().count(), 1);
            assert!(report.contains(&state.path().join("config.toml").display().to_string()));
            if settings.contains("local") {
                assert_eq!(configured.optional().network, NetworkAccess::Local);
                assert_eq!(
                    configured.optional().deny_read[0].path,
                    Path::new("/fixture/private")
                );
                assert_eq!(
                    configured.optional().deny_write[0].path,
                    Path::new("/fixture/archive")
                );
            } else {
                assert_eq!(configured.optional(), &OptionalPolicy::default());
            }
        }
        Ok(())
    }

    // Medium: exercise the strict domain reader and the aggregate config path.
    #[test]
    fn invalid_host_policy_warns_retains_siblings_and_cannot_remove_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Domains {
            run: RunConfig,
        }
        const SECTIONS: provenant_config::DomainSections<Domains> =
            provenant_config::DomainSections::new(&[SECTION]);
        let state = tempfile::tempdir()?;
        let mut input = provenant_core::RootInputs::new(
            state.path().join("home"),
            state.path().join("checkout"),
        );
        input.state_dir = Some(state.path().join("state"));
        let roots = input.resolve(&provenant_os::fs::SystemPathResolver)?;
        let mandatory = mandatory();
        for (settings, fields) in [
            (
                "deny_read = ['/fixture/read', 7, 'relative-secret-sentinel', '/fixture/../escape']\ndeny_write = ['/fixture/write']\nnetwork = 'invalid-secret-sentinel'\nmandatory = []\nsbpl = '(allow default)'",
                vec![
                    "unknown",
                    "deny_read.1",
                    "deny_read.2",
                    "deny_read.3",
                    "network",
                ],
            ),
            (
                "deny_read = false\ndeny_write = ['/fixture/write', false]\nnetwork = 9",
                vec!["deny_read", "deny_write.1", "network"],
            ),
        ] {
            let text = format!(
                "schema_version=1\n[domains.run]\nwriter_writes=['/fixture/locks']\n[domains.run.host_policy]\n{settings}\n"
            );
            std::fs::write(state.path().join("config.toml"), &text)?;
            let domain = RunConfig::read(state.path())?;
            let aggregate =
                provenant_config::load_with_domains(&roots, Some(text.as_bytes()), &SECTIONS)?;
            assert_eq!(domain, aggregate.domains.run);
            assert_eq!(domain.writer_writes, [PathBuf::from("/fixture/locks")]);
            assert!(domain.validate().is_empty());
            let policy = domain.policy_for_launch(state.path(), mandatory.clone());
            assert_eq!(policy.mandatory(), &mandatory);
            assert_eq!(policy.optional().network, NetworkAccess::Open);
            assert_eq!(
                policy.optional().deny_write[0].path,
                Path::new("/fixture/write")
            );
            if settings.contains("/fixture/read") {
                assert_eq!(policy.optional().deny_read.len(), 1);
                assert_eq!(
                    policy.optional().deny_read[0].path,
                    Path::new("/fixture/read")
                );
            } else {
                assert!(policy.optional().deny_read.is_empty());
            }
            assert_eq!(
                policy
                    .warnings()
                    .iter()
                    .map(|w| w.field.as_str())
                    .collect::<Vec<_>>(),
                fields
            );
            let mut report = Vec::new();
            policy.report_launch(&mut report)?;
            let report = String::from_utf8(report)?;
            assert_eq!(
                report
                    .lines()
                    .filter(|line| line.starts_with("lane policy:"))
                    .count(),
                1
            );
            assert_eq!(
                report
                    .lines()
                    .filter(|line| line.starts_with("warning:"))
                    .count(),
                fields.len()
            );
            assert!(!report.contains("secret-sentinel"));
            assert!(!report.contains("(allow default)"));
        }
        for value in ["false", "7", "'invalid-secret-sentinel'", "[]"] {
            let domain: RunConfig = toml_edit::de::from_str(&format!("host_policy = {value}"))?;
            let policy = domain.policy_for_launch(state.path(), mandatory.clone());
            assert_eq!(policy.optional(), &OptionalPolicy::default());
            assert_eq!(policy.mandatory(), &mandatory);
            assert_eq!(policy.warnings()[0].code, "type");
        }
        // Authority settings retain their existing refusal semantics.
        std::fs::write(
            state.path().join("config.toml"),
            "schema_version=1\n[domains.run]\nwriter_writes=['/fixture/home/.ssh']\n[domains.run.host_policy]\nnetwork='open'\n",
        )?;
        assert!(RunConfig::read(state.path()).is_err());
        Ok(())
    }

    // Small: lexical host-root contract, including nonexistent credential files.
    #[test]
    fn optional_open_policy_retains_every_mandatory_write_boundary() {
        let mandatory = mandatory();
        let policy =
            RunConfig::default().policy_for_launch(Path::new("/fixture/state"), mandatory.clone());
        let roots = policy.mandatory();
        assert_eq!(roots, &mandatory);
        assert!(roots.credentials().iter().any(|g| g.path
            == Path::new("/fixture/home/.cargo/credentials.toml")
            && g.scope == PathScope::Exact));
        assert!(
            roots
                .credentials()
                .iter()
                .any(|g| g.path == Path::new("/fixture/home/.claude.json")
                    && g.scope == PathScope::Exact)
        );
        assert!(
            roots
                .credentials()
                .iter()
                .any(|g| g.path == Path::new("/fixture/home/Library/Keychains"))
        );
        assert!(
            roots
                .credentials()
                .iter()
                .any(|g| g.path == Path::new("/fixture/identities/agy-second"))
        );
        for root in roots
            .credentials()
            .iter()
            .chain(roots.git())
            .chain([roots.state(), roots.source()])
        {
            for path in [
                root.path.clone(),
                root.path.join("child"),
                root.path.parent().expect("parent").to_owned(),
            ] {
                assert!(
                    roots
                        .validate_extra_write(&PathGrant {
                            path: path.clone(),
                            scope: PathScope::Subtree
                        })
                        .is_err(),
                    "{}",
                    path.display()
                );
            }
        }
        for path in [
            "/fixture/locks",
            "/fixture/project/source-sibling",
            "/fixture/state-sibling",
        ] {
            assert!(
                roots
                    .validate_extra_write(&PathGrant {
                        path: path.into(),
                        scope: PathScope::Subtree
                    })
                    .is_ok(),
                "{path}"
            );
        }
    }

    // Small: exhaust all network ceiling/request pairs.
    #[test]
    fn host_network_policy_never_widens_lane_authority() {
        use NetworkAccess::{Local, Open, Provider};
        for (host, request, expected) in [
            (Open, Open, Open),
            (Open, Local, Local),
            (Open, Provider, Provider),
            (Local, Open, Local),
            (Local, Local, Local),
            (Local, Provider, Provider),
            (Provider, Open, Provider),
            (Provider, Local, Provider),
            (Provider, Provider, Provider),
        ] {
            let config: RunConfig = toml_edit::de::from_str(&format!(
                "[host_policy]\nnetwork = {}",
                serde_json::to_string(&host).expect("network")
            ))
            .expect("host policy");
            let policy = config.policy_for_launch(Path::new("/fixture/state"), mandatory());
            assert!(policy.warnings().is_empty());
            assert_eq!(policy.optional().network_for(request), expected);
        }
    }
}
