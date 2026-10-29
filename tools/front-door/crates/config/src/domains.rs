use crate::Profile;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{marker::PhantomData, path::Path};
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

/// Read one domain's table straight from `<state>/config.toml`, without
/// resolving the other domains, for a settlement or status reader that does
/// not link the binary's closed aggregate. A missing file or table answers the
/// section's serde defaults. Unknown fields warn; invalid known authority input,
/// syntax, version, size and I/O errors remain hard failures.
pub fn read_domain<T: DeserializeOwned + Default>(state: &Path, name: &str) -> std::io::Result<T> {
    crate::loader::read_boundary_settings(state, &format!("domains.{name}"))
}

/// Editor metadata for one field the owning domain accepts. Paths are
/// section-local; a `{name}` segment stands for one keyed map entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldMetadata {
    pub path: &'static str,
    pub kind: &'static str,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub values: &'static [&'static str],
    pub restart_required: bool,
}
impl FieldMetadata {
    pub const fn new(path: &'static str, kind: &'static str) -> Self {
        Self {
            path,
            kind,
            min: None,
            max: None,
            values: &[],
            restart_required: false,
        }
    }
    pub const fn int(path: &'static str, min: i64, max: i64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
            ..Self::new(path, "int")
        }
    }
    pub const fn enumeration(path: &'static str, values: &'static [&'static str]) -> Self {
        Self {
            values,
            ..Self::new(path, "enum")
        }
    }
}

/// One domain's immutable entry. Assets contain section-local keys: config
/// nests them under `[domains.<name>]`. Defaults include every accepted key.
/// Arrays replace as a whole; tables (including inline tables) merge recursively.
pub struct DomainSection {
    pub name: &'static str,
    pub defaults: fn(Profile) -> &'static str,
    pub validate: fn(&Table) -> Vec<SectionError>,
    /// Advisory findings that do not trigger recovery.
    pub advisories: fn(&Table) -> Vec<SectionError>,
    /// The owner identifies invalid input that controls a real boundary.
    pub boundary: fn(&SectionError) -> bool,
    /// Owner-published types, bounds and variants for the settings editor.
    pub fields: &'static [FieldMetadata],
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
            advisories: |_| Vec::new(),
            boundary: |_| false,
            fields: &[],
        }
    }
    pub const fn with_fields(mut self, fields: &'static [FieldMetadata]) -> Self {
        self.fields = fields;
        self
    }
    pub const fn with_boundary_checks(mut self) -> Self {
        self.boundary = |_| true;
        self
    }
    pub const fn with_boundary_check(mut self, boundary: fn(&SectionError) -> bool) -> Self {
        self.boundary = boundary;
        self
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
