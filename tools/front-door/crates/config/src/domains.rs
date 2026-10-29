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
/// section's serde defaults; an unreadable or malformed one fails.
pub fn read_domain<T: DeserializeOwned>(state: &Path, name: &str) -> std::io::Result<T> {
    let read = provenant_os::fs::Directory::open_anchor(state).and_then(|directory| {
        directory.read_bounded(
            "config.toml".as_ref(),
            provenant_records::Limits::CONFIG.max_bytes,
        )
    });
    let table = match read {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
            let document = text.parse::<DocumentMut>().map_err(std::io::Error::other)?;
            document
                .get("domains")
                .and_then(|domains| domains.get(name))
                .and_then(|item| item.as_table_like())
                .map(|table| {
                    table.iter().fold(Table::new(), |mut out, (key, item)| {
                        out.insert(key, item.clone());
                        out
                    })
                })
        }
        Err(error) if error.is_not_found() => None,
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let document = DocumentMut::from(table.unwrap_or_default());
    toml_edit::de::from_document(document).map_err(std::io::Error::other)
}

/// One domain's immutable entry. Assets contain section-local keys: config
/// nests them under `[domains.<name>]`. Defaults include every accepted key.
/// Arrays replace as a whole; tables (including inline tables) merge recursively.
pub struct DomainSection {
    pub name: &'static str,
    pub defaults: fn(Profile) -> &'static str,
    pub validate: fn(&Table) -> Vec<SectionError>,
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
