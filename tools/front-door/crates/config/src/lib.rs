//! One bounded, user-owned TOML configuration and immutable startup snapshots.
//!
//! `load` accepts an already opened optional reader; `read` uses the OS crate's
//! no-follow filesystem boundary. CLI owns envelopes, pagination and exits:
//! library errors retain their typed cause; CLI classifies inspected defects.
#![forbid(unsafe_code)]

pub mod admission;
pub use admission::{
    ADMISSION_PRECEDENCE, AdmissionClassConfig, DEFAULT_CLASS, DEFAULT_WAIT_SECONDS,
    ResolvedAdmissionClass,
};
mod account_sync;
pub mod adapter_capabilities;
mod cache_budget;
mod domains;
mod edit;
mod gc;
mod github_policy;
mod history;
mod schema;
pub use cache_budget::resolve as cargo_cache_budget;
pub use cache_budget::{CargoCacheBudget, KNOB as CARGO_CACHE_MAX_KNOB};
pub use gc::{
    ByteObservation, DISK_FLOOR, DiskGate, DiskLevel, DiskLimits, ForeignWorktrees, GcSettings,
    LaneBuildSettings, MemoryPressureThreshold, PROJECT_WRITER_LANES_CEILING, ProjectGcSettings,
    RETENTION_CLASSES, RemoteBranches, RetentionAdapter, RetentionBytes, RetentionClass,
    RetentionCollector, RetentionEligibility, RetentionIssue, RetentionKind, RetentionMeasurements,
    RetentionOwner, RetentionTrigger, STORE_BUSY_TIMEOUT_CEILING_MS, StoreSettings,
    WRITER_LANES_CEILING, deserialize_auto, parse_cache_size, parse_gc_idle_duration,
    retention_schedule,
};
pub use github_policy::{
    DEFAULT_GH_BUDGET, GH_INVOCATIONS_FILE, GithubPolicy, label_events_trigger_ci,
};
pub use pr_policy::{KNOB as PR_KNOB, PrPolicy, ResolvedPrPolicy};
mod harness;
mod hooks;
pub use hooks::{hook_plugin_source, lane_hook_settings, registrations as hook_registrations};
pub mod hooks_config;
pub use hooks_config::{
    DEFAULT_HOOK_TIMEOUT_SECS, HOOK_BUDGET_PERCENT, SECTION as HOOKS_SECTION,
    Settings as HookSettings, hook_budget,
};
mod inspection;
mod knobs;
mod lane_history;
mod loader;
mod managed;
mod pr_policy;
pub use harness::{InstallSettings, TargetSettings, apply_install_config_with_domains};
pub const NATIVE_CAPABILITY_UNAVAILABLE: &str = "native_capability_unavailable";
pub mod overlays;
pub use overlays::{OVERLAY_PRECEDENCE, OverlayLayer, OverlayWarning};
pub mod policy;
mod presets;
mod providers;
pub use provenant_records::StallPolicy;
pub use providers::{
    OutOfScopePin, ProviderSettings, ProviderUpdate, defaults as provider_defaults,
};
pub mod detect;
pub mod forge;
pub use forge::{
    Config as ForgeConfig, ForgeContext, ForgeContextError, ForgeProbe, Git as ForgeGit,
    NoForgeProbe, RemoteName, RemoteNameError, operation_policy, resolve_for, resolve_in,
};
pub mod ports;
mod project_binding;
pub use ports::{
    Adapter, BuildClean, BuildOffload, BuildOverflow, BuildPort, BuildPriority, BuildSoakPort,
    BuildStrategyName, BuildTargetKey, CommandExitCodes, DEFAULT_BUILD_TARGET_KEYS,
    DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS, ExitDisposition, Forge, ForgeOperation, ForgePort,
    ForgeResponse, GitPort, HeadersObservation, Integration, LandBackend, LandPort, PORTS_CONFIG,
    PORTS_SCHEMA_VERSION, PackagePath, Port, PortBackend, PortBackendKind, PortCommand,
    PortCommandBackend, PortCommandDisposition, PortCommandExecution, PortCommandPublication,
    PortCommandReceipt, PortCommandReceiptCollectionLock, PortCommandReceiptEntry,
    PortCommandReceiptInventory, PortCommandReceiptProof, PortError, PortEvidenceError,
    PortExecution, PortReceiptIssue, PortReceiptLifetime, PortTable, ResolvedBuildValue,
    TierCommand, VerifyAffectedPort, VerifyFormat, VerifyPort, VerifySelection, VerifyStaticCheck,
    VerifyTier, WorktreeLayoutPort, dispatch_port_backend, execute_port_command,
    execute_port_command_with_input, infer_verify_base, land_backend, port_command_environment,
    port_command_receipt_proof, port_command_receipts, port_declaration_warnings,
    port_source_layer, project_key, project_key_for, record_port_command,
    remove_port_command_receipt, try_lock_port_command_receipts, verify_base_unset_error,
};
mod release;
pub mod repository_files;
mod repository_policy;
pub use repository_policy::{REPOSITORY_CONFIG, RepositoryPolicy, read_declaration, read_land};
mod retention;
mod warning;
mod worktrees;
pub use worktrees::{ProjectWorktreeSettings, WorktreeSettings, WorktreeSetup};
pub mod shared_settings;
pub use retention::{ProjectRetentionSettings, RetentionSettings};
pub use warning::{WarningsSettings, warnings_dedup_retention_days};
#[cfg(test)]
mod edit_tests;
mod splice;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tolerance_tests;

pub use domains::{
    DomainConfig, DomainSection, DomainSections, FieldMetadata, NoDomains, SectionError,
    read_domain, read_domain_with_keys,
};
pub use edit::{EditOptions, Edited, edit_config, revision_of, show};
pub use history::{FieldChange, HistoryEntry, HistoryLimits, history};
pub use knobs::{KNOB_PRECEDENCE, KnobEdit, knob_source};
pub use lane_history::{
    CONFLICT_MARKERS, DEFAULT_BRANCH_REWRITE, DefaultBranchRewrite, HistoryEffect, HistoryValue,
    LaneHistory, REBASE, Rebase, Setting, UNROOTED_TIP,
};
pub use loader::{
    config_show, inspect_config, load, load_with_domains, read, read_knobs, read_with_domains,
};
pub use presets::Profile;
pub use project_binding::{
    GcProjectSelection, GcProjectStatus, ProjectBinding, ProjectLocation, Retirement, bind_host,
    bind_project, forget_project, forget_project_at_state, host_project_dir, lookup_gc_project,
    lookup_host, lookup_project, lookup_project_at_state, project_binding_dir,
    registered_gc_project_selection, registered_gc_project_selection_with_progress,
    registered_gc_projects, registered_projects, retire_vanished_projects, transient_location,
};
pub use provenant_records::{Finding, Layer};
pub use schema::{
    DEDICATED_COMMANDS, DeclaredEnum, SCHEMA_PRECEDENCE, Schema, SchemaField, apply_edit,
    apply_patch, declared_enums, dedicated_command, probe_field, schema,
};

use serde::Serialize;
use std::{collections::BTreeMap, fmt, path::PathBuf};
use thiserror::Error;

/// Errors contain no TOML text or parser excerpts; only numeric graph policy
/// values may be included when rejected.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot encode config path at {path}")]
    PathEncoding {
        path: PathBuf,
        #[source]
        source: provenant_records::PathWireError,
    },
    #[error("cannot read config at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: provenant_os::fs::FsError,
    },
    #[error("config at {path} exceeds the {limit}-byte limit")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("config at {path} has invalid UTF-8")]
    InvalidUtf8 { path: PathBuf },
    #[error("invalid TOML at {location}")]
    Syntax { location: Location },
    #[error("{rule} at {location}; next: run `provenant config check` and edit the reported key")]
    Schema {
        location: Location,
        rule: &'static str,
    },
    #[error("config needs schema_version = 1 at {location}")]
    MissingVersion { location: Location },
    #[error("unsupported config schema version at {location}")]
    UnsupportedVersion { location: Location },
    #[error("config exceeds collection bounds at {location}")]
    CollectionLimit { location: Location },
    #[error(
        "invalid domain configuration at {location}: {source}{rejected}; next: provenant config check"
    )]
    Domain {
        location: Location,
        #[source]
        source: Box<SectionError>,
        /// Safe numeric graph policy value, when available.
        rejected: Box<String>,
    },
    #[error("config edit refused: {reason}")]
    Edit { reason: &'static str },
    #[error("config edit refused: {0}")]
    Refused(String),
    #[error("config changed since revision was read; current revision is {current}")]
    StaleRevision { current: String },
    #[error("another config edit holds the lock")]
    Locked,
    #[error("unknown knob {name}{}", close_matches(.close))]
    UnknownKnob { name: String, close: Vec<String> },
    #[error("invalid value for knob {name} ({rule})")]
    KnobValue { name: String, rule: &'static str },
    #[error("knob registry or config text is malformed")]
    KnobRegistry,
    #[error("project location must be an absolute canonical directory path")]
    InvalidProjectLocation,
    #[error("project GitHub policy is invalid")]
    InvalidGithubPolicy,
    #[error("repository launch policy is invalid (.provenant/config.toml [launch_policy])")]
    InvalidRepositoryPolicy,
    #[error("project binding state is invalid")]
    InvalidProjectBinding,
    #[error("project binding directory ownership or permissions are invalid")]
    ProjectBindingDirectoryUnsafe {
        path: PathBuf,
        mode: u32,
        owner_matches: bool,
    },
    #[error("project binding lock budget is invalid")]
    ProjectBindingBudget,
    #[error(transparent)]
    ProjectBindingFilesystem(#[from] provenant_os::fs::FsError),
}

impl ConfigError {
    /// A stable domain refusal code, when its owner provides one.
    pub fn refusal_code(&self) -> Option<&'static str> {
        match self {
            Self::Domain { source, .. } if source.code == shared_settings::NEVER_SYNCED_CODE => {
                Some(source.code)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Compiled {
        profile: Profile,
    },
    /// Config file path relative to the explicitly selected state root.
    User {
        path: provenant_records::PathWire,
    },
}

impl Origin {
    /// Retain the source in a forward-extensible record without making the
    /// configuration resolver's own origin an input codec.
    pub fn receipt(&self) -> provenant_records::run::OriginReceipt {
        use provenant_records::run::OriginReceipt;
        match self {
            Self::Compiled { profile } => OriginReceipt::Compiled {
                profile: Some(profile.as_str().into()),
                extra: Default::default(),
            },
            Self::User { path } => OriginReceipt::User {
                path: path.clone(),
                extra: Default::default(),
            },
        }
    }
}

/// One-based character line/column and the dotted key path in the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    pub origin: Origin,
    pub key_path: String,
    pub line: usize,
    pub column: usize,
}
impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            Origin::Compiled { profile } => {
                write!(
                    f,
                    "the compiled {} profile ({})",
                    profile.as_str(),
                    self.key_path
                )
            }
            Origin::User { path } => {
                write!(
                    f,
                    "{}:{}:{} ({})",
                    path.as_path_text().unwrap_or("config.toml"),
                    self.line,
                    self.column,
                    self.key_path
                )
            }
        }
    }
}
fn close_matches(close: &[String]) -> String {
    if close.is_empty() {
        String::new()
    } else {
        format!("; did you mean {}?", close.join(", "))
    }
}

/// `D` is composition's closed, typed aggregate of registered domain sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedConfig<D = NoDomains> {
    pub schema_version: u64,
    pub profile: Profile,
    pub domains: D,
    pub install: InstallSettings,
    pub gc: GcSettings,
    pub retention: RetentionSettings,
    pub worktrees: WorktreeSettings,
    pub providers: BTreeMap<String, ProviderSettings>,
    /// Effective knobs with the layer that set each; not part of `origins`.
    pub knobs: BTreeMap<String, provenant_records::Sourced<serde_json::Value>>,
    pub origins: BTreeMap<String, Origin>,
    /// Per-change warning memory. Skipped from the serialised config because
    /// `warnings` above is already the key for ignored-setting findings; the
    /// effective value and its source are published by `config schema`.
    #[serde(skip_serializing)]
    pub warning_dedup: WarningsSettings,
    /// Every ignored user setting, ordered by source location.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Finding>,
}

/// A bounded first page, ordered by source line, column, key and rule. `count`
/// includes omitted findings. CLI owns digest-bound cursors for further pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigCheck {
    pub schema_version: u64,
    pub profile: Profile,
    pub findings: Vec<Finding>,
    pub count: usize,
}

/// Complete, ordered findings and the exact configuration input identity.
#[derive(Debug)]
pub struct ConfigInspection {
    pub check: ConfigCheck,
    /// Malformed configuration remains typed; CLI can report it as a finding
    /// while still binding pagination to the same bytes inspected here.
    pub error: Option<ConfigError>,
    pub input_digest: provenant_records::FileDigest,
}

pub use managed::{
    AccountHome, AccountState, AccountStatus, Action, Disposition, EntryIdentity,
    HOOKS_UNAVAILABLE_DETAIL, HarnessTarget, InstallContext, InstallError, InstallFinding,
    InstallInspection, InstallOutcome, InstallPlan, InstallReporter, ManualInclude, ReleaseSmoke,
    STABLE_BINARY_NAME, SelectorChange, Snapshots, Targets, inspect_accounts, inspect_install,
    install, installed_binary, peer_agent_plist, plan_and_install, plan_install, private_directory,
    release_instruction_asset, release_root, systemd_user_dir, tick_agent_plist, tick_service_path,
    tick_timer_path, uninstall, user_dir_in,
};
pub use provenant_records::Harness;
pub use release::{Release, package_release};

/// Schema version accepted by the single-file configuration owner.
pub const CONFIG_SCHEMA_VERSION: u64 = 1;

pub use managed::HookRegistration;

pub use knobs::documented_field;
