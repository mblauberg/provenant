use crate::{
    ConfigCheck, ConfigError, DomainSections, Origin, Profile, ResolvedConfig, domains::NO_DOMAINS,
};
use provenant_core::Roots;
use provenant_records::Limits;
use provenant_supervisor::fs::SafeFs;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{self, Read},
    path::Path,
};
use toml_edit::{Document, DocumentMut, Item, Table, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u64,
    #[serde(default)]
    profile: Option<Profile>,
}

/// Absence alone selects compiled defaults. An empty present file is invalid.
/// The reader must be opened at `<roots.state>/config.toml` by the caller.
pub fn load<R: Read>(roots: &Roots, reader: Option<R>) -> Result<ResolvedConfig, ConfigError> {
    load_with_domains(roots, reader, &NO_DOMAINS)
}

pub fn load_with_domains<R: Read, T: DeserializeOwned + Serialize>(
    roots: &Roots,
    reader: Option<R>,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let user_origin = Origin::User {
        path: provenant_records::PathWire::from_path(&path).map_err(|source| {
            ConfigError::PathEncoding {
                path: path.clone(),
                source,
            }
        })?,
    };
    let mut origins = BTreeMap::new();
    let mut user_domains = Table::new();
    let header = match reader {
        Some(reader) => {
            let text = bounded_text(reader, &path)?;
            let mut doc = parse(&text, &path)?;
            if doc.get("schema_version").is_none() {
                return Err(ConfigError::MissingVersion { path });
            }
            if let Some(item) = doc.remove("domains") {
                user_domains = item.into_table().map_err(|_| schema(&path, None))?;
            }
            let header: Header =
                toml_edit::de::from_document(doc).map_err(|e| schema(&path, e.span()))?;
            if header.schema_version != 1 {
                return Err(ConfigError::UnsupportedVersion { path });
            }
            origins.insert("schema_version".into(), user_origin.clone());
            header
        }
        None => Header {
            schema_version: 1,
            profile: None,
        },
    };
    let profile = header.profile.unwrap_or_default();
    let compiled = Origin::Compiled { profile };
    origins
        .entry("schema_version".into())
        .or_insert_with(|| compiled.clone());
    origins.insert(
        "profile".into(),
        if header.profile.is_some() {
            user_origin.clone()
        } else {
            compiled.clone()
        },
    );
    let preset = profile.preset()?;
    origins.insert("preset".into(), compiled.clone());
    let mut defaults = parse(
        sections.defaults(profile),
        Path::new("compiled-domain-defaults"),
    )?;
    normalise_tables(defaults.as_table_mut());
    normalise_tables(&mut user_domains);
    record_origins(defaults.as_table(), "domains", &compiled, &mut origins);
    record_origins(&user_domains, "domains", &user_origin.clone(), &mut origins);
    merge(defaults.as_table_mut(), user_domains);
    // Decode the complete result once: unknown sections/fields cannot hide in
    // overrides, and a partial override still receives selected preset defaults.
    let domains = toml_edit::de::from_document(defaults).map_err(|e| schema(&path, e.span()))?;
    (sections.validate)(&domains).map_err(|source| ConfigError::Domain { path, source })?;
    Ok(ResolvedConfig {
        schema_version: header.schema_version,
        profile,
        preset,
        domains,
        origins,
    })
}

/// No-follow read at the bound state root. A dangling link, directory, permission
/// error or I/O failure is not absence. Never creates a directory or lock.
pub fn read(roots: &Roots, fs: &impl SafeFs) -> Result<ResolvedConfig, ConfigError> {
    read_with_domains(roots, fs, &NO_DOMAINS)
}
pub fn read_with_domains<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    fs: &impl SafeFs,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let dir = match fs.open_anchor(roots.state.as_path()) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return load_with_domains::<io::Empty, _>(roots, None, sections);
        }
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    let file = match dir.open_read(OsStr::new("config.toml")) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    load_with_domains(roots, file, sections)
}

/// Library outcomes: CLI wraps these with its command envelope and exit mapping.
pub fn config_show(roots: &Roots, fs: &impl SafeFs) -> Result<ResolvedConfig, ConfigError> {
    read(roots, fs)
}
pub fn check_config(roots: &Roots, fs: &impl SafeFs) -> Result<ConfigCheck, ConfigError> {
    let snapshot = read(roots, fs)?;
    Ok(ConfigCheck {
        schema_version: snapshot.schema_version,
        profile: snapshot.profile,
    })
}

fn bounded_text(reader: impl Read, path: &Path) -> Result<String, ConfigError> {
    let mut bytes = Vec::new();
    reader
        .take(Limits::CONFIG.max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigError::Read {
            path: path.into(),
            source,
        })?;
    if bytes.len() as u64 > Limits::CONFIG.max_bytes {
        return Err(ConfigError::TooLarge {
            path: path.into(),
            limit: Limits::CONFIG.max_bytes,
        });
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::InvalidUtf8 { path: path.into() })
}
fn parse(text: &str, path: &Path) -> Result<DocumentMut, ConfigError> {
    let doc = Document::parse(text).map_err(|e| ConfigError::Syntax {
        path: path.into(),
        offset: e.span().map_or(0, |s| s.start),
    })?;
    let mut members = 0;
    check_bounds(doc.as_item(), 0, &mut members, path)?;
    Ok(DocumentMut::from(doc.into_table()))
}
pub(crate) fn parse_typed<T: DeserializeOwned>(text: &str, path: &Path) -> Result<T, ConfigError> {
    toml_edit::de::from_document(parse(text, path)?).map_err(|e| schema(path, e.span()))
}
fn schema(path: &Path, span: Option<std::ops::Range<usize>>) -> ConfigError {
    ConfigError::Schema {
        path: path.into(),
        offset: span.map_or(0, |s| s.start),
    }
}
fn check_bounds(
    item: &Item,
    depth: usize,
    members: &mut usize,
    path: &Path,
) -> Result<(), ConfigError> {
    if depth > Limits::CONFIG.max_depth || *members > Limits::CONFIG.max_members {
        return Err(ConfigError::CollectionLimit { path: path.into() });
    }
    match item {
        Item::Table(table) => {
            for (_, child) in table.iter() {
                *members += 1;
                check_bounds(child, depth + 1, members, path)?;
            }
        }
        Item::ArrayOfTables(tables) => {
            for table in tables.iter() {
                *members += 1;
                check_bounds(&Item::Table(table.clone()), depth + 1, members, path)?;
            }
        }
        Item::Value(value) => check_value(value, depth, members, path)?,
        Item::None => {}
    }
    Ok(())
}
fn check_value(
    value: &Value,
    depth: usize,
    members: &mut usize,
    path: &Path,
) -> Result<(), ConfigError> {
    if depth > Limits::CONFIG.max_depth || *members > Limits::CONFIG.max_members {
        return Err(ConfigError::CollectionLimit { path: path.into() });
    }
    match value {
        Value::Array(array) => {
            for child in array.iter() {
                *members += 1;
                check_value(child, depth + 1, members, path)?;
            }
        }
        Value::InlineTable(table) => {
            for (_, child) in table.iter() {
                *members += 1;
                check_value(child, depth + 1, members, path)?;
            }
        }
        // Configuration has exact integer units. Reject all floats/datetimes,
        // including inside a field a permissive downstream type might ignore.
        Value::Float(_) | Value::Datetime(_) => return Err(schema(path, value.span())),
        _ => {}
    }
    Ok(())
}
fn merge(base: &mut Table, user: Table) {
    for (key, value) in user {
        if let (Some(base_table), Some(user_table)) = (
            base.get_mut(&key).and_then(Item::as_table_mut),
            value.as_table(),
        ) {
            merge(base_table, user_table.clone());
        } else {
            base.insert(&key, value);
        }
    }
}
fn record_origins(
    table: &Table,
    prefix: &str,
    origin: &Origin,
    origins: &mut BTreeMap<String, Origin>,
) {
    for (key, value) in table.iter() {
        let field = format!("{prefix}.{key}");
        if let Some(child) = value.as_table() {
            record_origins(child, &field, origin, origins);
        } else {
            origins.insert(field, origin.clone());
        }
    }
}

// Inline tables and table headers are equivalent TOML representations. Convert
// them before merging so both preserve defaults and report leaf origins.
fn normalise_tables(table: &mut Table) {
    for (_, item) in table.iter_mut() {
        if let Ok(mut child) = item.clone().into_table() {
            normalise_tables(&mut child);
            *item = Item::Table(child);
        }
    }
}
