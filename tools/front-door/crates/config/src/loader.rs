use crate::{
    ConfigCheck, ConfigError, ConfigInspection, DomainSections, Finding, Location, Origin, Profile,
    ResolvedConfig, SectionError, domains::NO_DOMAINS,
};
use provenant_core::Roots;
use provenant_records::Limits;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, ffi::OsStr, io::Read, ops::Range, path::Path};
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
    offset: Option<usize>,
}
#[derive(Clone)]
struct Issue {
    location: Location,
    rule: &'static str,
    source: Option<SectionError>,
    rejected: String,
}
impl Issue {
    fn error(self) -> ConfigError {
        match self.source {
            Some(source) => ConfigError::Domain {
                location: self.location,
                source: Box::new(source),
                rejected: Box::new(self.rejected),
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
    error: Option<ConfigError>,
}

/// Invalid optional settings warn and select defaults. The reader, when present, must
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
    for warning in &inspection.snapshot.warnings {
        if first_warning(roots.state.as_path(), text.as_deref(), warning) {
            warn(&roots.state.as_path().join("config.toml"), warning);
        }
    }
    if let Some(error) = inspection.error {
        return Err(error);
    }
    Ok(inspection.snapshot)
}

/// No-follow, read-only boundary. Links, directories and I/O failures are not
/// absence. Neither a missing root nor a missing file creates any state.
pub fn read(roots: &Roots) -> Result<ResolvedConfig, ConfigError> {
    read_with_domains(roots, &NO_DOMAINS)
}
pub fn read_with_domains<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    load_with_domains(roots, open(roots)?, sections)
}
/// Read a snapshot without printing its advisory findings; errors remain errors.
pub fn config_show<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    sections: &DomainSections<T>,
) -> Result<ResolvedConfig<T>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let text = open(roots)?.map(|r| bounded_text(r, &path)).transpose()?;
    let inspection = inspect(text.as_deref(), sections)?;
    if let Some(error) = inspection.error {
        return Err(error);
    }
    Ok(inspection.snapshot)
}

// A separate marker for each finding ensures different domain readers do not
// consume each other's warnings. Creation is atomic across concurrent commands.
fn first_warning(state: &Path, text: Option<&str>, warning: &Finding) -> bool {
    // The command edge reads settings for usage attribution before dispatching
    // the hook. Suppress those advisories as well as the hook's own reads.
    if is_pre_tool_hook(std::env::args_os().skip(1)) {
        return false;
    }
    let text = text.unwrap_or_default().as_bytes();
    let mut bytes = (text.len() as u64).to_be_bytes().to_vec();
    bytes.extend_from_slice(text);
    bytes.extend_from_slice(warning.message.as_bytes());
    let digest = provenant_records::CanonicalV1::file_digest(&bytes);
    let name = format!("config-warning-{digest}");
    let Ok(dir) = provenant_os::fs::Directory::open_anchor(state) else {
        return true;
    };
    match dir.create_new(OsStr::new(&name)) {
        Ok(_) => true,
        Err(error) if error.is_exists() => false,
        Err(_) => true,
    }
}

fn is_pre_tool_hook(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    let mut words = args.into_iter();
    words.any(|word| word == "hook")
        && words.any(|word| word == "claude")
        && words.any(|word| word == "pre-tool-use")
}

/// Validate a proposed edit strictly through the owning domain validators.
/// Reads stay tolerant and recover invalid optional values; a write refuses
/// instead of publishing a document that the effective configuration repairs.
pub(crate) fn validate_edit<T: DeserializeOwned + Serialize>(
    text: &str,
    sections: &DomainSections<T>,
) -> Result<(), ConfigError> {
    let origin = Origin::User { path: user_path()? };
    let root = parse(text, &origin, "")?;
    let sites = sites(&root, text, &origin, "");
    let profile = root
        .get("profile")
        .and_then(Item::as_str)
        .and_then(Profile::from_name)
        .unwrap_or_default();
    let mut domains = root
        .get("domains")
        .and_then(Item::as_table)
        .cloned()
        .unwrap_or_default();
    normalise_tables(&mut domains);
    for entry in sections.entries {
        let Some(user) = domains.get(entry.name).and_then(Item::as_table) else {
            continue;
        };
        // Complete the owner's compiled defaults before validating, so a
        // partial patch is checked as the effective section it would become.
        let mut table = parse((entry.defaults)(profile), &origin, "")?;
        normalise_tables(&mut table);
        merge_table(&mut table, user);
        if let Some(error) = validate_entry(entry, &table).into_iter().next() {
            return Err(domain_issue(entry.name, error, &table, &sites, &origin).error());
        }
    }
    Ok(())
}

fn validate_entry(entry: &crate::DomainSection, table: &Table) -> Vec<SectionError> {
    if entry.name == "fabric"
        && table
            .get("limits")
            .and_then(Item::as_table_like)
            .is_some_and(|limits| limits.contains_key("bundle_bytes"))
    {
        return vec![SectionError {
            field: "limits.bundle_bytes".into(),
            code: "retired.bundle_bytes",
        }];
    }
    (entry.validate)(table)
}

fn merge_table(target: &mut dyn toml_edit::TableLike, source: &dyn toml_edit::TableLike) {
    for (key, item) in source.iter() {
        let nested = match (
            target.get_mut(key).and_then(Item::as_table_like_mut),
            item.as_table_like(),
        ) {
            (Some(target), Some(source)) => {
                merge_table(target, source);
                true
            }
            _ => false,
        };
        if !nested {
            target.insert(key, item.clone());
        }
    }
}

/// Inspect every finding within the existing config bounds. The digest binds
/// exact file bytes and presence, including edits that preserve every finding.
pub fn inspect_config<T: DeserializeOwned + Serialize>(
    roots: &Roots,
    sections: &DomainSections<T>,
) -> Result<ConfigInspection, ConfigError> {
    let text = open(roots)?
        .map(|r| bounded_text(r, &roots.state.as_path().join("config.toml")))
        .transpose()?;
    let mut evidence = crate::inspection::InspectionEvidence::default();
    evidence.observe(
        b"config",
        Path::new("config.toml"),
        text.as_ref().map(String::as_bytes),
    );
    let input_digest = evidence.finish();
    let inspection = match inspect(text.as_deref(), sections) {
        Ok(inspection) => inspection,
        Err(
            error @ (ConfigError::Syntax { .. }
            | ConfigError::MissingVersion { .. }
            | ConfigError::UnsupportedVersion { .. }
            | ConfigError::Schema { .. }
            | ConfigError::CollectionLimit { .. }
            | ConfigError::Domain { .. }),
        ) => {
            return Ok(ConfigInspection {
                check: ConfigCheck {
                    schema_version: crate::CONFIG_SCHEMA_VERSION,
                    profile: Profile::Balanced,
                    findings: Vec::new(),
                    count: 0,
                },
                error: Some(error),
                input_digest,
            });
        }
        Err(error) => return Err(error),
    };
    let findings = inspection.snapshot.warnings;
    Ok(ConfigInspection {
        error: inspection.error,
        check: ConfigCheck {
            schema_version: inspection.snapshot.schema_version,
            profile: inspection.snapshot.profile,
            count: findings.len(),
            findings,
        },
        input_digest,
    })
}
pub(crate) fn open(roots: &Roots) -> Result<Option<provenant_os::fs::FileHandle>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    let dir = match provenant_os::fs::Directory::open_anchor(roots.state.as_path()) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(None),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    match dir.open_read(OsStr::new("config.toml")) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.is_not_found() => Ok(None),
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
    normalise_tables(&mut user);
    let mut issues = Vec::new();
    let mut install_item = user.remove("install");
    let install = recover_settings::<crate::InstallSettings>(
        &mut install_item,
        "install",
        &user_sites,
        &user_origin,
        &mut issues,
        |value| value.validate().map_err(|_| "absolute_path"),
    );
    let mut gc_item = user.remove("gc");
    let gc = recover_settings::<crate::GcSettings>(
        &mut gc_item,
        "gc",
        &user_sites,
        &user_origin,
        &mut issues,
        crate::GcSettings::validate,
    );
    let mut retention_item = user.remove("retention");
    let retention = recover_settings::<crate::RetentionSettings>(
        &mut retention_item,
        "retention",
        &user_sites,
        &user_origin,
        &mut issues,
        crate::RetentionSettings::validate,
    );
    let mut providers_item = user.remove("providers");
    let mut providers = crate::providers::defaults();
    if let Some(item) = &mut providers_item {
        if let Some(table) = item.as_table_mut() {
            let keys: Vec<_> = table.iter().map(|(key, _)| key.to_owned()).collect();
            for key in keys {
                let mut row = table.remove(&key);
                let prefix = format!("providers.{key}");
                if provenant_records::AdapterSpec::get(&key).is_none() {
                    issues.push(Issue {
                        location: site(&user_sites, &prefix, &user_origin, false),
                        rule: "provider_id",
                        source: None,
                        rejected: String::new(),
                    });
                    continue;
                }
                let settings = recover_settings::<crate::ProviderSettings>(
                    &mut row,
                    &prefix,
                    &user_sites,
                    &user_origin,
                    &mut issues,
                    |value| {
                        crate::providers::validate(&BTreeMap::from([(key.clone(), value.clone())]))
                    },
                );
                providers.insert(key.clone(), settings);
                if let Some(row) = row {
                    table.insert(&key, row);
                }
            }
        } else {
            issues.push(Issue {
                location: site(&user_sites, "providers", &user_origin, false),
                rule: "type",
                source: None,
                rejected: String::new(),
            });
            providers_item = None;
        }
    }

    let knobs_item = user.remove("knobs");
    let version = user.get("schema_version");
    if text.is_some() {
        let location = site(&user_sites, "schema_version", &user_origin, false);
        match version {
            None => return Err(ConfigError::MissingVersion { location }),
            Some(item)
                if item
                    .as_integer()
                    .is_some_and(|v| v != crate::CONFIG_SCHEMA_VERSION as i64) =>
            {
                return Err(ConfigError::UnsupportedVersion { location });
            }
            Some(item) if item.as_integer().is_none() => {
                return Err(ConfigError::Schema {
                    location,
                    rule: "type",
                });
            }
            _ => {}
        }
    }
    // Inspect this single scalar without decoding the header or any domain.
    let profile = user
        .get("profile")
        .and_then(Item::as_str)
        .and_then(Profile::from_name)
        .unwrap_or_default();
    let mut valid_knobs = knobs_item.clone().and_then(|item| item.into_table().ok());
    let mut knob_boundary = None;
    // The knobs table holds capability and authority knobs, so a malformed
    // container refuses like a malformed strict knob.
    if knobs_item.is_some() && valid_knobs.is_none() {
        knob_boundary = Some(ConfigError::Schema {
            location: site(&user_sites, "knobs", &user_origin, false),
            rule: "type",
        });
    }
    let knobs = loop {
        match crate::knobs::resolve(
            profile,
            valid_knobs
                .as_ref()
                .map(|table| table as &dyn toml_edit::TableLike),
        ) {
            Ok(knobs) => break knobs,
            Err((key, rule)) if key.starts_with("knobs.") => {
                let location = site(&user_sites, &key, &user_origin, rule == "unknown_key");
                // An invalid capability or authority knob never recovers to
                // its default, which could grant what the user withheld.
                if crate::knobs::strict(key.trim_start_matches("knobs.")) {
                    knob_boundary.get_or_insert(ConfigError::Schema { location, rule });
                } else {
                    issues.push(Issue {
                        location,
                        rule,
                        source: None,
                        rejected: String::new(),
                    });
                }
                if let Some(table) = &mut valid_knobs {
                    table.remove(key.trim_start_matches("knobs."));
                }
            }
            Err(_) => return Err(ConfigError::KnobRegistry),
        }
    };
    let compiled = Origin::Compiled { profile };
    let header = format!(
        "schema_version={}\nprofile='{}'",
        crate::CONFIG_SCHEMA_VERSION,
        profile.as_str()
    );
    let mut defaults = parse(&header, &compiled, "")?;
    let mut compiled_sites = sites(&defaults, &header, &compiled, "");
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
        if let Some(error) = validate_entry(entry, &table).into_iter().next() {
            return Err(
                domain_issue(entry.name, error, &table, &compiled_sites, &compiled).error(),
            );
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
    for key in [
        "gc.schedule",
        "gc.idle_days",
        "gc.free_floor",
        "gc.alert_below",
        "gc.projects",
    ] {
        origins.insert(key.into(), compiled.clone());
    }
    if let Some(item) = &gc_item {
        walk_item(item, "gc", None, &mut |key, _, _| {
            origins.insert(key.into(), user_origin.clone());
        });
    }
    for project in gc.projects.keys() {
        origins
            .entry(format!("gc.projects.{project}.foreign_worktrees"))
            .or_insert_with(|| compiled.clone());
    }
    origins.insert("install.targets".into(), compiled.clone());
    origins.insert("retention.provider_state_days".into(), compiled.clone());
    origins.insert("retention.logs_days".into(), compiled.clone());
    origins.insert("retention.session_revisions".into(), compiled.clone());
    origins.insert("retention.projects".into(), compiled.clone());
    origins.insert("retention.chair_log_days".into(), compiled.clone());
    origins.insert("retention.chair_log_replay_days".into(), compiled.clone());
    origins.insert("retention.chair_log_entries".into(), compiled.clone());
    origins.insert("retention.chair_log_bytes".into(), compiled.clone());
    for provider in providers.keys() {
        for field in [
            "update",
            "update_interval_seconds",
            "update_timeout_seconds",
            "runtime_deadline_seconds",
        ] {
            origins.insert(format!("providers.{provider}.{field}"), compiled.clone());
        }
    }
    if let Some(item) = &providers_item {
        walk_item(item, "providers", None, &mut |key, _, _| {
            origins.insert(key.into(), user_origin.clone());
        });
    }
    for project in retention.projects.keys() {
        origins.insert(
            format!("retention.projects.{project}.checkpoint_soft_bytes"),
            compiled.clone(),
        );
    }
    if let Some(item) = &retention_item {
        walk_item(item, "retention", None, &mut |key, _, _| {
            origins.insert(key.into(), user_origin.clone());
        });
    }
    if let Some(item) = &install_item {
        walk_item(item, "install", None, &mut |key, _, _| {
            origins.insert(key.into(), user_origin.clone());
        });
    }
    let mut effective = defaults.clone();
    let mut semantic_sites = compiled_sites.clone();
    Overrides {
        defaults: &defaults,
        user: &user,
        sites: &user_sites,
        origin: &user_origin,
        origins: &mut origins,
        issues: &mut issues,
        compiled_sites: &compiled_sites,
        semantic_sites: &mut semantic_sites,
    }
    .apply::<T>(&mut effective, &user, &mut Vec::new());
    let mut boundary_error = knob_boundary;
    for issue in &issues {
        if boundary_error.is_none() && boundary_issue(issue, sections) {
            boundary_error = Some(issue.clone().error());
            break;
        }
    }
    let mut domain_table = effective
        .get("domains")
        .and_then(Item::as_table)
        .cloned()
        .unwrap_or_default();
    for entry in sections.entries {
        if let Some(table) = domain_table
            .get_mut(entry.name)
            .and_then(Item::as_table_mut)
        {
            let errors = validate_entry(entry, table);
            if let Some(error) = errors.iter().find(|error| (entry.boundary)(error))
                && boundary_error.is_none()
            {
                boundary_error = Some(
                    domain_issue(
                        entry.name,
                        error.clone(),
                        table,
                        &semantic_sites,
                        &user_origin,
                    )
                    .error(),
                );
            }
            for error in &errors {
                let key = qualify(entry.name, &error.field);
                let origin = origins.get(&key).unwrap_or(&compiled);
                issues.push(domain_issue(
                    entry.name,
                    error.clone(),
                    table,
                    &semantic_sites,
                    origin,
                ));
            }
            for advisory in (entry.advisories)(table) {
                let key = qualify(entry.name, &advisory.field);
                let origin = origins.get(&key).unwrap_or(&compiled);
                issues.push(domain_issue(
                    entry.name,
                    advisory,
                    table,
                    &semantic_sites,
                    origin,
                ));
            }
            let baseline = defaults
                .get("domains")
                .and_then(|item| item.get(entry.name))
                .and_then(Item::as_table);
            let prefix = format!("domains.{}", entry.name);
            let mut repairs: Vec<_> = errors
                .iter()
                .map(|error| {
                    let path: Vec<String> = error.field.split('.').map(String::from).collect();
                    let mut candidate = Item::Table(table.clone());
                    if remove_item_path(&mut candidate, &path)
                        && candidate.as_table().is_some_and(|table| {
                            !validate_entry(entry, table)
                                .iter()
                                .any(|error| error.code == "schema")
                        })
                    {
                        path
                    } else {
                        semantic_repair_path(table, baseline, &path)
                    }
                })
                .collect();
            repairs.sort_by(|a, b| {
                b.iter()
                    .map(|part| (part.parse::<usize>().unwrap_or(0), part))
                    .cmp(
                        a.iter()
                            .map(|part| (part.parse::<usize>().unwrap_or(0), part)),
                    )
            });
            repairs.dedup();
            for path in repairs {
                repair_semantic(table, baseline, &path, &prefix, &mut origins, &compiled);
            }
            if !validate_entry(entry, table).is_empty() {
                *table = defaults["domains"][entry.name]
                    .as_table()
                    .cloned()
                    .unwrap_or_default();
                replace_origins(
                    &prefix,
                    Some(&Item::Table(table.clone())),
                    &mut origins,
                    &compiled,
                );
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
    let domains: T = decode(domain_table).map_err(|_| ConfigError::Schema {
        location: location("", 0, &compiled, "domains"),
        rule: "registration",
    })?;
    for (prefix, value) in [
        ("install", serde_json::to_value(&install)),
        ("gc", serde_json::to_value(&gc)),
        ("retention", serde_json::to_value(&retention)),
        ("providers", serde_json::to_value(&providers)),
        ("domains", serde_json::to_value(&domains)),
    ] {
        let value = value.map_err(|_| ConfigError::Schema {
            location: location("", 0, &compiled, prefix),
            rule: "registration",
        })?;
        seed_origins(&value, prefix, &mut origins, &compiled);
    }
    Ok(Inspection {
        error: boundary_error,
        snapshot: ResolvedConfig {
            schema_version: header.schema_version,
            profile: header.profile,
            domains,
            install,
            gc,
            retention,
            providers,
            knobs,
            origins,
            warnings: issues
                .iter()
                .filter(|issue| !boundary_issue(issue, sections))
                .map(issue_finding)
                .collect::<Result<_, _>>()?,
        },
    })
}
fn boundary_issue<T>(issue: &Issue, sections: &DomainSections<T>) -> bool {
    if issue.rule == "unknown_key" {
        return false;
    }
    sections.entries.iter().any(|entry| {
        let prefix = format!("domains.{}", entry.name);
        // A malformed `domains` container is malformed input for every section.
        let field = if issue.location.key_path == prefix || issue.location.key_path == "domains" {
            Some("")
        } else {
            issue.location.key_path.strip_prefix(&format!("{prefix}."))
        };
        field.is_some_and(|field| {
            (entry.boundary)(&SectionError {
                field: field.to_owned(),
                code: issue
                    .source
                    .as_ref()
                    .map_or(issue.rule, |source| source.code),
            })
        })
    })
}
fn seed_origins(
    value: &serde_json::Value,
    prefix: &str,
    origins: &mut BTreeMap<String, Origin>,
    compiled: &Origin,
) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                seed_origins(value, &join(prefix, key), origins, compiled);
            }
        }
        serde_json::Value::Array(array) if !array.is_empty() => {
            for (index, value) in array.iter().enumerate() {
                seed_origins(value, &join(prefix, &index.to_string()), origins, compiled);
            }
        }
        _ => {
            origins
                .entry(prefix.to_owned())
                .or_insert_with(|| compiled.clone());
        }
    }
}
fn warn(path: &Path, warning: &Finding) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: {}:{}: {}",
        path.display(),
        warning.line,
        warning.message
    );
}

/// Shared bounded reader for setting owners that cannot link the aggregate.
pub(crate) fn read_settings<T: DeserializeOwned + Default>(
    state: &Path,
    prefix: &str,
    validate: impl Fn(&T) -> Result<(), &'static str>,
) -> std::io::Result<T> {
    read_section(state, prefix, validate, false)
}
pub(crate) fn read_boundary_settings<T: DeserializeOwned + Default>(
    state: &Path,
    prefix: &str,
) -> std::io::Result<T> {
    read_section(state, prefix, |_| Ok(()), true)
}
fn read_section<T: DeserializeOwned + Default>(
    state: &Path,
    prefix: &str,
    validate: impl Fn(&T) -> Result<(), &'static str>,
    strict_types: bool,
) -> std::io::Result<T> {
    let path = state.join("config.toml");
    let directory = match provenant_os::fs::Directory::open_anchor(state) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(T::default()),
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let reader = match directory.open_read(OsStr::new("config.toml")) {
        Ok(reader) => reader,
        Err(error) if error.is_not_found() => return Ok(T::default()),
        Err(error) => return Err(std::io::Error::other(error)),
    };
    let text = bounded_text(reader, &path).map_err(std::io::Error::other)?;
    let origin = Origin::User {
        path: user_path().map_err(std::io::Error::other)?,
    };
    let mut root = parse(&text, &origin, "").map_err(std::io::Error::other)?;
    match root.get("schema_version").and_then(Item::as_integer) {
        Some(version) if version == crate::CONFIG_SCHEMA_VERSION as i64 => {}
        _ => {
            return Err(std::io::Error::other(
                "unsupported or missing config schema_version",
            ));
        }
    }
    let sites = sites(&root, &text, &origin, "");
    normalise_tables(&mut root);
    // A malformed ancestor controls every section beneath it, so a boundary
    // section refuses as the aggregate reader does.
    if strict_types && let Some(ancestor) = non_table_ancestor(&root, prefix) {
        return Err(std::io::Error::other(format!(
            "{}: {ancestor}: expected a table",
            path.display()
        )));
    }
    let mut item = prefix
        .split('.')
        .try_fold(&root, |table, key| table.get(key).and_then(Item::as_table))
        .map(|table| Item::Table(table.clone()));
    // Preserve a scalar at the requested key so it produces a type warning.
    if item.is_none() {
        let mut parts: Vec<_> = prefix.split('.').collect();
        if let Some(last) = parts.pop() {
            item = parts
                .iter()
                .try_fold(&root, |table, key| table.get(key).and_then(Item::as_table))
                .and_then(|table| table.get(last))
                .cloned();
        }
    }
    if strict_types {
        let mut item = item.unwrap_or_else(|| Item::Table(Table::new()));
        loop {
            let table = item
                .clone()
                .into_table()
                .map_err(|_| std::io::Error::other("invalid domain table"))?;
            match decode::<T>(table) {
                Ok(value) => return Ok(value),
                Err(error) if error.message().starts_with("unknown field ") => {
                    let offset = error.span().map_or(0, |span| span.start);
                    let key = remove_unknown(&mut item, offset)
                        .ok_or_else(|| std::io::Error::other("invalid domain key"))?;
                    let issue = Issue {
                        location: site(&sites, &join(prefix, &key.join(".")), &origin, true),
                        rule: "unknown_key",
                        source: None,
                        rejected: String::new(),
                    };
                    let finding = issue_finding(&issue).map_err(std::io::Error::other)?;
                    if first_warning(state, Some(&text), &finding) {
                        warn(&path, &finding);
                    }
                }
                Err(_) => return Err(std::io::Error::other("invalid domain value")),
            }
        }
    }
    let mut issues = Vec::new();
    let value = recover_settings(&mut item, prefix, &sites, &origin, &mut issues, validate);
    for issue in &issues {
        let finding = issue_finding(issue).map_err(std::io::Error::other)?;
        if first_warning(state, Some(&text), &finding) {
            warn(&path, &finding);
        }
    }
    Ok(value)
}

/// The first ancestor of `prefix` that exists but is not a table, if any.
fn non_table_ancestor(root: &Table, prefix: &str) -> Option<String> {
    let parts: Vec<&str> = prefix.split('.').collect();
    let mut table = root;
    for (index, key) in parts[..parts.len() - 1].iter().enumerate() {
        match table.get(key) {
            Some(Item::Table(child)) => table = child,
            Some(_) => return Some(parts[..=index].join(".")),
            None => return None,
        }
    }
    None
}

// Keep schemas closed, repairing only user-owned optional input.
fn recover_settings<T: DeserializeOwned + Default>(
    item: &mut Option<Item>,
    prefix: &str,
    sites: &BTreeMap<String, Site>,
    origin: &Origin,
    issues: &mut Vec<Issue>,
    validate: impl Fn(&T) -> Result<(), &'static str>,
) -> T {
    let mut table = match item.as_ref().and_then(table) {
        Some(table) => table,
        None => {
            if item.is_some() {
                issues.push(Issue {
                    location: site(sites, prefix, origin, false),
                    rule: "type",
                    source: None,
                    rejected: String::new(),
                });
                *item = None;
            }
            return T::default();
        }
    };
    loop {
        match decode::<T>(table.clone()) {
            Ok(value) => match validate(&value) {
                Ok(()) => {
                    *item = Some(Item::Table(table));
                    return value;
                }
                Err(rule) => {
                    let mut bad = Vec::new();
                    invalid_settings::<T>(&table, &[], &validate, &mut bad);
                    if bad.is_empty() {
                        issues.push(Issue {
                            location: site(sites, prefix, origin, false),
                            rule,
                            source: None,
                            rejected: String::new(),
                        });
                        *item = None;
                        return T::default();
                    }
                    for (key, rule) in bad {
                        issues.push(Issue {
                            location: site(sites, &join(prefix, &key.join(".")), origin, false),
                            rule,
                            source: None,
                            rejected: String::new(),
                        });
                        remove_path(&mut table, &key);
                    }
                }
            },
            Err(error) => {
                let mut key = error
                    .span()
                    .map(|span| key_at(&table, span.start))
                    .unwrap_or_default();
                let unknown = error.message().starts_with("unknown field ");
                if key.is_empty() {
                    key = table
                        .iter()
                        .next()
                        .map(|(key, _)| key.to_owned())
                        .unwrap_or_default();
                }
                let rule = if unknown { "unknown_key" } else { "type" };
                issues.push(Issue {
                    location: site(sites, &join(prefix, &key), origin, unknown),
                    rule,
                    source: None,
                    rejected: String::new(),
                });
                if key.is_empty() {
                    *item = None;
                    return T::default();
                }
                let path: Vec<_> = key.split('.').map(String::from).collect();
                if !remove_path(&mut table, &path) {
                    let first = table.iter().next().map(|(key, _)| key.to_owned());
                    if let Some(first) = first {
                        table.remove(&first);
                    }
                }
            }
        }
    }
}
fn invalid_settings<T: DeserializeOwned>(
    table: &Table,
    prefix: &[String],
    validate: &impl Fn(&T) -> Result<(), &'static str>,
    output: &mut Vec<(Vec<String>, &'static str)>,
) {
    for (key, item) in table.iter() {
        let mut path = prefix.to_vec();
        path.push(key.to_owned());
        let mut candidate = item.clone();
        for key in path.iter().rev() {
            let mut wrapper = Table::new();
            wrapper.insert(key, candidate);
            candidate = Item::Table(wrapper);
        }
        let Some(rule) = candidate
            .into_table()
            .ok()
            .and_then(|table| decode::<T>(table).ok())
            .and_then(|value| validate(&value).err())
        else {
            continue;
        };
        let before = output.len();
        if let Some(child) = item.as_table() {
            invalid_settings::<T>(child, &path, validate, output);
        }
        if output.len() == before {
            output.push((path, rule));
        }
    }
}
fn remove_path(table: &mut Table, path: &[String]) -> bool {
    for length in (1..=path.len()).rev() {
        let key = path[..length].join(".");
        if length == path.len() {
            if table.remove(&key).is_some() {
                return true;
            }
        } else if let Some(child) = table.get_mut(&key).and_then(Item::as_table_mut)
            && remove_path(child, &path[length..])
        {
            return true;
        }
    }
    false
}
fn replace_origins(
    prefix: &str,
    item: Option<&Item>,
    origins: &mut BTreeMap<String, Origin>,
    origin: &Origin,
) {
    let dotted = format!("{prefix}.");
    origins.retain(|key, _| key != prefix && !key.starts_with(&dotted));
    if let Some(item) = item {
        walk_item(item, prefix, None, &mut |key, _, _| {
            origins.insert(key.to_owned(), origin.clone());
        });
    }
}
/// Split `path` after the longest leading key present in `table`, as removal
/// does, since map keys such as account IDs may contain dots.
fn split_key<'a>(table: &Table, path: &'a [String]) -> Option<(String, &'a [String])> {
    let length = (1..=path.len())
        .rev()
        .find(|length| table.contains_key(&path[..*length].join(".")))
        .unwrap_or(1);
    (!path.is_empty()).then(|| (path[..length].join("."), &path[length..]))
}
fn semantic_repair_path(table: &Table, defaults: Option<&Table>, path: &[String]) -> Vec<String> {
    let Some((first, tail)) = split_key(table, path) else {
        return Vec::new();
    };
    let first = &first;
    let mut result = vec![first.clone()];
    let default = defaults.and_then(|table| table.get(first));
    if default.is_none() {
        return result;
    }
    if let Some(item) = table.get(first) {
        if array(item).is_some() {
            if let Some(index) = tail.first() {
                result.push(index.clone());
            }
        } else if let Some(child) = item.as_table()
            && !tail.is_empty()
        {
            result.extend(semantic_repair_path(
                child,
                default.and_then(Item::as_table),
                tail,
            ));
        }
    }
    result
}
fn repair_semantic(
    table: &mut Table,
    defaults: Option<&Table>,
    path: &[String],
    prefix: &str,
    origins: &mut BTreeMap<String, Origin>,
    compiled: &Origin,
) {
    let Some((first, tail)) = split_key(table, path) else {
        *table = defaults.cloned().unwrap_or_default();
        replace_origins(prefix, Some(&Item::Table(table.clone())), origins, compiled);
        return;
    };
    let first = &first;
    let dotted = join(prefix, first);
    if let Some(item) = table.get_mut(first) {
        // A field inside a row: drop only that field, keeping its siblings.
        if tail.len() > 1
            && tail[0].parse::<usize>().is_ok()
            && array(item).is_some()
            && remove_item_path(item, tail)
        {
            replace_origins(&join(&dotted, &tail.join(".")), None, origins, compiled);
            return;
        }
        if let Some(index) = tail.first().and_then(|key| key.parse::<usize>().ok()) {
            let removed = if let Some(array) = item.as_array_mut() {
                if index < array.len() {
                    array.remove(index);
                    true
                } else {
                    false
                }
            } else if let Some(array) = item.as_array_of_tables_mut() {
                if index < array.len() {
                    array.remove(index);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if removed {
                let old = origins.clone();
                replace_origins(&dotted, None, origins, compiled);
                let array_prefix = format!("{dotted}.");
                for (key, origin) in old {
                    if let Some(rest) = key.strip_prefix(&array_prefix) {
                        let (number, suffix) = rest.split_once('.').unwrap_or((rest, ""));
                        if let Ok(old_index) = number.parse::<usize>() {
                            if old_index == index {
                                continue;
                            }
                            let new_index = old_index - usize::from(old_index > index);
                            let suffix = if suffix.is_empty() {
                                String::new()
                            } else {
                                format!(".{suffix}")
                            };
                            origins.insert(format!("{dotted}.{new_index}{suffix}"), origin);
                        }
                    }
                }
                if array(item).is_some_and(|array| array.is_empty()) {
                    origins.insert(dotted, compiled.clone());
                }
                return;
            }
        }
        if !tail.is_empty()
            && let Some(child) = item.as_table_mut()
        {
            repair_semantic(
                child,
                defaults
                    .and_then(|table| table.get(first))
                    .and_then(Item::as_table),
                tail,
                &dotted,
                origins,
                compiled,
            );
            return;
        }
    }
    let value = defaults.and_then(|table| table.get(first));
    replace_origins(&dotted, value, origins, compiled);
    match value {
        Some(value) => {
            table.insert(first, value.clone());
        }
        None => {
            table.remove(first);
        }
    }
}

fn issue_finding(issue: &Issue) -> Result<Finding, ConfigError> {
    let key: String = issue.location.key_path.chars().take(256).collect();
    Ok(Finding {
        rule: issue.rule.into(),
        path: user_path()?,
        line: issue.location.line,
        message: match &issue.source {
            Some(source)
                if source.code == "unknown_fallback"
                    || source.code == "unsupported_fallback_effort" =>
            {
                format!(
                    "{key}: invalid model fallback ({}); effective fallback checked after overlays",
                    source.code
                )
            }
            Some(source) if crate::is_retired_key_code(source.code) => {
                if source.code.starts_with("retired.") {
                    return Ok(Finding {
                        rule: issue.rule.into(),
                        path: user_path()?,
                        line: issue.location.line,
                        message: format!(
                            "{key}: ignored invalid configuration ({}); run `provenant config migrate`",
                            source.code
                        ),
                    });
                }
                let replacement = source.code.trim_start_matches("moved_to_schedule.");
                format!(
                    "{key}: ignored invalid configuration ({}: {}); replace the retired key with schedule.{replacement}; run `provenant config migrate`",
                    issue.rule, source.code
                )
            }
            Some(source) => format!(
                "{key}: ignored invalid configuration ({}: {})",
                issue.rule, source.code
            ),
            None => format!("{key}: ignored invalid configuration ({})", issue.rule),
        },
    })
}
fn qualify(name: &str, field: &str) -> String {
    if field.is_empty() {
        format!("domains.{name}")
    } else {
        format!("domains.{name}.{field}")
    }
}

fn numeric_rejected(item: Option<&Item>) -> String {
    item.and_then(Item::as_value)
        .and_then(|value| match value {
            Value::Integer(_) | Value::Float(_) => Some(format!(" (rejected value: {value})")),
            _ => None,
        })
        .unwrap_or_default()
}

fn domain_issue(
    name: &str,
    mut error: SectionError,
    table: &Table,
    sites: &BTreeMap<String, Site>,
    origin: &Origin,
) -> Issue {
    // Graph policy bounds are numeric, so they can be shown without exposing
    // arbitrary user strings such as credentials or private paths.
    let rejected = if name == "fabric" && error.field.starts_with("graph.") {
        let mut parts = error.field.split('.');
        let mut item = parts.next().and_then(|key| table.get(key));
        for key in parts {
            item = item.and_then(|value| value.get(key));
        }
        numeric_rejected(item)
    } else {
        String::new()
    };
    error.field = qualify(name, &error.field);
    let location = site(sites, &error.field, origin, false);
    error.field = location.key_path.clone();
    if error.code == "schema" {
        Issue {
            location,
            rule: "type",
            source: None,
            rejected,
        }
    } else {
        Issue {
            location,
            rule: "domain",
            source: Some(error),
            rejected,
        }
    }
}
fn decode<T: DeserializeOwned>(table: Table) -> Result<T, toml_edit::de::Error> {
    toml_edit::de::from_document(DocumentMut::from(table))
}
fn remove_unknown(item: &mut Item, offset: usize) -> Option<Vec<String>> {
    if let Some(table) = item.as_table_mut() {
        let keys: Vec<_> = table.iter().map(|(key, _)| key.to_owned()).collect();
        for key in keys {
            if table
                .key(&key)
                .and_then(|key| key.span())
                .is_some_and(|span| span.contains(&offset))
                || table.get(&key).is_some_and(|item| {
                    item.as_value().is_some_and(|value| {
                        !matches!(value, Value::InlineTable(_) | Value::Array(_))
                    }) && item.span().is_some_and(|span| span.contains(&offset))
                })
            {
                table.remove(&key);
                return Some(vec![key]);
            }
            if let Some(child) = table.get_mut(&key)
                && let Some(mut path) = remove_unknown(child, offset)
            {
                path.insert(0, key);
                return Some(path);
            }
            if table
                .get(&key)
                .and_then(Item::span)
                .is_some_and(|span| span.contains(&offset))
            {
                table.remove(&key);
                return Some(vec![key]);
            }
        }
    } else if let Some(items) = item.as_array_of_tables_mut() {
        for (index, table) in items.iter_mut().enumerate() {
            let mut child = Item::Table(table.clone());
            if let Some(mut path) = remove_unknown(&mut child, offset) {
                *table = child.into_table().ok()?;
                path.insert(0, index.to_string());
                return Some(path);
            }
        }
    } else if let Some(array) = item.as_array_mut() {
        for (index, value) in array.iter_mut().enumerate() {
            let mut child = Item::Value(value.clone());
            if let Some(mut path) = remove_unknown(&mut child, offset) {
                *value = child.into_value().ok()?;
                path.insert(0, index.to_string());
                return Some(path);
            }
        }
    } else if item.as_inline_table().is_some() {
        let mut table = item.clone().into_table().ok()?;
        let mut child = Item::Table(table.clone());
        let path = remove_unknown(&mut child, offset)?;
        table = child.into_table().ok()?;
        *item = Item::Table(table);
        return Some(path);
    }
    None
}
fn remove_array_member_at(item: &mut Item, offset: usize) -> Option<Vec<String>> {
    if let Some(mut items) = array(item) {
        for index in 0..items.len() {
            let mut found = remove_array_member_at(&mut items[index], offset);
            if found.is_none()
                && items[index]
                    .span()
                    .is_some_and(|span| span.contains(&offset))
            {
                items.remove(index);
                found = Some(Vec::new());
            }
            if let Some(mut path) = found {
                let mut values = toml_edit::Array::new();
                for item in items {
                    values.push(item.into_value().ok()?);
                }
                *item = Item::Value(Value::Array(values));
                path.insert(0, index.to_string());
                return Some(path);
            }
        }
    } else if let Some(mut table) = table(item) {
        let found = table.iter_mut().find_map(|(key, child)| {
            let mut path = remove_array_member_at(child, offset)?;
            path.insert(0, key.to_owned());
            Some(path)
        });
        if let Some(path) = found {
            *item = Item::Table(table);
            return Some(path);
        }
    }
    None
}
fn remove_item_path(item: &mut Item, path: &[String]) -> bool {
    // An array of tables keeps its form, so a nested field can be removed
    // without discarding the whole row.
    if let Item::ArrayOfTables(tables) = item {
        let Some(index) = path.first().and_then(|part| part.parse::<usize>().ok()) else {
            return false;
        };
        if index >= tables.len() {
            return false;
        }
        if path.len() == 1 {
            tables.remove(index);
            return true;
        }
        let Some(row) = tables.get_mut(index) else {
            return false;
        };
        let mut child = Item::Table(row.clone());
        if !remove_item_path(&mut child, &path[1..]) {
            return false;
        }
        let Ok(table) = child.into_table() else {
            return false;
        };
        *row = table;
        return true;
    }
    if let Some(items) = array(item) {
        let Some(index) = path.first().and_then(|part| part.parse::<usize>().ok()) else {
            return false;
        };
        let mut items = items;
        if index >= items.len() {
            return false;
        }
        if path.len() == 1 {
            items.remove(index);
        } else if !remove_item_path(&mut items[index], &path[1..]) {
            return false;
        }
        let mut values = toml_edit::Array::new();
        for item in items {
            let Ok(value) = item.into_value() else {
                return false;
            };
            values.push(value);
        }
        *item = Item::Value(Value::Array(values));
        return true;
    }
    let Some(mut table) = table(item) else {
        return false;
    };
    for length in (1..=path.len()).rev() {
        let key = path[..length].join(".");
        let removed = if length == path.len() {
            table.remove(&key).is_some()
        } else {
            table
                .get_mut(&key)
                .is_some_and(|child| remove_item_path(child, &path[length..]))
        };
        if removed {
            *item = Item::Table(table);
            return true;
        }
    }
    false
}
fn decode_probe<T: DeserializeOwned>(table: &Table) -> Result<(), toml_edit::de::Error> {
    let mut header = table.clone();
    let domains = header
        .remove("domains")
        .and_then(|item| item.into_table().ok())
        .unwrap_or_default();
    decode::<Header>(header)?;
    decode::<T>(domains)?;
    Ok(())
}
fn decodes<T: DeserializeOwned>(table: &Table) -> bool {
    decode_probe::<T>(table).is_ok()
}

// Validate against complete defaults while all user spans still identify their
// own input. Independent scalar probes use a valid baseline, so one refusal
// cannot hide another. Only validated overrides enter the effective snapshot.
struct Overrides<'a> {
    defaults: &'a Table,
    user: &'a Table,
    sites: &'a BTreeMap<String, Site>,
    origin: &'a Origin,
    origins: &'a mut BTreeMap<String, Origin>,
    issues: &'a mut Vec<Issue>,
    compiled_sites: &'a BTreeMap<String, Site>,
    semantic_sites: &'a mut BTreeMap<String, Site>,
}
impl Overrides<'_> {
    fn issue(&mut self, path: &[String], rule: &'static str) {
        let graph_policy =
            path.len() >= 4 && path[0] == "domains" && path[1] == "fabric" && path[2] == "graph";
        let retired_key = rule == "retired.bundle_bytes";
        let rejected = if graph_policy {
            numeric_rejected(at(self.user, path))
        } else {
            String::new()
        };
        self.issues.push(Issue {
            location: site(
                self.sites,
                &path.join("."),
                self.origin,
                rule == "unknown_key" || retired_key,
            ),
            rule,
            source: (graph_policy || retired_key).then(|| SectionError {
                field: if retired_key {
                    path[2..].join(".")
                } else {
                    path.join(".")
                },
                code: rule,
            }),
            rejected,
        });
    }
    fn strip_unknown<T: DeserializeOwned>(
        &mut self,
        value: &mut Item,
        path: &[String],
        index: Option<usize>,
    ) {
        loop {
            let Some(candidate) = probe(&Item::Table(self.defaults.clone()), path, value)
                .and_then(|item| item.into_table().ok())
            else {
                return;
            };
            let Err(error) = decode_probe::<T>(&candidate) else {
                return;
            };
            if !error.message().starts_with("unknown field ") {
                return;
            }
            let Some(offset) = error.span().map(|span| span.start) else {
                return;
            };
            let Some(mut relative) = remove_unknown(value, offset) else {
                return;
            };
            if let Some(index) = index
                && !relative.is_empty()
            {
                relative[0] = index.to_string();
            }
            let mut key = path.to_vec();
            key.extend(relative);
            let rule = if key == ["domains", "fabric", "limits", "bundle_bytes"] {
                "retired.bundle_bytes"
            } else {
                "unknown_key"
            };
            self.issue(&key, rule);
        }
    }
    // Removing a field is safe only if serde supplies a default and the whole
    // candidate then decodes. Required fields never acquire invented values.
    fn recover_optional_fields<T: DeserializeOwned>(
        &mut self,
        value: &mut Item,
        path: &[String],
        index: Option<usize>,
    ) {
        let mut projected = value.clone();
        let mut discarded: Vec<Vec<String>> = Vec::new();
        loop {
            let Some(candidate) = probe(&Item::Table(self.defaults.clone()), path, &projected)
                .and_then(|item| item.into_table().ok())
            else {
                return;
            };
            match decode_probe::<T>(&candidate) {
                Ok(()) => {
                    *value = projected;
                    for key in discarded {
                        self.issue(&key, "type");
                    }
                    return;
                }
                Err(error) => {
                    if error.message().starts_with("unknown field ") {
                        return;
                    }
                    let Some(offset) = error.span().map(|span| span.start) else {
                        return;
                    };
                    if error.message().starts_with("missing field ") {
                        if index.is_some() && !discarded.is_empty() {
                            return;
                        }
                        let Some(mut relative) = remove_array_member_at(&mut projected, offset)
                        else {
                            return;
                        };
                        if let Some(index) = index
                            && !relative.is_empty()
                        {
                            relative[0] = index.to_string();
                        }
                        let mut key = path.to_vec();
                        key.extend(relative);
                        discarded.push(key);
                        continue;
                    }
                    let mut repaired = projected.clone();
                    let Some(mut relative) = remove_unknown(&mut repaired, offset) else {
                        return;
                    };
                    if let Some(array_index) = relative
                        .iter()
                        .rposition(|part| part.parse::<usize>().is_ok())
                        && (index.is_none() || array_index > 0)
                    {
                        if !remove_item_path(&mut projected, &relative[..=array_index]) {
                            return;
                        }
                    } else {
                        projected = repaired;
                    }
                    if let Some(index) = index
                        && !relative.is_empty()
                    {
                        relative[0] = index.to_string();
                    }
                    let mut key = path.to_vec();
                    key.extend(relative);
                    discarded.push(key);
                }
            }
        }
    }
    fn apply<T: DeserializeOwned>(
        &mut self,
        effective: &mut Table,
        user: &Table,
        path: &mut Vec<String>,
    ) {
        for (key, value) in user.iter() {
            path.push(key.into());
            let mut cleaned = value.clone();
            if let Some(items) = array(value) {
                let mut values = toml_edit::Array::new();
                for (index, item) in items.into_iter().enumerate() {
                    let mut singleton = toml_edit::Array::new();
                    if let Ok(item) = item.into_value() {
                        singleton.push(item);
                    }
                    let mut row = Item::Value(Value::Array(singleton));
                    self.strip_unknown::<T>(&mut row, path, Some(index));
                    self.recover_optional_fields::<T>(&mut row, path, Some(index));
                    if let Some(item) = row.as_array().and_then(|array| array.get(0)) {
                        values.push(item.clone());
                    }
                }
                cleaned = Item::Value(Value::Array(values));
            } else {
                self.strip_unknown::<T>(&mut cleaned, path, None);
                if at(self.defaults, path).is_none() {
                    self.recover_optional_fields::<T>(&mut cleaned, path, None);
                }
            }
            let value = &cleaned;
            let expected = at(self.defaults, path);
            if expected.is_none() {
                // The aggregate's closed serde types (deny_unknown_fields for
                // structs) distinguish an added map key from a field typo.
                let candidate = probe(&Item::Table(self.defaults.clone()), path, value)
                    .and_then(|item| item.into_table().ok());
                if candidate.as_ref().is_some_and(decodes::<T>) {
                    let dotted = path.join(".");
                    copy_sites(value, &dotted, &dotted, self.sites, self.semantic_sites);
                    set(effective, path, value.clone());
                    walk_item(value, &dotted, None, &mut |key, _, _| {
                        if let Some(site) = self.semantic_sites.get(key) {
                            self.origins.insert(key.into(), site.value.origin.clone());
                        }
                    });
                } else {
                    let error = candidate
                        .as_ref()
                        .and_then(|candidate| decode_probe::<T>(candidate).err());
                    let rule = if error
                        .as_ref()
                        .is_none_or(|error| error.message().starts_with("unknown field "))
                    {
                        "unknown_key"
                    } else {
                        "type"
                    };
                    let precise = error
                        .as_ref()
                        .filter(|error| !error.message().starts_with("missing field "))
                        .and_then(|error| error.span())
                        .map(|span| key_at(self.user, span.start))
                        .filter(|key| key.starts_with(&format!("{}.", path.join("."))));
                    if let Some(key) = precise {
                        self.issue(&key.split('.').map(String::from).collect::<Vec<_>>(), rule);
                    } else if let Some(missing) = error
                        .as_ref()
                        .and_then(|error| error.message().strip_prefix("missing field "))
                        .and_then(|text| text.split(char::from(96)).nth(1))
                    {
                        let mut key = path.clone();
                        key.push(missing.to_owned());
                        self.issue(&key, rule);
                    } else {
                        self.issue(path, rule);
                    }
                    // Keep structural context for owners whose map IDs may
                    // contain dots and resemble optional metadata field names.
                    if error
                        .as_ref()
                        .is_some_and(|error| error.message().contains("expected struct"))
                        && let Some(issue) = self.issues.last_mut()
                        && issue.location.key_path == path.join(".")
                    {
                        issue.source = Some(SectionError {
                            field: issue.location.key_path.clone(),
                            code: "row_type",
                        });
                    }
                }
            } else if let Some(table) = table(value).filter(|_| {
                expected.and_then(table).is_some_and(|table| {
                    !table.is_empty()
                        || !probe(&Item::Table(self.defaults.clone()), path, value)
                            .and_then(|item| item.into_table().ok())
                            .as_ref()
                            .is_some_and(decodes::<T>)
                })
            }) {
                self.apply::<T>(effective, &table, path);
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
            // Empty compiled maps have no exemplar keys. The aggregate's
            // closed serde model decides whether this is a typed map or a
            // closed empty section; unknown fields still fail deserialisation.
            if expected_table.as_ref().is_some_and(Table::is_empty)
                && probe(&Item::Table(self.defaults.clone()), path, user)
                    .and_then(|item| item.into_table().ok())
                    .as_ref()
                    .is_some_and(decodes::<T>)
            {
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
            // An empty default has no member exemplar. Let the aggregate's
            // serde type validate a populated array, as for empty maps.
            if expected_items.as_ref().is_some_and(Vec::is_empty)
                && probe(&Item::Table(self.defaults.clone()), path, user)
                    .and_then(|item| item.into_table().ok())
                    .as_ref()
                    .is_some_and(decodes::<T>)
            {
                return;
            }
            if expected_items.as_ref().is_some_and(Vec::is_empty) {
                for (index, value) in items.iter().enumerate() {
                    let mut singleton = toml_edit::Array::new();
                    if let Ok(value) = value.clone().into_value() {
                        singleton.push(value);
                    }
                    let candidate = probe(
                        &Item::Table(self.defaults.clone()),
                        path,
                        &Item::Value(Value::Array(singleton)),
                    )
                    .and_then(|item| item.into_table().ok());
                    if !candidate.as_ref().is_some_and(decodes::<T>) {
                        path.push(index.to_string());
                        self.issue(path, "type");
                        path.pop();
                    }
                }
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
        if self
            .issues
            .iter()
            .any(|issue| issue.location.key_path == source && issue.rule != "unknown_key")
            && source
                .rsplit('.')
                .next()
                .is_some_and(|part| part.parse::<usize>().is_ok())
        {
            return None;
        }
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
                ) && let Ok(value) = value.into_value()
                {
                    result.push(value);
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
// also supplies the provenance of the recovered effective snapshot.
fn copy_sites(
    item: &Item,
    source: &str,
    target: &str,
    input: &BTreeMap<String, Site>,
    output: &mut BTreeMap<String, Site>,
) {
    walk_item(item, target, None, &mut |key, key_span, value_span| {
        let source_key = format!("{source}{}", key.strip_prefix(target).unwrap_or(""));
        let offset = value_span.or(key_span).map(|span| span.start);
        let direct = input.get(&source_key);
        let site = if offset.is_some() && direct.is_none_or(|site| site.offset != offset) {
            input.values().find(|site| site.offset == offset).or(direct)
        } else {
            direct
        };
        if let Some(site) = site {
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
        let child = if tail.is_empty() && table.get(first).is_none() {
            leaf.clone()
        } else {
            probe(table.get(first)?, tail, leaf)?
        };
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

pub(crate) fn bounded_text(reader: impl Read, path: &Path) -> Result<String, ConfigError> {
    let mut bytes = Vec::new();
    reader
        .take(Limits::CONFIG.max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigError::Read {
            path: path.into(),
            source: provenant_os::fs::FsError::Io {
                operation: "read",
                path: path.into(),
                source,
            },
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
                offset: value_span
                    .as_ref()
                    .or(key_span.as_ref())
                    .map(|span| span.start),
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

#[cfg(test)]
mod warning_tests {
    use super::*;
    use provenant_core::{PathResolver, RootInputs};
    use std::{io, path::PathBuf};

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_owned())
        }
    }

    #[test]
    fn warning_stamp_is_per_revision_and_finding() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut inputs = RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        std::fs::create_dir(inputs.state_dir.as_ref().ok_or("state")?)?;
        let roots = inputs.resolve(&Resolver)?;
        let issue = Issue {
            location: location(
                "",
                0,
                &Origin::Compiled {
                    profile: Profile::Balanced,
                },
                "domains.routing.pacing_overrides.0",
            ),
            rule: "invalid",
            source: Some(SectionError {
                field: String::new(),
                code: "moved_to_schedule.expires",
            }),
            rejected: String::new(),
        };
        let warning = issue_finding(&issue)?;
        assert!(warning.message.contains("schedule.expires"));
        assert!(warning.message.contains("provenant config migrate"));
        assert!(first_warning(
            roots.state.as_path(),
            Some("revision one"),
            &warning
        ));
        assert!(!first_warning(
            roots.state.as_path(),
            Some("revision one"),
            &warning
        ));
        assert!(first_warning(
            roots.state.as_path(),
            Some("revision two"),
            &warning
        ));
        Ok(())
    }

    #[test]
    fn pre_tool_hook_arguments_are_identified() {
        assert!(is_pre_tool_hook(
            ["--json", "hook", "claude", "pre-tool-use"].map(Into::into)
        ));
        assert!(!is_pre_tool_hook(["config", "show"].map(Into::into)));
    }
}
