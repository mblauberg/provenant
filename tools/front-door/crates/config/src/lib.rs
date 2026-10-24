//! One bounded, user-owned TOML configuration and immutable startup snapshots.
//!
//! `load` accepts an already opened optional reader; `read` uses supervisor's
//! no-follow filesystem boundary. CLI owns envelopes, pagination and exits:
//! library errors retain their typed cause; CLI classifies inspected defects.
#![forbid(unsafe_code)]

mod domains;
mod inspection;
mod loader;
mod managed;
mod presets;
mod project_binding;
mod release;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use domains::{DomainConfig, DomainSection, DomainSections, NoDomains, SectionError};
pub use loader::{
    check_config, config_show, inspect_config, load, load_with_domains, read, read_with_domains,
};
pub use presets::Profile;
pub use project_binding::{ProjectBinding, ProjectLocation, bind_host, bind_project};
pub use provenant_records::Finding;

use serde::{Deserialize, Serialize, Serializer};
use std::{collections::BTreeMap, fmt, path::PathBuf};
use thiserror::Error;

/// Errors never contain TOML text, rejected values or parser input excerpts.
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
        source: provenant_supervisor::fs::FsError,
    },
    #[error("config at {path} exceeds the {limit}-byte limit")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("config at {path} has invalid UTF-8")]
    InvalidUtf8 { path: PathBuf },
    #[error("invalid TOML at {location}")]
    Syntax { location: Location },
    #[error("{rule} at {location}")]
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
    #[error("invalid domain configuration at {location}: {source}")]
    Domain {
        location: Location,
        #[source]
        source: SectionError,
    },
    #[error("finding limit must be between 1 and 100")]
    InvalidLimit,
    #[error("project location must be an absolute canonical directory path")]
    InvalidProjectLocation,
    #[error("project binding state is invalid")]
    InvalidProjectBinding,
    #[error("project binding lock budget is invalid")]
    ProjectBindingBudget,
    #[error(transparent)]
    ProjectBindingFilesystem(#[from] provenant_supervisor::fs::FsError),
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
        write!(
            f,
            "{:?} {}:{}:{}",
            self.origin, self.key_path, self.line, self.column
        )
    }
}

/// Secret values can be decoded and used by their owner but every serialiser
/// and Debug formatter emits a placeholder. Display/show cannot expose `T`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);
impl<T> Redacted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &T {
        &self.0
    }
}
impl<T> Serialize for Redacted<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[redacted]")
    }
}
impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// `D` is composition's closed, typed aggregate of registered domain sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedConfig<D = NoDomains> {
    pub schema_version: u64,
    pub profile: Profile,
    pub domains: D,
    pub origins: BTreeMap<String, Origin>,
}

/// A bounded first page, ordered by source line, column, key and rule. `count`
/// includes omitted findings. CLI owns digest-bound cursors for further pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigCheck {
    pub schema_version: u64,
    pub profile: Profile,
    pub findings: Vec<Finding>,
    pub count: usize,
    pub truncated: bool,
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
    Action, Disposition, EntryIdentity, HarnessTarget, InstallContext, InstallError,
    InstallFinding, InstallInspection, InstallOutcome, InstallPlan, InstallReporter, ManualInclude,
    ReleaseSmoke, Snapshots, Targets, check_install, inspect_install, install, plan_install,
    uninstall,
};
pub use provenant_records::Harness;
pub use release::{Release, package_release};

/// Schema version accepted by the single-file configuration owner.
pub const CONFIG_SCHEMA_VERSION: u64 = 1;
