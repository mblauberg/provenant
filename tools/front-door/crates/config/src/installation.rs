//! Managed skills: ownership, retirement, planning and reconciliation.
use crate::{
    invalid, io,
    managed_skills::{self, Catalogues},
    manifest::{self, Manifest},
    ordered::{self},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const LEGACY_AGENT_MANIFEST: &str = ".agent-harness-agents-installation.json";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    ValidateSources,
    PreflightRetirement,
    RetireLegacyAgents,
    CheckLegacyAgy,
    MigrateDirectoryLink,
    Plan,
    Preflight,
    Check,
    Install,
    Reconcile,
    UninstallManaged,
}
impl Action {
    pub const ALL: &'static [(Self, &'static str)] = &[
        (Self::ValidateSources, "validate-sources"),
        (Self::PreflightRetirement, "preflight-retirement"),
        (Self::RetireLegacyAgents, "retire-legacy-agents"),
        (Self::CheckLegacyAgy, "check-legacy-agy"),
        (Self::MigrateDirectoryLink, "migrate-directory-link"),
        (Self::Plan, "plan"),
        (Self::Preflight, "preflight"),
        (Self::Check, "check"),
        (Self::Install, "install"),
        (Self::Reconcile, "reconcile"),
        (Self::UninstallManaged, "uninstall-managed"),
    ];
    pub fn text(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(a, _)| a == &self)
            .map(|(_, s)| *s)
            .unwrap_or("plan")
    }
    pub fn parse(value: &str) -> Result<Self> {
        Self::ALL
            .iter()
            .find(|(_, s)| s == &value)
            .map(|(a, _)| *a)
            .ok_or_else(|| invalid(format!("unsupported action: {value}")))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Missing,
    Unmanaged,
    Managed,
    Stale,
    Conflicting,
    RetiredManaged,
    RetiredMissing,
    CustomToManaged,
    ManagedToCustom,
    CustomUntracked,
    CustomMissing,
    CustomRebind,
    CustomConflicting,
    Custom,
    CustomRetired,
    CustomRetiredMissing,
    MissingSource,
    Present,
    Foreign,
    Noncanonical,
    Dangling,
    Current,
    Retired,
    #[serde(rename = "unreadable receipt")]
    UnreadableReceipt,
}
impl State {
    pub fn text(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unmanaged => "unmanaged",
            Self::Managed => "managed",
            Self::Stale => "stale",
            Self::Conflicting => "conflicting",
            Self::RetiredManaged => "retired-managed",
            Self::RetiredMissing => "retired-missing",
            Self::CustomToManaged => "custom-to-managed",
            Self::ManagedToCustom => "managed-to-custom",
            Self::CustomUntracked => "custom-untracked",
            Self::CustomMissing => "custom-missing",
            Self::CustomRebind => "custom-rebind",
            Self::CustomConflicting => "custom-conflicting",
            Self::Custom => "custom",
            Self::CustomRetired => "custom-retired",
            Self::CustomRetiredMissing => "custom-retired-missing",
            Self::MissingSource => "missing-source",
            Self::Present => "present",
            Self::Foreign => "foreign",
            Self::Noncanonical => "noncanonical",
            Self::Dangling => "dangling",
            Self::Current => "current",
            Self::Retired => "retired",
            Self::UnreadableReceipt => "unreadable receipt",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Owner {
    Managed,
    Custom,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Required,
    Extra,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    PerEntry,
    Converted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<Owner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
}
impl Item {
    fn state(name: impl Into<String>, state: State) -> Self {
        Self {
            name: name.into(),
            state: Some(state),
            owner: None,
            scope: None,
            source_target: None,
            source: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<Layout>,
    pub items: Vec<Item>,
    pub changed: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migrated: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_sources: Option<MigrationSources>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MigrationSources(
    #[serde(
        serialize_with = "ordered::serialize_pairs",
        deserialize_with = "ordered::deserialize_pairs"
    )]
    pub Vec<(String, String)>,
);
impl Report {
    fn new(action: Action) -> Self {
        Self {
            schema_version: 1,
            action,
            ok: None,
            layout: None,
            items: vec![],
            changed: vec![],
            migrated: None,
            migration_sources: None,
        }
    }
}

pub fn same_link(destination: &Path, source: &Path) -> bool {
    destination.is_symlink()
        && crate::resolve(destination)
            .ok()
            .zip(crate::resolve(source).ok())
            .is_some_and(|(a, b)| a == b)
}
fn occupied(path: &Path) -> bool {
    path.exists() || path.is_symlink()
}
fn sorted_children(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(path)
        .map_err(|e| io(path, e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| io(path, e)))
        .collect::<Result<Vec<_>>>()?;
    paths.sort_by_cached_key(|p| crate::pending::path_sort_key(p.as_os_str()));
    Ok(paths)
}

#[derive(Debug, Deserialize)]
struct LegacyReceipt {
    #[serde(
        serialize_with = "ordered::serialize_pairs",
        deserialize_with = "ordered::deserialize_pairs"
    )]
    managed: Vec<(String, LegacyEntry)>,
}
#[derive(Debug, Deserialize)]
struct LegacyEntry {
    source_target: String,
}
fn legacy_receipt(target: &Path) -> Result<Option<(PathBuf, LegacyReceipt)>> {
    let parent = target.parent().unwrap_or(Path::new("."));
    let path = parent.join(LEGACY_AGENT_MANIFEST);
    if !occupied(&path) {
        return Ok(None);
    }
    if path.is_symlink() || !path.is_file() {
        return Err(invalid(
            "legacy agent installation receipt is not a regular file",
        ));
    }
    let bytes = fs::read(&path).map_err(|source| crate::Error::Read {
        context: "legacy agent installation receipt is unreadable",
        path: path.clone(),
        source,
    })?;
    let receipt = ordered::decode(
        &bytes,
        "legacy agent installation receipt is unreadable",
        &path,
    )?;
    let value = receipt.value();
    let legacy_target = parent.join("agents");
    if !value.is_object()
        || !crate::pending::schema_one(&value["schema_version"])
        || value["owner"] != "agent-harness"
        || value["surface"] != "claude-agents"
        || value["target_root"].as_str()
            != Some(crate::pending::path_text(&crate::resolve(&legacy_target)?)?.as_str())
        || !value["managed"].is_object()
    {
        return Err(invalid("legacy agent installation receipt is invalid"));
    }
    for (name, entry) in receipt["managed"].object().unwrap_or(&[]) {
        let valid_name = name
            .strip_suffix(".md")
            .is_some_and(manifest::is_skill_name);
        if !valid_name
            || entry.object().is_none_or(|e| e.len() != 4)
            || !["owner", "source_target", "source_sha256", "installed_at"]
                .iter()
                .all(|k| entry.get(k).is_some())
            || entry["owner"].string() != Some("agent-harness")
            || entry["source_target"]
                .string()
                .is_none_or(|s| !Path::new(s).is_absolute())
            || entry["source_sha256"]
                .string()
                .is_none_or(|s| !crate::workflows::valid_sha(s))
            || entry["installed_at"].string().is_none()
        {
            return Err(invalid(format!(
                "legacy agent installation entry is invalid: {name}"
            )));
        }
    }
    let receipt: LegacyReceipt =
        serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
    for (name, entry) in &receipt.managed {
        let destination = legacy_target.join(name);
        if occupied(&destination) && !same_link(&destination, Path::new(&entry.source_target)) {
            return Err(invalid(format!(
                "conflicting retired managed agent target changed outside harness: {name}"
            )));
        }
    }
    Ok(Some((path, receipt)))
}
pub fn retire_legacy_agents(target: &Path, apply: bool) -> Result<Vec<String>> {
    let Some((path, receipt)) = legacy_receipt(target)? else {
        return Ok(vec![]);
    };
    let mut names = receipt
        .managed
        .into_iter()
        .map(|(n, _)| n)
        .collect::<Vec<_>>();
    names.sort();
    if apply {
        let directory = target.parent().unwrap_or(Path::new(".")).join("agents");
        for name in &names {
            let destination = directory.join(name);
            if destination.is_symlink() {
                fs::remove_file(&destination).map_err(|e| io(&destination, e))?;
            }
        }
        fs::remove_file(&path).map_err(|e| io(&path, e))?;
    }
    Ok(names)
}

pub fn migrate_directory_link(
    source: &Path,
    target: &Path,
    catalogues: &Catalogues,
    now: &str,
) -> Result<Report> {
    let mut report = Report::new(Action::MigrateDirectoryLink);
    if !target.is_symlink() {
        report.layout = Some(Layout::PerEntry);
        return Ok(report);
    }
    if crate::resolve(target)? != source {
        return Err(invalid(format!(
            "managed directory is a non-canonical symlink: {}",
            target.display()
        )));
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    let mut staged = None;
    for _ in 0..128 {
        let name = provenant_supervisor::fs::temporary_name().map_err(|e| io(parent, e.into()))?;
        let path = parent.join(format!(".skills-migrate.{}", name.to_string_lossy()));
        match fs::create_dir(&path) {
            Ok(()) => {
                staged = Some(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&path, e)),
        }
    }
    let staged = staged.ok_or_else(|| invalid("could not reserve skill migration directory"))?;
    let result = (|| {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .map_err(|e| io(&staged, e))?;
        for (name, path) in catalogues.managed.iter().chain(&catalogues.custom) {
            let destination = staged.join(name);
            std::os::unix::fs::symlink(path, &destination).map_err(|e| io(&destination, e))?;
        }
        fs::remove_file(target).map_err(|e| io(target, e))?;
        if let Err(error) = fs::rename(&staged, target) {
            std::os::unix::fs::symlink(source, target).map_err(|e| io(target, e))?;
            return Err(io(target, error));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staged);
    }
    result?;
    let mut receipt = manifest::load_manifest(target, now)?;
    for (name, path) in &catalogues.managed {
        receipt
            .managed
            .insert(name.clone(), manifest::entry(path, vec![], now)?);
        receipt.custom.remove(name);
    }
    for (name, path) in &catalogues.custom {
        receipt.custom.insert(
            name.clone(),
            manifest::CustomLink {
                source_target: crate::pending::path_text(path)?,
            },
        );
        receipt.managed.remove(name);
    }
    manifest::write_manifest(target, &mut receipt, now)?;
    let mut catalogue = catalogues
        .managed
        .iter()
        .chain(&catalogues.custom)
        .collect::<Vec<_>>();
    catalogue.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path) in catalogue {
        report.items.push(Item {
            name: name.clone(),
            owner: Some(if catalogues.custom.iter().any(|(n, _)| n == name) {
                Owner::Custom
            } else {
                Owner::Managed
            }),
            source_target: Some(crate::pending::path_text(path)?),
            scope: None,
            source: None,
            state: None,
        });
        report.changed.push(name.clone());
    }
    report.layout = Some(Layout::Converted);
    Ok(report)
}

pub fn plan(
    catalogues: &Catalogues,
    target: &Path,
    receipt: &Manifest,
    custom_source_provided: bool,
) -> Result<Vec<Item>> {
    use State::*;
    let mut items = vec![];
    for (name, source) in &catalogues.managed {
        if !custom_source_provided && receipt.custom.contains_key(name) {
            continue;
        }
        let destination = target.join(name);
        let state = if let Some(entry) = receipt.managed.get(name) {
            if same_link(&destination, source) {
                if entry.source_sha256 == manifest::sha_skill(source)? {
                    Managed
                } else {
                    Stale
                }
            } else if same_link(&destination, Path::new(&entry.source_target))
                || !destination.exists()
            {
                Stale
            } else {
                Conflicting
            }
        } else if let Some(previous) = receipt
            .custom
            .get(name)
            .filter(|_| !catalogues.custom.iter().any(|(n, _)| n == name))
        {
            if same_link(&destination, Path::new(&previous.source_target)) {
                CustomToManaged
            } else if occupied(&destination) {
                Conflicting
            } else {
                Missing
            }
        } else if occupied(&destination) {
            Unmanaged
        } else {
            Missing
        };
        items.push(Item::state(name, state));
    }
    let mut retired = receipt.managed.iter().collect::<Vec<_>>();
    retired.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, entry) in retired {
        if catalogues
            .managed
            .iter()
            .chain(&catalogues.custom)
            .any(|(n, _)| n == name)
        {
            continue;
        }
        let destination = target.join(name);
        let state = if same_link(&destination, Path::new(&entry.source_target)) {
            RetiredManaged
        } else if !occupied(&destination) {
            RetiredMissing
        } else {
            Conflicting
        };
        items.push(Item::state(name, state));
    }
    for (name, source) in &catalogues.custom {
        let destination = target.join(name);
        let state = if let Some(entry) = receipt.custom.get(name) {
            let recorded = Path::new(&entry.source_target);
            if crate::resolve(recorded)? != *source {
                if same_link(&destination, recorded) {
                    CustomRebind
                } else {
                    CustomConflicting
                }
            } else if same_link(&destination, source) {
                Custom
            } else {
                CustomConflicting
            }
        } else if let Some(previous) = receipt
            .managed
            .get(name)
            .filter(|_| !catalogues.managed.iter().any(|(n, _)| n == name))
        {
            if same_link(&destination, Path::new(&previous.source_target)) {
                ManagedToCustom
            } else {
                Conflicting
            }
        } else if same_link(&destination, source) {
            CustomUntracked
        } else if occupied(&destination) {
            Conflicting
        } else {
            CustomMissing
        };
        items.push(Item::state(name, state));
    }
    if custom_source_provided {
        let mut retired = receipt.custom.iter().collect::<Vec<_>>();
        retired.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, entry) in retired {
            if catalogues
                .managed
                .iter()
                .chain(&catalogues.custom)
                .any(|(n, _)| n == name)
            {
                continue;
            }
            let destination = target.join(name);
            let state = if same_link(&destination, Path::new(&entry.source_target)) {
                CustomRetired
            } else if !occupied(&destination) {
                CustomRetiredMissing
            } else {
                CustomConflicting
            };
            items.push(Item::state(name, state));
        }
    }
    Ok(items)
}

pub fn integrity(catalogues: &Catalogues, target: &Path) -> Result<Vec<Item>> {
    let mut items = vec![];
    // Catalogue merges preserve the managed position when a custom key replaces it.
    let mut catalogue = catalogues.managed.clone();
    for (name, path) in &catalogues.custom {
        if let Some((_, old)) = catalogue.iter_mut().find(|(n, _)| n == name) {
            *old = path.clone();
        } else {
            catalogue.push((name.clone(), path.clone()));
        }
    }
    for (name, source) in &catalogue {
        let destination = target.join(name);
        let owner = if catalogues.custom.iter().any(|(n, _)| n == name) {
            Owner::Custom
        } else {
            Owner::Managed
        };
        let state =
            if owner == Owner::Custom && (!source.is_dir() || !source.join("SKILL.md").is_file()) {
                State::MissingSource
            } else if same_link(&destination, source) {
                State::Present
            } else if !occupied(&destination) {
                State::Missing
            } else if destination.is_symlink() {
                State::Foreign
            } else if owner == Owner::Custom {
                State::Noncanonical
            } else if manifest::sha_skill(&destination)
                .ok()
                .zip(manifest::sha_skill(source).ok())
                .is_some_and(|(a, b)| a == b)
            {
                State::Present
            } else {
                State::Noncanonical
            };
        let mut item = Item::state(name, state);
        item.owner = Some(owner);
        item.scope = Some(Scope::Required);
        item.source_target = Some(crate::pending::path_text(source)?);
        items.push(item);
    }
    if target.is_dir() {
        for destination in sorted_children(target)? {
            let name = destination
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| invalid("extra skill link name is not UTF-8"))?;
            if name == ".DS_Store"
                || catalogue.iter().any(|(n, _)| n == name)
                || !destination.is_symlink()
            {
                continue;
            }
            let state = match crate::resolve(&destination) {
                Ok(resolved)
                    if catalogue.iter().any(|(_, p)| {
                        p.parent()
                            .is_some_and(|parent| resolved.starts_with(parent))
                    }) =>
                {
                    State::Noncanonical
                }
                _ => State::Foreign,
            };
            let mut item = Item::state(name, state);
            item.scope = Some(Scope::Extra);
            items.push(item);
        }
    }
    Ok(items)
}

pub fn raise_on_conflicts(
    items: &[Item],
    receipt: &Manifest,
    reject_unmanaged: bool,
) -> Result<()> {
    let custom = items
        .iter()
        .filter(|i| i.state == Some(State::CustomConflicting))
        .collect::<Vec<_>>();
    if !custom.is_empty() {
        let remedies=custom.iter().map(|i|receipt.custom.get(&i.name).map(|e|format!("manually restore {} from {} or provide --custom-source pointing to the intended custom skills directory",i.name,e.source_target)).ok_or_else(||invalid("custom conflict has no recorded source"))).collect::<Result<Vec<_>>>()?.join("; ");
        return Err(invalid(format!(
            "custom targets changed outside harness: {remedies}"
        )));
    }
    let mut names = items
        .iter()
        .filter(|i| i.state == Some(State::Conflicting))
        .map(|i| i.name.as_str())
        .collect::<Vec<_>>();
    if reject_unmanaged {
        names.extend(
            items
                .iter()
                .filter(|i| i.state == Some(State::Unmanaged))
                .map(|i| i.name.as_str()),
        );
    }
    if !names.is_empty() {
        return Err(invalid(format!(
            "conflicting managed targets changed outside harness: {}",
            names.join(", ")
        )));
    }
    Ok(())
}

fn legacy_check(source: &Path, target: &Path) -> Result<Report> {
    let source = crate::resolve(source)?;
    let catalogues = managed_skills::catalogues(&source, None, &[])?;
    let mut report = Report::new(Action::CheckLegacyAgy);
    if target.is_dir() {
        for destination in sorted_children(target)? {
            if !destination.is_symlink() {
                continue;
            }
            let state = if !destination.exists() {
                State::Dangling
            } else if crate::resolve(&destination)
                .ok()
                .is_some_and(|r| catalogues.managed.iter().any(|(_, s)| s == &r))
            {
                State::Current
            } else {
                State::Foreign
            };
            if state != State::Current {
                report.items.push(Item::state(
                    crate::pending::path_text(Path::new(
                        destination
                            .file_name()
                            .ok_or_else(|| invalid("legacy skill has no name"))?,
                    ))?,
                    state,
                ));
            }
        }
    }
    let path = manifest::manifest_path(target);
    if path.exists() {
        let receipt = fs::read(&path)
            .ok()
            .and_then(|b| ordered::decode(&b, "installation manifest is unreadable", &path).ok());
        if let Some(managed) = receipt.as_ref().and_then(|r| r["managed"].object()) {
            let mut retired = managed
                .iter()
                .filter(|(n, _)| !catalogues.managed.iter().any(|(current, _)| current == n))
                .map(|(n, _)| n)
                .collect::<Vec<_>>();
            retired.sort();
            for name in retired {
                report.items.push(Item::state(name, State::Retired));
            }
        } else {
            report.items.push(Item::state(
                manifest::MANIFEST_NAME,
                State::UnreadableReceipt,
            ));
        }
    }
    report.ok = Some(report.items.is_empty());
    Ok(report)
}

