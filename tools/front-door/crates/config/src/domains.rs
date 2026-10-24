use crate::Profile;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::marker::PhantomData;
use thiserror::Error;
use toml_edit::{DocumentMut, Table};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDomains {}

/// Domain-local dotted field path and a static code; never a rejected value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{field}: {code}")]
pub struct SectionError {
    pub field: String,
    pub code: &'static str,
}

/// Implement on a domain's closed serde section, alongside its TOML assets.
pub trait DomainConfig: DeserializeOwned {
    /// Return all independent semantic failures. An empty vector means valid.
    fn validate(&self) -> Vec<SectionError> {
        Vec::new()
    }
}

/// One domain's immutable entry. Assets contain section-local keys: config
/// nests them under `[domains.<name>]`. Defaults include every accepted key.
/// Arrays replace as a whole; tables (including inline tables) merge recursively.
pub struct DomainSection {
    pub name: &'static str,
    pub defaults: fn(Profile) -> &'static str,
    pub validate: fn(&Table) -> Vec<SectionError>,
    pub root_alias: bool,
}
impl DomainSection {
    /// Erase only the registration function, never the resolved section type.
    pub const fn new<T: DomainConfig>(
        name: &'static str,
        defaults: fn(Profile) -> &'static str,
    ) -> Self {
        Self {
            name,
            defaults,
            validate: validate::<T>,
            root_alias: false,
        }
    }

    /// Register a section accepted both under `[domains.<name>]` and at the
    /// root as `[<name>]`; root values override matching domain values.
    pub const fn new_root_alias<T: DomainConfig>(
        name: &'static str,
        defaults: fn(Profile) -> &'static str,
    ) -> Self {
        Self {
            name,
            defaults,
            validate: validate::<T>,
            root_alias: true,
        }
    }
}
fn validate<T: DomainConfig>(table: &Table) -> Vec<SectionError> {
    match toml_edit::de::from_document::<T>(DocumentMut::from(table.clone())) {
        Ok(value) => value.validate(),
        Err(error) => vec![SectionError {
            field: crate::loader::key_at(table, error.span().map_or(0, |s| s.start)),
            code: "schema",
        }],
    }
}

/// Composition supplies a closed aggregate `T` and the domain-owned entries.
/// No domain content, runtime dependency, registry mutation or const-concat is
/// needed in config. Each domain's name must be unique and a simple identifier.
pub struct DomainSections<T> {
    pub entries: &'static [DomainSection],
    marker: PhantomData<fn() -> T>,
}
impl<T> DomainSections<T> {
    pub const fn new(entries: &'static [DomainSection]) -> Self {
        Self {
            entries,
            marker: PhantomData,
        }
    }
}
pub(crate) const NO_DOMAINS: DomainSections<NoDomains> = DomainSections::new(&[]);
