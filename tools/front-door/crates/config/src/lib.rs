//! One bounded, user-owned TOML configuration and immutable startup snapshots.
//!
//! `load` accepts an already opened optional reader; `read` uses supervisor's
//! no-follow filesystem boundary. Neither discovers environment, creates state,
//! reads project config, nor contacts a provider. CLI owns envelopes and exits.
#![forbid(unsafe_code)]

mod domains;
mod loader;
mod presets;
#[cfg(test)]
mod tests;

pub use domains::{DomainSections, NoDomains, SectionError};
pub use loader::{check_config, config_show, load, load_with_domains, read, read_with_domains};
pub use presets::{CrossFamily, EffortPolicy, PoolSelection, Preset, Profile, RereviewScope};

use serde::Serialize;
use std::{collections::BTreeMap, io, path::PathBuf};
use thiserror::Error;

/// Errors never contain TOML text, rejected values or a parser's input excerpt.
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
        source: io::Error,
    },
    #[error("config at {path} exceeds the {limit}-byte limit")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("config at {path} has invalid UTF-8")]
    InvalidUtf8 { path: PathBuf },
    #[error("invalid TOML at {path}, byte {offset}")]
    Syntax { path: PathBuf, offset: usize },
    #[error("config at {path} does not match its closed schema, byte {offset}")]
    Schema { path: PathBuf, offset: usize },
    #[error("config at {path} needs schema_version = 1")]
    MissingVersion { path: PathBuf },
    #[error("unsupported config schema version at {path}")]
    UnsupportedVersion { path: PathBuf },
    #[error("config at {path} exceeds collection bounds")]
    CollectionLimit { path: PathBuf },
    #[error("invalid domain configuration at {path}: {source}")]
    Domain {
        path: PathBuf,
        #[source]
        source: SectionError,
    },
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

/// The startup snapshot contains typed, validated values, never an untyped bag.
/// `D` is the composition edge's closed aggregate of registered domain sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedConfig<D = NoDomains> {
    pub schema_version: u64,
    pub profile: Profile,
    pub preset: Preset,
    pub domains: D,
    pub origins: BTreeMap<String, Origin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigCheck {
    pub schema_version: u64,
    pub profile: Profile,
}
