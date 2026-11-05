//! The binary edge registers the closed aggregate of landed domain owners.
use provenant_config::{DomainConfig, DomainSection, DomainSections, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domains {
    #[serde(default)]
    pub wait: crate::wait::Settings,
    pub accounts: provenant_providers::accounts::Settings,
    pub quota: provenant_quota::Settings,
    pub routing: provenant_routing::RoutingConfig,
    pub fabric: provenant_fabric_runtime::config::FabricConfig,
    pub guard: provenant_confinement::guard::GuardConfig,
    #[serde(default)]
    pub run: provenant_confinement::run_config::RunConfig,
    #[serde(default)]
    pub pr: PrSettings,
    #[serde(default)]
    pub lanes: LanesSettings,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrSettings {
    pub land: LandSettings,
}
pub const DEFAULT_LAND_POLL_SECONDS: u64 = 30;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LandSettings {
    pub poll_seconds: u64,
    pub integration: Integration,
    pub batch_size: u64,
    pub batch_wait_seconds: u64,
    pub candidate_ref_prefix: String,
    pub max_rebuilds: u64,
    pub publish_settle_seconds: u64,
    pub publish_unknown_warn_seconds: u64,
    pub gate: GateSettings,
    pub pr_ci: PrCiSettings,
    pub publish: Publication,
    pub method: MergeMethod,
    pub post_merge: PostMergeSettings,
    pub budget: BudgetSettings,
    pub local_gate: LocalGateSettings,
    pub projects: std::collections::BTreeMap<String, LandOverride>,
    pub tiers: std::collections::BTreeMap<String, LandOverride>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LandOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<Integration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_wait_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_ref_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rebuilds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_settle_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_unknown_warn_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_ci: Option<PrCiOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish: Option<Publication>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<MergeMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_merge: Option<PostMergeOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_gate: Option<LocalGateOverride>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<GateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_passthrough: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrCiOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<PrCiMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostMergeOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bisect: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert_forward: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_micros_per_minute: Option<std::collections::BTreeMap<String, u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory_micros: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap_micros: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalGateOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<LocalGateMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

macro_rules! land_enum {
    ($name:ident, $default:ident, $($variant:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { #[default] $default, $($variant),+ }
    };
}
land_enum!(Integration, Batch, Single, ForgeQueue);
land_enum!(Publication, Auto, RestOrdered, FastForward);
land_enum!(MergeMethod, Merge, Squash, Rebase);
land_enum!(PrCiMode, Always, OptIn, Affected, None);
land_enum!(LocalGateMode, Warn, Enforce, Off);
land_enum!(GateKind, Checks, Commands);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GateSettings {
    pub kind: GateKind,
    pub checks: Vec<String>,
    pub issuer: Option<String>,
    pub commands: Vec<String>,
    pub executor: Option<String>,
    pub pool: Option<String>,
    pub env_passthrough: Vec<String>,
    pub timeout_seconds: u64,
    pub coverage: Vec<String>,
}
impl Default for GateSettings {
    fn default() -> Self {
        Self {
            kind: GateKind::Checks,
            checks: vec!["ci-status".into()],
            issuer: None,
            commands: Vec::new(),
            executor: None,
            pool: None,
            env_passthrough: Vec::new(),
            timeout_seconds: 3600,
            coverage: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrCiSettings {
    pub mode: PrCiMode,
    pub label: Option<String>,
}
impl Default for PrCiSettings {
    fn default() -> Self {
        Self {
            mode: PrCiMode::Always,
            label: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostMergeSettings {
    pub checks: Vec<String>,
    pub bisect: bool,
    pub revert_forward: bool,
    pub pool: Option<String>,
    pub timeout_seconds: u64,
}
impl Default for PostMergeSettings {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            bisect: false,
            revert_forward: false,
            pool: None,
            timeout_seconds: 3600,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetSettings {
    /// Pool price in millionths of the configured currency per minute.
    pub rate_micros_per_minute: std::collections::BTreeMap<String, u64>,
    /// Advisory threshold in millionths of the configured currency.
    pub advisory_micros: Option<u64>,
    /// Spending cap in millionths of the configured currency.
    pub cap_micros: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalGateSettings {
    pub mode: LocalGateMode,
    pub commands: Vec<String>,
    pub timeout_seconds: u64,
}
impl Default for LocalGateSettings {
    fn default() -> Self {
        Self {
            mode: LocalGateMode::Warn,
            commands: Vec::new(),
            timeout_seconds: 60,
        }
    }
}
impl Default for LandSettings {
    fn default() -> Self {
        Self {
            poll_seconds: DEFAULT_LAND_POLL_SECONDS,
            integration: Integration::Batch,
            batch_size: 8,
            batch_wait_seconds: 30,
            candidate_ref_prefix: "land/".into(),
            max_rebuilds: 3,
            publish_settle_seconds: 30,
            publish_unknown_warn_seconds: 3600,
            gate: GateSettings::default(),
            pr_ci: PrCiSettings::default(),
            publish: Publication::Auto,
            method: MergeMethod::Merge,
            post_merge: PostMergeSettings::default(),
            budget: BudgetSettings::default(),
            local_gate: LocalGateSettings::default(),
            projects: Default::default(),
            tiers: Default::default(),
        }
    }
}
// A PR number is appended to the configured prefix before it becomes a ref.
fn valid_candidate_ref_prefix(prefix: &str) -> bool {
    if prefix.is_empty()
        || prefix.starts_with('/')
        || prefix.contains("..")
        || prefix.contains("@{")
    {
        return false;
    }
    let name = format!("{prefix}1");
    !name
        .chars()
        .any(|c| c.is_ascii_control() || c == ' ' || "~^:?*[\\".contains(c))
        && name
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}
impl DomainConfig for PrSettings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        let mut check = |invalid, field: &str, code| {
            if invalid {
                errors.push(SectionError {
                    detail: None,
                    field: field.into(),
                    code,
                });
            }
        };
        let land = &self.land;
        check(
            land.post_merge.timeout_seconds == 0,
            "land.post_merge.timeout_seconds",
            "range",
        );
        check(land.poll_seconds == 0, "land.poll_seconds", "range");
        check(land.batch_size == 0, "land.batch_size", "range");
        check(
            land.gate.timeout_seconds == 0,
            "land.gate.timeout_seconds",
            "range",
        );
        check(
            land.local_gate.timeout_seconds == 0,
            "land.local_gate.timeout_seconds",
            "range",
        );
        check(
            !valid_candidate_ref_prefix(&land.candidate_ref_prefix),
            "land.candidate_ref_prefix",
            "invalid",
        );
        check(
            land.gate.kind == GateKind::Checks
                && (land.gate.checks.is_empty()
                    || land.gate.checks.iter().any(|check| check.trim().is_empty())),
            "land.gate.checks",
            "required",
        );
        check(
            land.gate.kind == GateKind::Commands
                && (land.gate.commands.is_empty()
                    || land
                        .gate
                        .commands
                        .iter()
                        .any(|command| command.trim().is_empty())
                    || land.gate.executor.as_deref().is_none_or(str::is_empty)),
            "land.gate.commands",
            "required",
        );
        check(
            land.pr_ci.mode == PrCiMode::OptIn
                && land.pr_ci.label.as_deref().is_none_or(str::is_empty),
            "land.pr_ci.label",
            "required",
        );
        for (layer, overrides) in [("projects", &land.projects), ("tiers", &land.tiers)] {
            for (name, value) in overrides {
                let prefix = format!("land.{layer}.{name}");
                for (field, invalid) in [
                    (
                        "post_merge.timeout_seconds",
                        value.post_merge.as_ref().and_then(|p| p.timeout_seconds) == Some(0),
                    ),
                    ("poll_seconds", value.poll_seconds == Some(0)),
                    ("batch_size", value.batch_size == Some(0)),
                    (
                        "gate.timeout_seconds",
                        value.gate.as_ref().and_then(|gate| gate.timeout_seconds) == Some(0),
                    ),
                    (
                        "local_gate.timeout_seconds",
                        value
                            .local_gate
                            .as_ref()
                            .and_then(|gate| gate.timeout_seconds)
                            == Some(0),
                    ),
                ] {
                    if invalid {
                        errors.push(SectionError {
                            detail: None,
                            field: format!("{prefix}.{field}"),
                            code: "range",
                        });
                    }
                }
                if value
                    .candidate_ref_prefix
                    .as_deref()
                    .is_some_and(|prefix| !valid_candidate_ref_prefix(prefix))
                {
                    errors.push(SectionError {
                        detail: None,
                        field: format!("{prefix}.candidate_ref_prefix"),
                        code: "invalid",
                    });
                }
            }
        }
        errors
    }
}
const PR_SECTION: DomainSection = DomainSection::new::<PrSettings>("pr", |_| {
    r#"[land]
poll_seconds = 30
integration = "batch"
batch_size = 8
batch_wait_seconds = 30
candidate_ref_prefix = "land/"
max_rebuilds = 3
publish_settle_seconds = 30
publish_unknown_warn_seconds = 3600
publish = "auto"
method = "merge"
[land.gate]
kind = "checks"
checks = ["ci-status"]
commands = []
env_passthrough = []
timeout_seconds = 3600
coverage = []
[land.pr_ci]
mode = "always"
[land.post_merge]
checks = []
bisect = false
revert_forward = false
timeout_seconds = 3600
[land.budget.rate_micros_per_minute]
[land.local_gate]
mode = "warn"
commands = []
timeout_seconds = 60
"#
})
.with_fields(&[
    FieldMetadata::int("land.poll_seconds", 1, i64::MAX),
    FieldMetadata::enumeration("land.integration", &["batch", "single", "forge_queue"]),
    FieldMetadata::int("land.batch_size", 1, i64::MAX),
    FieldMetadata::new("land.batch_wait_seconds", "int"),
    FieldMetadata::new("land.candidate_ref_prefix", "string"),
    FieldMetadata::new("land.max_rebuilds", "int"),
    FieldMetadata::new("land.publish_settle_seconds", "int"),
    FieldMetadata::new("land.publish_unknown_warn_seconds", "int"),
    FieldMetadata::enumeration("land.gate.kind", &["checks", "commands"]),
    FieldMetadata::new("land.gate.checks", "array"),
    FieldMetadata::new("land.gate.issuer", "string"),
    FieldMetadata::new("land.gate.commands", "array"),
    FieldMetadata::new("land.gate.executor", "string"),
    FieldMetadata::new("land.gate.pool", "string"),
    FieldMetadata::new("land.gate.env_passthrough", "array"),
    FieldMetadata::int("land.gate.timeout_seconds", 1, i64::MAX),
    FieldMetadata::new("land.gate.coverage", "array"),
    FieldMetadata::enumeration("land.pr_ci.mode", &["always", "opt_in", "affected", "none"]),
    FieldMetadata::new("land.pr_ci.label", "string"),
    FieldMetadata::enumeration("land.publish", &["auto", "rest_ordered", "fast_forward"]),
    FieldMetadata::enumeration("land.method", &["merge", "squash", "rebase"]),
    FieldMetadata::new("land.post_merge.pool", "string"),
    FieldMetadata::int("land.post_merge.timeout_seconds", 1, i64::MAX),
    FieldMetadata::new("land.post_merge.checks", "array"),
    FieldMetadata::new("land.post_merge.bisect", "bool"),
    FieldMetadata::new("land.post_merge.revert_forward", "bool"),
    FieldMetadata::new("land.budget.rate_micros_per_minute.{pool}", "int"),
    FieldMetadata::new("land.budget.advisory_micros", "int"),
    FieldMetadata::new("land.budget.cap_micros", "int"),
    FieldMetadata::enumeration("land.local_gate.mode", &["warn", "enforce", "off"]),
    FieldMetadata::new("land.local_gate.commands", "array"),
    FieldMetadata::int("land.local_gate.timeout_seconds", 1, i64::MAX),
    FieldMetadata::int("land.projects.{project}.poll_seconds", 1, i64::MAX),
    FieldMetadata::int("land.tiers.{tier}.poll_seconds", 1, i64::MAX),
    FieldMetadata::enumeration(
        "land.projects.{project}.integration",
        &["batch", "single", "forge_queue"],
    ),
    FieldMetadata::enumeration(
        "land.tiers.{tier}.integration",
        &["batch", "single", "forge_queue"],
    ),
    FieldMetadata::int("land.projects.{project}.batch_size", 1, i64::MAX),
    FieldMetadata::int("land.tiers.{tier}.batch_size", 1, i64::MAX),
    FieldMetadata::new("land.projects.{project}.batch_wait_seconds", "int"),
    FieldMetadata::new("land.tiers.{tier}.batch_wait_seconds", "int"),
    FieldMetadata::new("land.projects.{project}.candidate_ref_prefix", "string"),
    FieldMetadata::new("land.tiers.{tier}.candidate_ref_prefix", "string"),
    FieldMetadata::new("land.projects.{project}.max_rebuilds", "int"),
    FieldMetadata::new("land.tiers.{tier}.max_rebuilds", "int"),
    FieldMetadata::new("land.projects.{project}.publish_settle_seconds", "int"),
    FieldMetadata::new(
        "land.projects.{project}.publish_unknown_warn_seconds",
        "int",
    ),
    FieldMetadata::new("land.tiers.{tier}.publish_settle_seconds", "int"),
    FieldMetadata::new("land.tiers.{tier}.publish_unknown_warn_seconds", "int"),
    FieldMetadata::enumeration("land.projects.{project}.gate.kind", &["checks", "commands"]),
    FieldMetadata::enumeration("land.tiers.{tier}.gate.kind", &["checks", "commands"]),
    FieldMetadata::new("land.projects.{project}.gate.checks", "array"),
    FieldMetadata::new("land.tiers.{tier}.gate.checks", "array"),
    FieldMetadata::new("land.projects.{project}.gate.issuer", "string"),
    FieldMetadata::new("land.tiers.{tier}.gate.issuer", "string"),
    FieldMetadata::new("land.projects.{project}.gate.commands", "array"),
    FieldMetadata::new("land.tiers.{tier}.gate.commands", "array"),
    FieldMetadata::new("land.projects.{project}.gate.pool", "string"),
    FieldMetadata::new("land.projects.{project}.gate.env_passthrough", "array"),
    FieldMetadata::new("land.tiers.{tier}.gate.pool", "string"),
    FieldMetadata::new("land.tiers.{tier}.gate.env_passthrough", "array"),
    FieldMetadata::new("land.projects.{project}.gate.executor", "string"),
    FieldMetadata::new("land.tiers.{tier}.gate.executor", "string"),
    FieldMetadata::int("land.projects.{project}.gate.timeout_seconds", 1, i64::MAX),
    FieldMetadata::int("land.tiers.{tier}.gate.timeout_seconds", 1, i64::MAX),
    FieldMetadata::new("land.projects.{project}.gate.coverage", "array"),
    FieldMetadata::new("land.tiers.{tier}.gate.coverage", "array"),
    FieldMetadata::enumeration(
        "land.projects.{project}.pr_ci.mode",
        &["always", "opt_in", "affected", "none"],
    ),
    FieldMetadata::enumeration(
        "land.tiers.{tier}.pr_ci.mode",
        &["always", "opt_in", "affected", "none"],
    ),
    FieldMetadata::new("land.projects.{project}.pr_ci.label", "string"),
    FieldMetadata::new("land.tiers.{tier}.pr_ci.label", "string"),
    FieldMetadata::enumeration(
        "land.projects.{project}.publish",
        &["auto", "rest_ordered", "fast_forward"],
    ),
    FieldMetadata::enumeration(
        "land.tiers.{tier}.publish",
        &["auto", "rest_ordered", "fast_forward"],
    ),
    FieldMetadata::enumeration(
        "land.projects.{project}.method",
        &["merge", "squash", "rebase"],
    ),
    FieldMetadata::enumeration("land.tiers.{tier}.method", &["merge", "squash", "rebase"]),
    FieldMetadata::new("land.projects.{project}.post_merge.pool", "string"),
    FieldMetadata::int(
        "land.projects.{project}.post_merge.timeout_seconds",
        1,
        i64::MAX,
    ),
    FieldMetadata::new("land.tiers.{tier}.post_merge.pool", "string"),
    FieldMetadata::int("land.tiers.{tier}.post_merge.timeout_seconds", 1, i64::MAX),
    FieldMetadata::new("land.projects.{project}.post_merge.checks", "array"),
    FieldMetadata::new("land.tiers.{tier}.post_merge.checks", "array"),
    FieldMetadata::new("land.projects.{project}.post_merge.bisect", "bool"),
    FieldMetadata::new("land.tiers.{tier}.post_merge.bisect", "bool"),
    FieldMetadata::new("land.projects.{project}.post_merge.revert_forward", "bool"),
    FieldMetadata::new("land.tiers.{tier}.post_merge.revert_forward", "bool"),
    FieldMetadata::new(
        "land.projects.{project}.budget.rate_micros_per_minute.{pool}",
        "int",
    ),
    FieldMetadata::new(
        "land.tiers.{tier}.budget.rate_micros_per_minute.{pool}",
        "int",
    ),
    FieldMetadata::new("land.projects.{project}.budget.advisory_micros", "int"),
    FieldMetadata::new("land.tiers.{tier}.budget.advisory_micros", "int"),
    FieldMetadata::new("land.projects.{project}.budget.cap_micros", "int"),
    FieldMetadata::new("land.tiers.{tier}.budget.cap_micros", "int"),
    FieldMetadata::enumeration(
        "land.projects.{project}.local_gate.mode",
        &["warn", "enforce", "off"],
    ),
    FieldMetadata::enumeration(
        "land.tiers.{tier}.local_gate.mode",
        &["warn", "enforce", "off"],
    ),
    FieldMetadata::new("land.projects.{project}.local_gate.commands", "array"),
    FieldMetadata::new("land.tiers.{tier}.local_gate.commands", "array"),
    FieldMetadata::int(
        "land.projects.{project}.local_gate.timeout_seconds",
        1,
        i64::MAX,
    ),
    FieldMetadata::int("land.tiers.{tier}.local_gate.timeout_seconds", 1, i64::MAX),
]);

pub use provenant_records::UserSkillsMode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanesSettings {
    pub user_skills: UserSkillsMode,
    pub skill_stage_max_bytes: u64,
    #[serde(default)]
    pub projects: std::collections::BTreeMap<String, ProjectLanesConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLanesConfig {
    #[serde(default)]
    pub user_skills: Option<UserSkillsMode>,
    #[serde(default)]
    pub shared_paths: Vec<PathBuf>,
    /// Capabilities granted by default to this project's writer lanes.
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub exec_roots: Vec<PathBuf>,
}

impl Default for LanesSettings {
    fn default() -> Self {
        Self {
            user_skills: UserSkillsMode::default(),
            skill_stage_max_bytes: 16 * 1024 * 1024,
            projects: Default::default(),
        }
    }
}

impl LanesSettings {
    pub fn user_skills_for(&self, project: Option<&str>) -> UserSkillsMode {
        project
            .and_then(|id| self.projects.get(id))
            .and_then(|p| p.user_skills)
            .unwrap_or(self.user_skills)
    }
}

impl DomainConfig for LanesSettings {
    fn validate(&self) -> Vec<SectionError> {
        Vec::new()
    }
}

const LANES_SECTION: DomainSection = DomainSection::new::<LanesSettings>(
    "lanes",
    |_| "user_skills = \"discover\"\nskill_stage_max_bytes = 16777216\n",
)
.with_fields(&[
    FieldMetadata::enumeration("user_skills", &["discover", "off"]),
    FieldMetadata::new("skill_stage_max_bytes", "int"),
    FieldMetadata::enumeration("projects.{project}.user_skills", &["discover", "off"]),
    FieldMetadata::new("projects.{project}.shared_paths", "array"),
    FieldMetadata::new("projects.{project}.capabilities", "array"),
    FieldMetadata::new("projects.{project}.exec_roots", "array"),
]);

static ENTRIES: [DomainSection; 9] = [
    crate::wait::SECTION,
    provenant_providers::accounts::SECTION,
    provenant_quota::SECTION,
    provenant_routing::SECTION,
    provenant_fabric_runtime::config::SECTION,
    provenant_confinement::guard::SECTION,
    provenant_confinement::run_config::SECTION,
    PR_SECTION,
    LANES_SECTION,
];
pub static SECTIONS: DomainSections<Domains> = DomainSections::new(&ENTRIES);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use provenant_core::{PathResolver, Roots};
    use serde_json::json;
    use std::path::Path;

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    fn roots(temp: &tempfile::TempDir) -> Roots {
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        inputs.resolve(&Resolver).unwrap()
    }
    fn edit(
        roots: &Roots,
        patch: serde_json::Value,
    ) -> Result<provenant_config::Edited<Domains>, provenant_config::ConfigError> {
        provenant_config::edit_config(
            roots,
            &SECTIONS,
            &provenant_config::EditOptions::new("test"),
            |document, current| provenant_config::apply_patch(&patch, document, current, &SECTIONS),
        )
    }

    #[test]
    fn login_warning_default_and_override_report_their_origins() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        std::fs::create_dir_all(roots.state.as_path()).unwrap();
        let default = provenant_config::read_with_domains(&roots, &SECTIONS).unwrap();
        assert_eq!(default.domains.quota.login_expiry_warning_seconds, 3600);
        assert!(matches!(
            default.origins["domains.quota.login_expiry_warning_seconds"],
            provenant_config::Origin::Compiled { .. }
        ));
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version = 1\n[domains.quota]\nlogin_expiry_warning_seconds = 90\n",
        )
        .unwrap();
        let configured = provenant_config::read_with_domains(&roots, &SECTIONS).unwrap();
        assert_eq!(configured.domains.quota.login_expiry_warning_seconds, 90);
        assert!(matches!(
            configured.origins["domains.quota.login_expiry_warning_seconds"],
            provenant_config::Origin::User { .. }
        ));
    }

    #[test]
    fn cargo_seed_section_is_a_known_key_and_turns_the_seed_off() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        std::fs::create_dir_all(temp.path().join("state")).unwrap();
        for text in [
            "schema_version = 1\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
            "schema_version = 1\n[gc]\nfree_floor = '1B'\n[domains.fabric.lanes.cargo_seed]\nenabled = false\n",
        ] {
            std::fs::write(temp.path().join("state/config.toml"), text).unwrap();
            let config = provenant_config::config_show(&roots, &SECTIONS).unwrap();
            assert!(config.warnings.is_empty(), "{:?}", config.warnings);
            assert!(!config.domains.fabric.lanes.cargo_seed.enabled);
        }
    }

    #[test]
    fn schema_reports_real_domain_bounds_enums_and_keyed_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let (config, revision) = provenant_config::show(&roots, &SECTIONS).unwrap();
        let schema = provenant_config::schema(&config, &SECTIONS, revision).unwrap();
        let field = |path: &str| {
            schema
                .fields
                .iter()
                .find(|field| field.path == path)
                .unwrap()
        };
        let parallel = field("domains.quota.max_parallel");
        assert_eq!(
            (parallel.kind, parallel.min, parallel.max),
            ("int", Some(1), Some(16))
        );
        let headline = field("domains.quota.display.headline");
        assert_eq!(headline.kind, "enum");
        assert_eq!(headline.values, ["auto", "5h", "weekly"]);
        let history = field("domains.quota.history_max_bytes");
        assert_eq!(
            (history.kind, history.min, history.max, &history.default),
            ("int", Some(65_536), Some(134_217_728), &json!(4_194_304))
        );
        let wait = field("domains.wait.timeout_seconds");
        assert_eq!(
            (wait.min, wait.max, &wait.default),
            (Some(1), Some(14400), &json!(300))
        );
        let max_targets = field("domains.wait.max_targets");
        assert_eq!(
            (max_targets.min, max_targets.max, &max_targets.default),
            (Some(1), Some(1024), &json!(64))
        );
        let pr_read = field("domains.wait.pr_read_timeout_seconds");
        assert_eq!(
            (pr_read.min, pr_read.max, &pr_read.default),
            (Some(1), Some(600), &json!(60))
        );
        let markers = field("domains.run.push_conflict_markers");
        assert_eq!((markers.kind, &markers.default), ("bool", &json!(true)));
        let brief_warnings = field("domains.run.brief_read_root_warnings");
        assert_eq!(
            (brief_warnings.kind, &brief_warnings.default),
            ("bool", &json!(true))
        );
        let idle = field("gc.idle_days");
        assert_eq!(
            (idle.kind, idle.min, idle.max, &idle.default),
            ("int", Some(1), Some(3650), &json!(7))
        );
        let graph_retention = field("gc.graph_checkpoint_days");
        assert_eq!(graph_retention.source, provenant_config::Layer::Default);
        assert_eq!(
            (
                graph_retention.kind,
                graph_retention.min,
                graph_retention.max,
                &graph_retention.default
            ),
            ("int", Some(1), Some(3650), &json!(90))
        );
        assert!(
            schema.fields.iter().any(
                |field| field.path == "domains.quota.polling_intervals.{adapter}.timer_seconds"
            )
        );
        assert!(
            schema
                .fields
                .iter()
                .any(|field| field.path == "gc.projects.{project}.foreign_worktrees")
        );
    }

    fn write_config(roots: &Roots, text: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(roots.state.as_path()).unwrap();
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn migrate_refuses_a_conflicting_schedule_and_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let text = "schema_version = 1\n[domains.routing]\npacing_overrides = [{ provider = \"codex\", policy = \"pace\", expires_unix_seconds = 4102444800, schedule = { expires = \"2100-01-01T00:00:00Z\" } }]\n";
        let path = write_config(&roots, text);
        let error = provenant_config::migrate(&roots, &SECTIONS, false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("schedule.expires"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(
            std::fs::read_dir(roots.state.as_path())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".bak-"))
        );
    }

    #[test]
    fn calibration_entry_is_created_by_one_patch() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let edited = edit(
            &roots,
            json!({
                "domains.quota.calibration.codex.five_hour": 40,
                "domains.quota.calibration.codex.expires": "2030-01-01T00:00:00Z",
            }),
        )
        .unwrap();
        assert_eq!(
            edited.config.domains.quota.calibration["codex"].five_hour,
            Some(40)
        );
        let (config, revision) = provenant_config::show(&roots, &SECTIONS).unwrap();
        let schema = provenant_config::schema(&config, &SECTIONS, revision).unwrap();
        assert!(schema.fields.iter().any(|field| {
            field.path == "domains.quota.calibration.codex.five_hour"
                && (field.kind, field.min, field.max) == ("int", Some(0), Some(100))
        }));
    }

    #[test]
    fn mixed_valid_invalid_domain_patch_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let first = edit(&roots, json!({"domains.quota.max_parallel": 4})).unwrap();
        assert_eq!(first.config.domains.quota.max_parallel, 4);
        let path = roots.state.as_path().join("config.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        let entries = provenant_config::history(&roots, None, 10).unwrap().len();
        let error = edit(
            &roots,
            json!({"domains.quota.max_parallel": 8, "domains.quota.display.headline": "invalid"}),
        )
        .err()
        .unwrap();
        assert!(
            matches!(
                error,
                provenant_config::ConfigError::Domain { .. }
                    | provenant_config::ConfigError::Schema { .. }
            ),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            provenant_config::history(&roots, None, 10).unwrap().len(),
            entries
        );
        let edited = edit(&roots, json!({"domains.quota.history_max_bytes": 131_072})).unwrap();
        assert_eq!(edited.config.domains.quota.history_max_bytes, 131_072);
        let edited = edit(
            &roots,
            json!({"domains.quota.polling_intervals.claude.timer_seconds": 17}),
        )
        .unwrap();
        assert_eq!(
            edited.config.domains.quota.polling_intervals["claude"].timer_seconds,
            Some(17)
        );
    }

    #[test]
    fn lanes_user_skills_defaults_to_discover_and_supports_project_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);

        // Default value is discover.
        let (config, revision) = provenant_config::show(&roots, &SECTIONS).unwrap();
        let schema = provenant_config::schema(&config, &SECTIONS, revision).unwrap();
        let field = schema
            .fields
            .iter()
            .find(|field| field.path == "domains.lanes.user_skills")
            .unwrap();
        assert_eq!(field.default, json!("discover"));
        assert_eq!(field.source, provenant_config::Layer::Default);
        assert_eq!(field.values, ["discover", "off"]);
        assert_eq!(config.domains.lanes.user_skills, UserSkillsMode::Discover);
        assert_eq!(
            config.domains.lanes.user_skills_for(None),
            UserSkillsMode::Discover
        );
        assert_eq!(
            config.domains.lanes.user_skills_for(Some("proj-a")),
            UserSkillsMode::Discover
        );

        // Can be switched to "off".
        let edited = edit(&roots, json!({"domains.lanes.user_skills": "off"})).unwrap();
        assert_eq!(edited.config.domains.lanes.user_skills, UserSkillsMode::Off);
        assert_eq!(
            edited.config.domains.lanes.user_skills_for(None),
            UserSkillsMode::Off
        );

        assert!(matches!(
            edited.config.origins.get("domains.lanes.user_skills"),
            Some(provenant_config::Origin::User { .. })
        ));
        // Project override overrides host default.
        let edited = edit(
            &roots,
            json!({
                "domains.lanes.user_skills": "off",
                "domains.lanes.projects.my-proj.user_skills": "discover",
            }),
        )
        .unwrap();
        assert_eq!(edited.config.domains.lanes.user_skills, UserSkillsMode::Off);
        assert_eq!(
            edited.config.domains.lanes.user_skills_for(Some("my-proj")),
            UserSkillsMode::Discover
        );
        assert_eq!(
            edited
                .config
                .domains
                .lanes
                .user_skills_for(Some("other-proj")),
            UserSkillsMode::Off
        );
    }

    #[test]
    fn project_paths_and_skill_stage_bound_have_defaults_and_user_origins() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let (config, _) = provenant_config::show(&roots, &SECTIONS).unwrap();
        assert_eq!(config.domains.lanes.skill_stage_max_bytes, 16 * 1024 * 1024);
        assert!(ProjectLanesConfig::default().shared_paths.is_empty());
        assert!(ProjectLanesConfig::default().capabilities.is_empty());
        assert!(ProjectLanesConfig::default().exec_roots.is_empty());
        let edited = edit(
            &roots,
            json!({
                "domains.lanes.projects.example.shared_paths": [".agent-run/locks", "~/locks"],
                "domains.lanes.projects.example.capabilities": ["postgres"],
                "domains.lanes.projects.example.exec_roots": [".local-tools"],
                "domains.lanes.skill_stage_max_bytes": 1024,
            }),
        )
        .unwrap();
        let project = &edited.config.domains.lanes.projects["example"];
        assert_eq!(
            project.shared_paths,
            [PathBuf::from(".agent-run/locks"), PathBuf::from("~/locks")]
        );
        assert_eq!(project.exec_roots, [PathBuf::from(".local-tools")]);
        assert_eq!(project.capabilities, ["postgres"]);
        assert_eq!(edited.config.domains.lanes.skill_stage_max_bytes, 1024);
        for key in [
            "domains.lanes.projects.example.shared_paths",
            "domains.lanes.projects.example.capabilities",
            "domains.lanes.projects.example.exec_roots",
            "domains.lanes.skill_stage_max_bytes",
        ] {
            assert!(matches!(
                edited.config.origins.get(key),
                Some(provenant_config::Origin::User { .. })
            ));
        }
    }
    #[test]
    fn landing_settings_defaults_and_validation() {
        let mut settings = PrSettings::default();
        assert_eq!(settings.land.integration, Integration::Batch);
        assert_eq!(settings.land.batch_size, 8);
        assert_eq!(settings.land.batch_wait_seconds, 30);
        assert_eq!(settings.land.gate.checks, ["ci-status"]);
        assert_eq!(settings.land.publish, Publication::Auto);
        assert!(!settings.land.post_merge.revert_forward);
        assert!(settings.validate().is_empty());
        assert!(valid_candidate_ref_prefix("queue-"));
        assert!(!valid_candidate_ref_prefix("land/../"));
        assert!(!valid_candidate_ref_prefix("land//"));
        settings.land.batch_size = 0;
        settings.land.pr_ci.mode = PrCiMode::OptIn;
        settings.land.gate.kind = GateKind::Commands;
        let errors = settings.validate();
        for field in ["land.batch_size", "land.pr_ci.label", "land.gate.commands"] {
            assert!(errors.iter().any(|error| error.field == field));
        }
    }

    #[test]
    fn landing_partial_overrides_load_and_reject_invalid_ranges() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let edited = edit(
            &roots,
            json!({
                "domains.pr.land.projects.demo.batch_size": 3,
                "domains.pr.land.projects.demo.gate.timeout_seconds": 42,
                "domains.pr.land.tiers.high.publish": "rest_ordered"
            }),
        )
        .unwrap();
        let land = &edited.config.domains.pr.land;
        assert_eq!(land.projects["demo"].batch_size, Some(3));
        assert_eq!(
            land.projects["demo"].gate.as_ref().unwrap().timeout_seconds,
            Some(42)
        );
        assert_eq!(land.tiers["high"].publish, Some(Publication::RestOrdered));
        let mut settings = PrSettings::default();
        settings.land.projects.insert(
            "demo".into(),
            LandOverride {
                batch_size: Some(0),
                ..LandOverride::default()
            },
        );
        assert!(
            settings
                .validate()
                .iter()
                .any(|error| error.field == "land.projects.demo.batch_size")
        );
    }

    #[test]
    fn landing_settings_round_trip_and_origins() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        let (config, _) = provenant_config::show(&roots, &SECTIONS).unwrap();
        assert_eq!(config.domains.pr.land, LandSettings::default());
        let edited = edit(
            &roots,
            json!({
                "domains.pr.land.integration": "single",
                "domains.pr.land.batch_size": 4,
                "domains.pr.land.gate.checks": ["test"],
            }),
        )
        .unwrap();
        assert_eq!(
            edited.config.domains.pr.land.integration,
            Integration::Single
        );
        assert_eq!(edited.config.domains.pr.land.gate.checks, ["test"]);
        assert!(matches!(
            edited.config.origins.get("domains.pr.land.batch_size"),
            Some(provenant_config::Origin::User { .. })
        ));
    }
}
