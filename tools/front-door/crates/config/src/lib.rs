//! One bounded, user-owned TOML configuration and immutable startup snapshots.
//!
//! `load` accepts an already opened optional reader; `read` uses supervisor's
//! no-follow filesystem boundary. CLI owns envelopes, pagination and exits:
//! syntax errors map to invalid/2 and check findings to findings/1.
#![forbid(unsafe_code)]

mod domains;
mod loader;
mod managed;
mod presets;
mod release;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use domains::{DomainConfig, DomainSection, DomainSections, NoDomains, SectionError};
pub use loader::{check_config, config_show, load, load_with_domains, read, read_with_domains};
pub use presets::Profile;
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

pub use managed::{
    Action, Disposition, HarnessTarget, InstallContext, InstallError, InstallFinding,
    InstallOutcome, InstallPlan, InstallReporter, ReleaseSmoke, Snapshots, Targets, check_install,
    install, plan_install, uninstall,
};
pub use provenant_records::Harness;
pub use release::{Release, package_release};
