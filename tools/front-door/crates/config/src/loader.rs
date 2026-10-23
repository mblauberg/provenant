use crate::{
    ConfigCheck, ConfigError, DomainSections, Finding, Location, Origin, Profile, ResolvedConfig,
    SectionError, domains::NO_DOMAINS,
};
use provenant_core::Roots;
use provenant_records::Limits;
use provenant_supervisor::fs::SafeFs;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{self, Read},
    ops::Range,
    path::Path,
};
use toml_edit::{Document, DocumentMut, Item, Table, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u64,
    profile: Profile,
}

#[derive(Clone)]
struct Site {
    value: Location,
    key: Location,
}
#[derive(Clone)]
struct Issue {
    location: Location,
    rule: &'static str,
    source: Option<SectionError>,
}
impl Issue {
    fn error(self) -> ConfigError {
        match self.source {
            Some(source) => ConfigError::Domain {
                location: self.location,
                source,
            },
            None => ConfigError::Schema {
                location: self.location,
                rule: self.rule,
            },
        }
    }
}
struct Inspection<T> {
    snapshot: ResolvedConfig<T>,
    issues: Vec<Issue>,
}

/// Absence alone selects compiled defaults. The reader, when present, must
/// already be opened at `<roots.state>/config.toml`. `None` means absent only.
pub fn load<R: Read>(roots: &Roots, reader: Option<R>) -> Result<ResolvedConfig, ConfigError> {
    load_with_domains(roots, reader, &NO_DOMAINS)
}
pub fn load_with_domains<R: Read, T: DeserializeOwned + Serialize>(
    roots: &Roots,
    reader: Option<R>,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    let text = reader
        .map(|r| bounded_text(r, &roots.state.as_path().join("config.toml")))
        .transpose()?;
    let inspection = inspect(text.as_deref(), sections)?;
    if let Some(issue) = inspection.issues.into_iter().next() {
        return Err(issue.error());
    }
    Ok(inspection.snapshot)
}

/// No-follow, read-only boundary. Links, directories and I/O failures are not
/// absence. Neither a missing root nor a missing file creates any state.
pub fn read(roots: &Roots, fs: &impl SafeFs) -> Result<ResolvedConfig, ConfigError> {
    read_with_domains(roots, fs, &NO_DOMAINS)
}
pub fn read_with_domains<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    fs: &impl SafeFs,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    load_with_domains(roots, open(roots, fs)?, sections)
}
pub fn config_show<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    fs: &impl SafeFs,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    read_with_domains(roots, fs, sections)
}

/// Return at most `limit` ordered findings (1..=100), with the total count.
/// Malformed TOML remains a distinct `Syntax` error, never a finding. Unknown
/// schema versions are refused before decoding any header/domain fields.
pub fn check_config<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    fs: &impl SafeFs,
    sections: &DomainSections<T>,
    limit: usize,
) -> Result<ConfigCheck, ConfigError> {
    if !(1..=100).contains(&limit) {
        return Err(ConfigError::InvalidLimit);
    }
    let text = open(roots, fs)?
        .map(|r| bounded_text(r, &roots.state.as_path().join("config.toml")))
        .transpose()?;
    let inspection = inspect(text.as_deref(), sections)?;
    let count = inspection.issues.len();
    let path = user_path()?;
    let findings = inspection
        .issues
        .into_iter()
        .take(limit)
        .map(|issue| {
            // Bound the message even for a hostile, nearly 1 MiB key. Never include
            // values, parser excerpts or domain-provided prose.
            let key: String = issue.location.key_path.chars().take(256).collect();
            Finding {
                rule: issue.rule.into(),
                path: path.clone(),
                line: issue.location.line,
                message: format!("{key}: invalid configuration ({})", issue.rule),
            }
        })
        .collect();
    Ok(ConfigCheck {
        schema_version: inspection.snapshot.schema_version,
        profile: inspection.snapshot.profile,
        findings,
        count,
        truncated: count > limit,
    })
}
fn open(
    roots: &Roots,
    fs: &impl SafeFs,
) -> Result<Option<Box<dyn provenant_supervisor::fs::SafeFile>>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let dir = match fs.open_anchor(roots.state.as_path()) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    match dir.open_read(OsStr::new("config.toml")) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read { path, source }),
    }
}
fn user_path() -> Result<provenant_records::PathWire, ConfigError> {
    let path = Path::new("config.toml");
    let relative =
        provenant_core::RelativePath::parse(path).map_err(|source| ConfigError::PathEncoding {
            path: path.into(),
            source: source.into(),
        })?;
    provenant_records::PathWire::from_relative(&relative).map_err(|source| {
        ConfigError::PathEncoding {
            path: path.into(),
            source,
        }
    })
}

fn inspect<T: DeserializeOwned + Serialize>(
    text: Option<&str>,
    sections: &DomainSections<T>,
) -> Result<Inspection<T>, ConfigError> {
    let user_origin = Origin::User { path: user_path()? };
    let mut user = text
        .map(|text| parse(text, &user_origin, ""))
        .transpose()?
        .unwrap_or_default();
    let user_sites = sites(&user, text.unwrap_or(""), &user_origin, "");
    let version = user.get("schema_version");
    if text.is_some() {
        let location = site(&user_sites, "schema_version", &user_origin, false);
        match version {
            None => return Err(ConfigError::MissingVersion { location }),
            Some(item) if item.as_integer().is_some_and(|v| v != 1) => {
                return Err(ConfigError::UnsupportedVersion { location });
            }
            _ => {}
        }
    }
    // Inspect this single scalar without decoding the header or any domain.
    let profile = match user.get("profile").and_then(Item::as_str) {
        Some("economy") => Profile::Economy,
        Some("rigorous") => Profile::Rigorous,
        _ => Profile::Balanced,
    };
    let compiled = Origin::Compiled { profile };
    let header = match profile {
        Profile::Balanced => "schema_version=1\nprofile='balanced'",
        Profile::Economy => "schema_version=1\nprofile='economy'",
        Profile::Rigorous => "schema_version=1\nprofile='rigorous'",
    };
    let mut defaults = parse(header, &compiled, "")?;
    let mut compiled_sites = sites(&defaults, header, &compiled, "");
    let mut domains = Table::new();
    for entry in sections.entries {
        let prefix = format!("domains.{}", entry.name);
        if entry.name.is_empty()
            || !entry
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || domains.contains_key(entry.name)
        {
            return Err(ConfigError::Schema {
                location: location("", 0, &compiled, &prefix),
                rule: "registration",
            });
        }
        let asset = (entry.defaults)(profile);
        let mut table = parse(asset, &compiled, &prefix)?;
        compiled_sites.extend(sites(&table, asset, &compiled, &prefix));
        normalise_tables(&mut table);
        // A bad compiled asset is a build defect, not a finding in the user's
        // file. Validate it before any user override can mask the defect.
        if let Some(error) = (entry.validate)(&table).into_iter().next() {
            return Err(domain_issue(entry.name, error, &compiled_sites, &compiled).error());
        }
        domains.insert(entry.name, Item::Table(table));
    }
    defaults.insert("domains", Item::Table(domains));
    normalise_tables(&mut defaults);
    normalise_tables(&mut user);
    // Registration must also match the closed aggregate at composition's edge.
    decode::<T>(
        defaults
            .get("domains")
            .and_then(Item::as_table)
            .cloned()
            .unwrap_or_default(),
    )
    .map_err(|_| ConfigError::Schema {
        location: location("", 0, &compiled, "domains"),
        rule: "registration",
    })?;
    let mut origins = BTreeMap::new();
    walk_table(&defaults, "", &mut |key, _, _| {
        origins.insert(key.into(), compiled.clone());
    });
    let mut effective = defaults.clone();
    let mut issues = Vec::new();
    let mut semantic_sites = compiled_sites.clone();
    Overrides {
        defaults: &defaults,
        sites: &user_sites,
        origin: &user_origin,
        origins: &mut origins,
        issues: &mut issues,
        compiled_sites: &compiled_sites,
        semantic_sites: &mut semantic_sites,
    }
    .apply::<T>(&mut effective, &user, &mut Vec::new());
    let domain_table = effective
        .get("domains")
        .and_then(Item::as_table)
        .cloned()
        .unwrap_or_default();
    for entry in sections.entries {
        if let Some(table) = domain_table.get(entry.name).and_then(Item::as_table) {
            for error in (entry.validate)(table) {
                let key = qualify(entry.name, &error.field);
                let origin = origins.get(&key).unwrap_or(&compiled);
                issues.push(domain_issue(entry.name, error, &semantic_sites, origin));
            }
        }
    }
    issues.sort_by(|a, b| {
        (
            a.location.line,
            a.location.column,
            &a.location.key_path,
            a.rule,
        )
            .cmp(&(
                b.location.line,
                b.location.column,
                &b.location.key_path,
                b.rule,
            ))
    });
    let mut header_table = effective.clone();
    header_table.remove("domains");
    let header: Header = decode(header_table).map_err(|_| ConfigError::Schema {
        location: location("", 0, &compiled, "$"),
        rule: "registration",
    })?;
    let domains = decode(domain_table).map_err(|_| ConfigError::Schema {
        location: location("", 0, &compiled, "domains"),
        rule: "registration",
    })?;
    Ok(Inspection {
        snapshot: ResolvedConfig {
            schema_version: header.schema_version,
            profile: header.profile,
            domains,
            origins,
        },
        issues,
    })
}
fn qualify(name: &str, field: &str) -> String {
    if field.is_empty() {
        format!("domains.{name}")
    } else {
        format!("domains.{name}.{field}")
    }
}
fn domain_issue(
    name: &str,
    mut error: SectionError,
    sites: &BTreeMap<String, Site>,
    origin: &Origin,
) -> Issue {
    error.field = qualify(name, &error.field);
    let location = site(sites, &error.field, origin, false);
    error.field = location.key_path.clone();
    if error.code == "schema" {
        Issue {
            location,
            rule: "type",
            source: None,
        }
    } else {
        Issue {
            location,
            rule: "domain",
            source: Some(error),
        }
    }
}
fn decode<T: DeserializeOwned>(table: Table) -> Result<T, toml_edit::de::Error> {
    toml_edit::de::from_document(DocumentMut::from(table))
}
fn decodes<T: DeserializeOwned>(table: &Table) -> bool {
    let mut header = table.clone();
    let domains = header
        .remove("domains")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    decode::<Header>(header).is_ok() && decode::<T>(domains).is_ok()
}

// Validate against complete defaults while all user spans still identify their
// own input. Independent scalar probes use a valid baseline, so one refusal
// cannot hide another. Only validated overrides enter the effective snapshot.
struct Overrides<'a> {
    defaults: &'a Table,
    sites: &'a BTreeMap<String, Site>,
    origin: &'a Origin,
    origins: &'a mut BTreeMap<String, Origin>,
    issues: &'a mut Vec<Issue>,
    compiled_sites: &'a BTreeMap<String, Site>,
    semantic_sites: &'a mut BTreeMap<String, Site>,
}
impl Overrides<'_> {
    fn issue(&mut self, path: &[String], rule: &'static str) {
        self.issues.push(Issue {
            location: site(
                self.sites,
                &path.join("."),
                self.origin,
                rule == "unknown_key",
            ),
            rule,
            source: None,
        });
    }
    fn apply<T: DeserializeOwned>(
        &mut self,
        effective: &mut Table,
        user: &Table,
        path: &mut Vec<String>,
    ) {
        for (key, value) in user.iter() {
            path.push(key.into());
            let expected = at(self.defaults, path);
            if expected.is_none() {
                self.issue(path, "unknown_key");
            } else if let (Some(expected), Some(table)) = (expected, value.as_table()) {
                if expected.is_table() {
                    self.apply::<T>(effective, table, path);
                } else {
                    self.issue(path, "type");
                }
            } else {
                let before = self.issues.len();
                self.check::<T>(expected, value, path);
                let dotted = path.join(".");
                let projected = if self.issues.len() == before {
                    copy_sites(value, &dotted, &dotted, self.sites, self.semantic_sites);
                    Some(value.clone())
                } else {
                    self.project(expected, value, &dotted, &dotted, &dotted)
                };
                if let Some(projected) = projected {
                    set(effective, path, projected.clone());
                    let prefix = format!("{dotted}.");
                    self.origins
                        .retain(|key, _| key != &dotted && !key.starts_with(&prefix));
                    walk_item(&projected, &dotted, None, &mut |key, _, _| {
                        if let Some(site) = self.semantic_sites.get(key) {
                            self.origins.insert(key.into(), site.value.origin.clone());
                        }
                    });
                }
            }
            path.pop();
        }
    }
    fn check<T: DeserializeOwned>(
        &mut self,
        expected: Option<&Item>,
        user: &Item,
        path: &mut Vec<String>,
    ) {
        if let Some(user_table) = table(user) {
            let expected_table = expected.and_then(table);
            if expected_table.is_none() {
                self.issue(path, "type");
                return;
            }
            let before = self.issues.len();
            for (key, value) in user_table.iter() {
                path.push(key.into());
                let expected = expected_table.as_ref().and_then(|table| table.get(key));
                if expected.is_none() {
                    self.issue(path, "unknown_key");
                } else {
                    self.check::<T>(expected, value, path);
                }
                path.pop();
            }
            if self.issues.len() == before {
                let candidate = probe(&Item::Table(self.defaults.clone()), path, user)
                    .and_then(|item| item.into_table().ok());
                if !candidate.as_ref().is_some_and(decodes::<T>) {
                    self.issue(path, "type");
                }
            }
        } else if let Some(items) = array(user) {
            let expected_items = expected.and_then(array);
            if expected_items.is_none() {
                self.issue(path, "type");
                return;
            }
            for (index, value) in items.iter().enumerate() {
                path.push(index.to_string());
                self.check::<T>(
                    expected_items.as_ref().and_then(|items| items.first()),
                    value,
                    path,
                );
                path.pop();
            }
            if items.is_empty() {
                self.scalar::<T>(expected, user, path);
            }
        } else {
            self.scalar::<T>(expected, user, path);
        }
    }
    fn project(
        &mut self,
        expected: Option<&Item>,
        user: &Item,
        base: &str,
        source: &str,
        target: &str,
    ) -> Option<Item> {
        if let (Some(expected_table), Some(user_table)) = (expected.and_then(table), table(user)) {
            let mut result = Table::new();
            for (key, default) in expected_table.iter() {
                let base = join(base, key);
                let source = join(source, key);
                let target = join(target, key);
                let value = if let Some(value) = user_table.get(key) {
                    self.project(Some(default), value, &base, &source, &target)
                } else {
                    copy_sites(
                        default,
                        &base,
                        &target,
                        self.compiled_sites,
                        self.semantic_sites,
                    );
                    Some(default.clone())
                };
                if let Some(value) = value {
                    result.insert(key, value);
                }
            }
            return Some(Item::Table(result));
        }
        if let (Some(expected_items), Some(user_items)) = (expected.and_then(array), array(user)) {
            let mut result = toml_edit::Array::new();
            for (index, value) in user_items.iter().enumerate() {
                let source = join(source, &index.to_string());
                let target = join(target, &result.len().to_string());
                if let Some(value) = self.project(
                    expected_items.first(),
                    value,
                    &join(base, "0"),
                    &source,
                    &target,
                ) {
                    if let Ok(value) = value.into_value() {
                        result.push(value);
                    }
                }
            }
            return Some(Item::Value(Value::Array(result)));
        }
        if self
            .issues
            .iter()
            .any(|issue| issue.location.key_path == source)
        {
            let expected = expected?;
            copy_sites(
                expected,
                base,
                target,
                self.compiled_sites,
                self.semantic_sites,
            );
            Some(expected.clone())
        } else {
            copy_sites(user, source, target, self.sites, self.semantic_sites);
            Some(user.clone())
        }
    }
    fn scalar<T: DeserializeOwned>(
        &mut self,
        expected: Option<&Item>,
        user: &Item,
        path: &[String],
    ) {
        if forbidden(user) || expected.is_some_and(|expected| !same_kind(expected, user)) {
            self.issue(path, "type");
            return;
        }
        let candidate = probe(&Item::Table(self.defaults.clone()), path, user)
            .and_then(|item| item.into_table().ok());
        if !candidate.as_ref().is_some_and(decodes::<T>) {
            self.issue(
                path,
                if user.is_integer() || user.is_str() {
                    "range"
                } else {
                    "type"
                },
            );
        }
    }
}
// Diagnostics for a typed-valid projection retain original source positions,
// even if an empty-default array has to omit an undecodable member. This map
// is used only while checking a refused file; no projected snapshot is exposed.
fn copy_sites(
    item: &Item,
    source: &str,
    target: &str,
    input: &BTreeMap<String, Site>,
    output: &mut BTreeMap<String, Site>,
) {
    walk_item(item, target, None, &mut |key, _, _| {
        let source_key = format!("{source}{}", key.strip_prefix(target).unwrap_or(""));
        if let Some(site) = input.get(&source_key) {
            output.insert(key.into(), site.clone());
        }
    });
}
fn table(item: &Item) -> Option<Table> {
    item.clone().into_table().ok()
}
fn array(item: &Item) -> Option<Vec<Item>> {
    match item {
        Item::Value(Value::Array(values)) => {
            Some(values.iter().cloned().map(Item::Value).collect())
        }
        Item::ArrayOfTables(tables) => Some(tables.iter().cloned().map(Item::Table).collect()),
        _ => None,
    }
}
fn forbidden(item: &Item) -> bool {
    matches!(item, Item::Value(Value::Float(_) | Value::Datetime(_)))
}
// Build one typed-schema probe. Array indices use the default's representative
// element, then replace the array as a whole with that one probe element. This
// also checks every scalar in an array whose compiled default is empty.
fn probe(base: &Item, path: &[String], leaf: &Item) -> Option<Item> {
    let Some((first, tail)) = path.split_first() else {
        return Some(leaf.clone());
    };
    if let Some(mut table) = table(base) {
        let child = probe(table.get(first)?, tail, leaf)?;
        table.insert(first, child);
        Some(Item::Table(table))
    } else if let Some(items) = array(base) {
        let child = probe(items.first().unwrap_or(leaf), tail, leaf)?;
        let mut values = toml_edit::Array::new();
        values.push(child.into_value().ok()?);
        Some(Item::Value(Value::Array(values)))
    } else {
        None
    }
}
fn at<'a>(table: &'a Table, path: &[String]) -> Option<&'a Item> {
    let (first, tail) = path.split_first()?;
    let value = table.get(first)?;
    if tail.is_empty() {
        Some(value)
    } else {
        at(value.as_table()?, tail)
    }
}
fn set(table: &mut Table, path: &[String], value: Item) {
    if let Some((first, tail)) = path.split_first() {
        if tail.is_empty() {
            table.insert(first, value);
        } else if let Some(child) = table.get_mut(first).and_then(Item::as_table_mut) {
            set(child, tail, value);
        }
    }
}
fn same_kind(a: &Item, b: &Item) -> bool {
    if table(a).is_some() {
        return table(b).is_some();
    }
    if array(a).is_some() {
        return array(b).is_some();
    }
    matches!(
        (a, b),
        (Item::Value(Value::String(_)), Item::Value(Value::String(_)))
            | (
                Item::Value(Value::Integer(_)),
                Item::Value(Value::Integer(_))
            )
            | (
                Item::Value(Value::Boolean(_)),
                Item::Value(Value::Boolean(_))
            )
    )
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
fn parse(text: &str, origin: &Origin, prefix: &str) -> Result<Table, ConfigError> {
    let doc = Document::parse(text).map_err(|e| {
        let offset = e.span().map_or(0, |span| span.start);
        let key = syntax_key(text, offset)
            .map(|key| join(prefix, &key))
            .unwrap_or_else(|| prefix.into());
        ConfigError::Syntax {
            location: location(text, offset, origin, &key),
        }
    })?;
    let mut members = 0;
    check_bounds(doc.as_item(), 0, &mut members, text, origin, prefix)?;
    Ok(doc.as_table().clone())
}
// Recover an assignment path only when toml_edit can parse its unchanged
// prefix with a dummy value. This is diagnostic recovery, never acceptance or
// another TOML lexer. Limit both lines and parser attempts for hostile input;
// syntactically unidentifiable keys retain the explicit root marker `$`.
fn syntax_key(text: &str, offset: usize) -> Option<String> {
    let before = text.get(..offset)?;
    let starts = std::iter::once(before.rfind('\n').map_or(0, |index| index + 1))
        .chain(before.match_indices('\n').rev().map(|(index, _)| index + 1))
        .chain(std::iter::once(0));
    let mut attempts = 0;
    for start in starts.take(64) {
        let line = text.get(start..offset)?;
        for (equal, _) in line.match_indices('=').rev() {
            attempts += 1;
            if attempts > 64 {
                return None;
            }
            let value_start = start + equal + 1;
            let mut candidate = text.get(..value_start)?.to_owned();
            candidate.push('0');
            if let Ok(doc) = Document::parse(&candidate) {
                let key = key_at(doc.as_table(), value_start);
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        // A valid prefix means this line began a new construct. Do not blame
        // an unrelated earlier assignment when this key cannot be recovered.
        if Document::parse(text.get(..start)?).is_ok() {
            return None;
        }
    }
    None
}
fn check_bounds(
    item: &Item,
    depth: usize,
    members: &mut usize,
    text: &str,
    origin: &Origin,
    key: &str,
) -> Result<(), ConfigError> {
    bound(depth, members, item.span(), text, origin, key)?;
    match item {
        Item::Table(table) => {
            for (field, child) in table.iter() {
                *members += 1;
                let key = join(key, field);
                check_bounds(child, depth + 1, members, text, origin, &key)?;
            }
        }
        Item::ArrayOfTables(tables) => {
            for (index, table) in tables.iter().enumerate() {
                *members += 1;
                check_bounds(
                    &Item::Table(table.clone()),
                    depth + 1,
                    members,
                    text,
                    origin,
                    &join(key, &index.to_string()),
                )?;
            }
        }
        Item::Value(value) => check_value(value, depth, members, text, origin, key)?,
        Item::None => {}
    }
    Ok(())
}
fn check_value(
    value: &Value,
    depth: usize,
    members: &mut usize,
    text: &str,
    origin: &Origin,
    key: &str,
) -> Result<(), ConfigError> {
    bound(depth, members, value.span(), text, origin, key)?;
    if matches!(origin, Origin::Compiled { .. })
        && matches!(value, Value::Float(_) | Value::Datetime(_))
    {
        return Err(ConfigError::Schema {
            location: location(text, value.span().map_or(0, |s| s.start), origin, key),
            rule: "type",
        });
    }
    match value {
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                *members += 1;
                check_value(
                    child,
                    depth + 1,
                    members,
                    text,
                    origin,
                    &join(key, &index.to_string()),
                )?;
            }
        }
        Value::InlineTable(table) => {
            for (field, child) in table.iter() {
                *members += 1;
                check_value(child, depth + 1, members, text, origin, &join(key, field))?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn bound(
    depth: usize,
    members: &usize,
    span: Option<Range<usize>>,
    text: &str,
    origin: &Origin,
    key: &str,
) -> Result<(), ConfigError> {
    if depth > Limits::CONFIG.max_depth || *members > Limits::CONFIG.max_members {
        return Err(ConfigError::CollectionLimit {
            location: location(text, span.map_or(0, |s| s.start), origin, key),
        });
    }
    Ok(())
}
fn location(text: &str, offset: usize, origin: &Origin, key: &str) -> Location {
    let prefix = text.get(..offset).unwrap_or("");
    Location {
        origin: origin.clone(),
        key_path: if key.is_empty() {
            "$".into()
        } else {
            key.into()
        },
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    }
}
fn sites(table: &Table, text: &str, origin: &Origin, prefix: &str) -> BTreeMap<String, Site> {
    let mut result = BTreeMap::new();
    walk_table(table, prefix, &mut |key, key_span, value_span| {
        result.insert(
            key.into(),
            Site {
                value: location(
                    text,
                    value_span
                        .clone()
                        .or_else(|| key_span.clone())
                        .map_or(0, |s| s.start),
                    origin,
                    key,
                ),
                key: location(
                    text,
                    key_span.or(value_span).map_or(0, |s| s.start),
                    origin,
                    key,
                ),
            },
        );
    });
    result
}
fn site(sites: &BTreeMap<String, Site>, key: &str, origin: &Origin, use_key: bool) -> Location {
    sites
        .get(key)
        .map(|s| {
            if use_key {
                s.key.clone()
            } else {
                s.value.clone()
            }
        })
        .unwrap_or_else(|| location("", 0, origin, key))
}
pub(crate) fn key_at(table: &Table, offset: usize) -> String {
    let mut result = String::new();
    walk_table(table, "", &mut |key, key_span, value_span| {
        if key_span.is_some_and(|s| s.contains(&offset))
            || value_span.is_some_and(|s| s.contains(&offset))
        {
            result = key.into();
        }
    });
    result
}
fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.into()
    } else {
        format!("{prefix}.{key}")
    }
}
fn walk_table(
    table: &Table,
    prefix: &str,
    visit: &mut impl FnMut(&str, Option<Range<usize>>, Option<Range<usize>>),
) {
    for (key, item) in table.iter() {
        walk_item(
            item,
            &join(prefix, key),
            table.key(key).and_then(|k| k.span()),
            visit,
        );
    }
}
fn walk_item(
    item: &Item,
    key: &str,
    key_span: Option<Range<usize>>,
    visit: &mut impl FnMut(&str, Option<Range<usize>>, Option<Range<usize>>),
) {
    match item {
        Item::Value(value) => walk_value(value, key, key_span, visit),
        Item::Table(table) => {
            visit(key, key_span, item.span());
            walk_table(table, key, visit);
        }
        Item::ArrayOfTables(tables) => {
            visit(key, key_span, item.span());
            for (index, table) in tables.iter().enumerate() {
                let path = join(key, &index.to_string());
                visit(&path, None, table.span());
                walk_table(table, &path, visit);
            }
        }
        Item::None => {}
    }
}
fn walk_value(
    value: &Value,
    key: &str,
    key_span: Option<Range<usize>>,
    visit: &mut impl FnMut(&str, Option<Range<usize>>, Option<Range<usize>>),
) {
    visit(key, key_span, value.span());
    match value {
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                walk_value(child, &join(key, &index.to_string()), None, visit);
            }
        }
        Value::InlineTable(table) => {
            for (field, child) in table.iter() {
                walk_value(
                    child,
                    &join(key, field),
                    table.key(field).and_then(|k| k.span()),
                    visit,
                );
            }
        }
        _ => {}
    }
}
fn normalise_tables(table: &mut Table) {
    for (_, item) in table.iter_mut() {
        if let Ok(mut child) = item.clone().into_table() {
            normalise_tables(&mut child);
            *item = Item::Table(child);
        }
    }
}
