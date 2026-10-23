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

/// A typed action entry point; callers provide a clock observation, never a global.
pub fn execute(
    action: Action,
    source: &Path,
    target: &Path,
    custom_source: Option<&Path>,
    now: &str,
) -> Result<Report> {
    if matches!(
        action,
        Action::PreflightRetirement | Action::RetireLegacyAgents
    ) {
        let parent = crate::resolve(target.parent().unwrap_or(Path::new(".")))?;
        let target = parent.join(
            target
                .file_name()
                .ok_or_else(|| invalid("skill target has no name"))?,
        );
        let changed = retire_legacy_agents(&target, action == Action::RetireLegacyAgents)?;
        let mut report = Report::new(action);
        if action == Action::RetireLegacyAgents {
            report.changed = changed;
        }
        return Ok(report);
    }
    if action == Action::CheckLegacyAgy {
        return legacy_check(source, target);
    }
    if source.is_symlink() {
        return Err(invalid("skill source contains a symlink: source"));
    }
    let source = crate::resolve(source)?;
    let custom_source = custom_source
        .map(|path| {
            if path.is_symlink() {
                Err(invalid("skill source contains a symlink: custom source"))
            } else {
                crate::resolve(path)
            }
        })
        .transpose()?;
    let custom_provided = custom_source.is_some();
    if matches!(
        action,
        Action::ValidateSources | Action::MigrateDirectoryLink
    ) {
        let catalogues = managed_skills::catalogues(&source, custom_source.as_deref(), &[])?;
        if action == Action::MigrateDirectoryLink {
            return migrate_directory_link(&source, target, &catalogues, now);
        }
        let mut report = Report::new(action);
        for (name, path) in catalogues.managed.iter().chain(&catalogues.custom) {
            let mut item = Item::state(name, State::Missing);
            item.state = None;
            item.source = Some(crate::pending::path_text(path)?);
            report.items.push(item);
        }
        return Ok(report);
    }
    let target = crate::resolve(target)?;
    let mut receipt = manifest::load_manifest(&target, now)?;
    let allowed = receipt
        .custom
        .iter()
        .filter(|(n, e)| same_link(&target.join(n), Path::new(&e.source_target)))
        .map(|(n, e)| (n.clone(), PathBuf::from(&e.source_target)))
        .collect::<Vec<_>>();
    let catalogues = managed_skills::catalogues(&source, custom_source.as_deref(), &allowed)?;
    if matches!(
        action,
        Action::Preflight | Action::Install | Action::Reconcile
    ) {
        retire_legacy_agents(&target, false)?;
    }
    if matches!(action, Action::Plan | Action::Preflight) {
        let mut report = Report::new(action);
        report.items = plan(&catalogues, &target, &receipt, custom_provided)?;
        if action == Action::Preflight {
            raise_on_conflicts(&report.items, &receipt, true)?;
        }
        return Ok(report);
    }
    if action == Action::Check {
        let custom = if custom_provided {
            catalogues.custom.clone()
        } else {
            receipt
                .custom
                .iter()
                .map(|(n, e)| (n.clone(), PathBuf::from(&e.source_target)))
                .collect()
        };
        let mut report = Report::new(action);
        report.items = integrity(
            &Catalogues {
                managed: catalogues.managed,
                custom,
            },
            &target,
        )?;
        report.ok = Some(
            report
                .items
                .iter()
                .filter(|i| i.scope == Some(Scope::Required))
                .all(|i| i.state == Some(State::Present)),
        );
        return Ok(report);
    }
    execute_mutation(
        action,
        &catalogues,
        &target,
        &mut receipt,
        custom_provided,
        now,
    )
}

fn catalogue_path<'a>(entries: &'a [(String, PathBuf)], name: &str) -> Result<&'a Path> {
    entries
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.as_path())
        .ok_or_else(|| invalid(format!("skill is not in source catalogue: {name}")))
}
fn execute_mutation(
    action: Action,
    catalogues: &Catalogues,
    target: &Path,
    receipt: &mut Manifest,
    custom_provided: bool,
    now: &str,
) -> Result<Report> {
    let items = plan(catalogues, target, receipt, custom_provided)?;
    raise_on_conflicts(&items, receipt, false)?;
    fs::create_dir_all(target).map_err(|e| io(target, e))?;
    let mut report = Report::new(action);
    let mut migrated = vec![];
    let mut migration_sources = MigrationSources::default();
    if matches!(action, Action::Install | Action::Reconcile) {
        for item in items {
            let name = item.name;
            match item.state {
                Some(State::Missing | State::Stale | State::CustomToManaged) => {
                    if item.state == Some(State::CustomToManaged) {
                        let previous = receipt
                            .custom
                            .get(&name)
                            .ok_or_else(|| invalid("custom migration has no recorded source"))?;
                        migration_sources
                            .0
                            .push((name.clone(), previous.source_target.clone()));
                    }
                    let source = catalogue_path(&catalogues.managed, &name)?;
                    crate::filesystem::replace_link(&target.join(&name), source)?;
                    let history = receipt
                        .managed
                        .get(&name)
                        .map(|e| e.history.clone())
                        .unwrap_or_default();
                    receipt
                        .managed
                        .insert(name.clone(), manifest::entry(source, history, now)?);
                    receipt.custom.remove(&name);
                    report.changed.push(name.clone());
                    if item.state == Some(State::CustomToManaged) {
                        migrated.push(name);
                    }
                }
                Some(State::RetiredManaged) => {
                    let destination = target.join(&name);
                    fs::remove_file(&destination).map_err(|e| io(&destination, e))?;
                    receipt.managed.remove(&name);
                    report.changed.push(name);
                }
                Some(State::RetiredMissing) => {
                    receipt.managed.remove(&name);
                    report.changed.push(name);
                }
                Some(
                    State::CustomMissing
                    | State::CustomUntracked
                    | State::CustomRebind
                    | State::ManagedToCustom,
                ) => {
                    let source = catalogue_path(&catalogues.custom, &name)?;
                    if item.state != Some(State::CustomUntracked) {
                        crate::filesystem::replace_link(&target.join(&name), source)?;
                    }
                    receipt.custom.insert(
                        name.clone(),
                        manifest::CustomLink {
                            source_target: crate::pending::path_text(source)?,
                        },
                    );
                    receipt.managed.remove(&name);
                    report.changed.push(name);
                }
                Some(State::CustomRetired) => {
                    let destination = target.join(&name);
                    fs::remove_file(&destination).map_err(|e| io(&destination, e))?;
                    receipt.custom.remove(&name);
                    report.changed.push(name);
                }
                Some(State::CustomRetiredMissing) => {
                    receipt.custom.remove(&name);
                    report.changed.push(name);
                }
                _ => {}
            }
        }
    } else if action == Action::UninstallManaged {
        let entries = receipt
            .managed
            .iter()
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect::<Vec<_>>();
        for (name, entry) in entries {
            let destination = target.join(&name);
            if same_link(&destination, Path::new(&entry.source_target)) {
                fs::remove_file(&destination).map_err(|e| io(&destination, e))?;
            } else if occupied(&destination) {
                return Err(invalid(format!(
                    "conflicting managed target changed outside harness: {name}"
                )));
            }
            receipt.managed.remove(&name);
            report.changed.push(name);
        }
    }
    if !report.changed.is_empty() {
        manifest::write_manifest(target, receipt, now)?;
    }
    if matches!(action, Action::Install | Action::Reconcile) {
        retire_legacy_agents(target, true)?;
    }
    report.items = plan(catalogues, target, receipt, custom_provided)?;
    report.migrated = Some(migrated);
    report.migration_sources = Some(migration_sources);
    Ok(report)
}

#[derive(Debug, clap::Parser)]
#[command(name = "manage_installation.py")]
pub struct Arguments {
    #[arg(value_enum)]
    pub action: Action,
    #[arg(long)]
    pub target: PathBuf,
    #[arg(long)]
    pub source: Option<PathBuf>,
    #[arg(long)]
    pub custom_source: Option<PathBuf>,
    #[arg(long)]
    pub summary: bool,
}
impl Arguments {
    pub fn run(&self, product_root: &Path, now: &str) -> crate::codex::CommandResult {
        run(
            self.action,
            self.source
                .as_deref()
                .unwrap_or(&product_root.join("skills")),
            &self.target,
            self.custom_source.as_deref(),
            self.summary,
            now,
        )
    }
}

fn report_json(report: &Report) -> Result<String> {
    let bytes = serde_json::to_vec(report).map_err(|e| invalid(e.to_string()))?;
    ordered::dumps(
        &ordered::decode(&bytes, "installation report", Path::new("<report>"))?,
        true,
        false,
    )
}
fn remedies(items: &[&Item]) -> String {
    items.iter().map(|i|format!("manually restore {} from {} or provide --custom-source pointing to the intended custom skills directory",i.name,i.source_target.as_deref().unwrap_or(""))).collect::<Vec<_>>().join("; ")
}
pub fn run(
    action: Action,
    source: &Path,
    target: &Path,
    custom_source: Option<&Path>,
    summary: bool,
    now: &str,
) -> crate::codex::CommandResult {
    let result = (|| -> Result<crate::codex::CommandResult> {
        let report = execute(action, source, target, custom_source, now)?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        if action == Action::Check {
            let warnings = report
                .items
                .iter()
                .filter(|i| i.scope == Some(Scope::Extra) && i.state != Some(State::Present))
                .map(|i| format!("{}={}", i.name, i.state.map(State::text).unwrap_or("")))
                .collect::<Vec<_>>();
            if !warnings.is_empty() {
                stderr.push_str(&format!(
                    "warning: unmanaged skill links outside catalogue: {}\n",
                    warnings.join(", ")
                ));
            }
        }
        if action == Action::CheckLegacyAgy {
            for item in &report.items {
                stdout.push_str(&if item.state == Some(State::Retired) {
                    format!(
                        "legacy agy skill retired={} (no longer in product)\n",
                        item.name
                    )
                } else {
                    format!(
                        "legacy agy skill stale={} ({})\n",
                        item.name,
                        item.state.map(State::text).unwrap_or("")
                    )
                });
            }
            return Ok(crate::codex::CommandResult {
                exit_code: i32::from(!report.items.is_empty()),
                stdout,
                stderr,
            });
        }
        if action == Action::Check && report.ok == Some(false) {
            stdout.push_str(&format!("{}\n", report_json(&report)?));
            let custom = report
                .items
                .iter()
                .filter(|i| {
                    i.owner == Some(Owner::Custom)
                        && i.scope == Some(Scope::Required)
                        && i.state != Some(State::Present)
                })
                .collect::<Vec<_>>();
            if !custom.is_empty() {
                stderr.push_str(&format!(
                    "conflicting: custom skill installation integrity failed: {}\n",
                    remedies(&custom)
                ));
            } else {
                let failures = report
                    .items
                    .iter()
                    .filter(|i| i.scope == Some(Scope::Required) && i.state != Some(State::Present))
                    .map(|i| format!("{}={}", i.name, i.state.map(State::text).unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(", ");
                stderr.push_str(&format!(
                    "conflicting: skill installation integrity failed: {failures}\n"
                ));
            }
            return Ok(crate::codex::CommandResult {
                exit_code: 3,
                stdout,
                stderr,
            });
        }
        if summary {
            match action {
                Action::PreflightRetirement => stdout.push_str(&format!(
                    "legacy retirement=checked target={}\n",
                    target.display()
                )),
                Action::RetireLegacyAgents => stdout.push_str(&format!(
                    "legacy retirement=complete target={}\n",
                    target.display()
                )),
                Action::MigrateDirectoryLink => stdout.push_str(&format!(
                    "skills layout={} linked={} target={}\n",
                    if report.layout == Some(Layout::Converted) {
                        "converted"
                    } else {
                        "per-entry"
                    },
                    report.changed.len(),
                    target.display()
                )),
                Action::Check => stdout.push_str(&format!(
                    "skills checked={} target={}\n",
                    report.items.len(),
                    target.display()
                )),
                _ => {
                    if action == Action::Install {
                        for name in report.migrated.as_deref().unwrap_or(&[]) {
                            if let Some((_, source)) = report
                                .migration_sources
                                .as_ref()
                                .and_then(|s| s.0.iter().find(|(n, _)| n == name))
                            {
                                stdout.push_str(&format!("skills migrated=custom-to-managed name={name} remove={source}\n"));
                            }
                        }
                    }
                    let linked = report.changed.len();
                    let existing = report
                        .items
                        .iter()
                        .filter(|i| {
                            matches!(
                                i.state,
                                Some(State::Managed | State::Unmanaged | State::Custom)
                            )
                        })
                        .count() as i128
                        - linked as i128;
                    stdout.push_str(&format!(
                        "skills linked={linked} existing={existing} target={}\n",
                        target.display()
                    ));
                }
            }
        } else {
            stdout.push_str(&format!("{}\n", report_json(&report)?));
        }
        Ok(crate::codex::CommandResult {
            exit_code: 0,
            stdout,
            stderr,
        })
    })();
    match result {
        Ok(result) => result,
        Err(error) => crate::codex::CommandResult {
            exit_code: 3,
            stdout: String::new(),
            stderr: format!("conflicting: {error}\n"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    const NOW: &str = "2026-10-23T12:00:00Z";
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
        custom: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let source = root.join("skills");
            let target = root.join("global/skills");
            let custom = root.join("custom");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&custom).unwrap();
            let f = Self {
                _dir: dir,
                root,
                source,
                target,
                custom,
            };
            f.skill("alpha", false);
            f.skill("zeta", false);
            fs::create_dir_all(f.source.join("_shared")).unwrap();
            fs::write(f.source.join("_shared/helper.py"), "shared").unwrap();
            f
        }
        fn skill(&self, name: &str, custom: bool) {
            let source = if custom { &self.custom } else { &self.source };
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(source.join(name).join("SKILL.md"), name).unwrap();
        }
        fn execute(&self, action: Action, custom: bool) -> Result<Report> {
            execute(
                action,
                &self.source,
                &self.target,
                custom.then_some(self.custom.as_path()),
                NOW,
            )
        }
        fn reset_target(&self) {
            let _ = fs::remove_dir_all(self.target.parent().unwrap());
        }
    }
    #[test]
    fn installs_all_shared_content_is_idempotent_and_preserves_unmanaged() {
        let f = Fixture::new();
        fs::create_dir_all(f.target.join("alpha")).unwrap();
        fs::write(f.target.join("alpha/SKILL.md"), "user").unwrap();
        assert!(f.execute(Action::Preflight, false).is_err());
        let result = f.execute(Action::Install, false).unwrap();
        assert_eq!(result.changed, ["zeta", "_shared"]);
        assert_eq!(
            fs::read_to_string(f.target.join("alpha/SKILL.md")).unwrap(),
            "user"
        );
        assert!(!manifest::load_manifest(&f.target, NOW)
            .unwrap()
            .managed
            .contains_key("alpha"));
        let before = fs::read(manifest::manifest_path(&f.target)).unwrap();
        assert!(f
            .execute(Action::Install, false)
            .unwrap()
            .changed
            .is_empty());
        assert_eq!(
            fs::read(manifest::manifest_path(&f.target)).unwrap(),
            before
        );
        let report = f.execute(Action::Check, false).unwrap();
        assert_eq!(report.ok, Some(false));
        f.execute(Action::UninstallManaged, false).unwrap();
        assert!(f.target.join("alpha/SKILL.md").is_file());
        assert!(!f.target.join("_shared").is_symlink());
    }
    #[test]
    fn broken_managed_links_repair_but_custom_missing_and_repointed_links_refuse() {
        let f = Fixture::new();
        f.skill("personal", true);
        f.execute(Action::Install, true).unwrap();
        fs::remove_file(f.target.join("alpha")).unwrap();
        std::os::unix::fs::symlink("missing", f.target.join("alpha")).unwrap();
        assert_eq!(
            f.execute(Action::Reconcile, true).unwrap().changed,
            ["alpha"]
        );
        fs::remove_file(f.target.join("personal")).unwrap();
        let before = fs::read(manifest::manifest_path(&f.target)).unwrap();
        assert!(f
            .execute(Action::Install, true)
            .unwrap_err()
            .to_string()
            .contains("custom targets changed outside harness"));
        assert_eq!(
            fs::read(manifest::manifest_path(&f.target)).unwrap(),
            before
        );
    }
    #[test]
    fn flagless_runs_preserve_custom_names_then_explicit_handoffs_are_tracked() {
        let f = Fixture::new();
        f.skill("personal", true);
        f.execute(Action::Install, true).unwrap();
        let before = fs::read(manifest::manifest_path(&f.target)).unwrap();
        assert!(f
            .execute(Action::Install, false)
            .unwrap()
            .changed
            .is_empty());
        assert_eq!(
            fs::read(manifest::manifest_path(&f.target)).unwrap(),
            before
        );
        f.skill("personal", false);
        assert!(f
            .execute(Action::Install, false)
            .unwrap()
            .changed
            .is_empty());
        assert!(same_link(
            &f.target.join("personal"),
            &f.custom.join("personal")
        ));
        let migrated = f.execute(Action::Install, true).unwrap();
        assert_eq!(migrated.migrated.unwrap(), ["personal"]);
        assert!(same_link(
            &f.target.join("personal"),
            &f.source.join("personal")
        ));
        fs::remove_dir_all(f.source.join("personal")).unwrap();
        let handed = f.execute(Action::Install, true).unwrap();
        assert_eq!(handed.changed, ["personal"]);
        assert!(same_link(
            &f.target.join("personal"),
            &f.custom.join("personal")
        ));
        fs::remove_dir_all(f.custom.join("personal")).unwrap();
        assert_eq!(
            f.execute(Action::Reconcile, true).unwrap().changed,
            ["personal"]
        );
        assert!(!f.target.join("personal").is_symlink());
    }
    #[test]
    fn canonical_directory_migration_and_legacy_retirement_keep_ownership_bounds() {
        let f = Fixture::new();
        f.skill("personal", true);
        fs::create_dir_all(f.target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&f.source, &f.target).unwrap();
        let report = f.execute(Action::MigrateDirectoryLink, true).unwrap();
        assert_eq!(report.layout, Some(Layout::Converted));
        assert!(!f.target.is_symlink());
        assert!(same_link(
            &f.target.join("_shared"),
            &f.source.join("_shared")
        ));
        assert_eq!(
            f.execute(Action::MigrateDirectoryLink, true)
                .unwrap()
                .layout,
            Some(Layout::PerEntry)
        );
        let agents = f.target.parent().unwrap().join("agents");
        fs::create_dir(&agents).unwrap();
        let original = f.root.join("agent.md");
        fs::write(&original, "agent").unwrap();
        std::os::unix::fs::symlink(&original, agents.join("a.md")).unwrap();
        fs::write(agents.join("unmanaged.md"), "user").unwrap();
        let receipt = json!({"schema_version":1,"owner":"agent-harness","surface":"claude-agents","target_root":agents,"managed":{"a.md":{"owner":"agent-harness","source_target":original,"source_sha256":"a".repeat(64),"installed_at":NOW}}});
        fs::write(
            f.target.parent().unwrap().join(LEGACY_AGENT_MANIFEST),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(retire_legacy_agents(&f.target, false).unwrap(), ["a.md"]);
        assert!(agents.join("a.md").is_symlink());
        assert_eq!(retire_legacy_agents(&f.target, true).unwrap(), ["a.md"]);
        assert!(agents.join("unmanaged.md").is_file());
        assert!(!agents.join("a.md").is_symlink());
    }
    #[test]
    fn full_action_sequence_matches_python_results_and_retained_receipt() {
        let f = Fixture::new();
        f.skill("personal", true);
        let body=format!("import json\nm.manifest_io.now=lambda: {NOW:?}\nsource=p.parent.parent/'skills';custom=p.parent.parent/'custom'\nout=[]\nfor action in ['validate-sources','plan','preflight','install','check','reconcile','uninstall-managed','check','install']:\n try: out.append(m.execute(action,source,p,custom))\n except m.InstallError as e: out.append(str(e))\nprint(json.dumps([out,m._load_manifest(p)]))");
        let Some(old) = crate::oracle::python("scripts/manage_installation.py", &body, &f.target)
        else {
            return;
        };
        f.reset_target();
        let mut reports = vec![];
        for action in [
            Action::ValidateSources,
            Action::Plan,
            Action::Preflight,
            Action::Install,
            Action::Check,
            Action::Reconcile,
            Action::UninstallManaged,
            Action::Check,
            Action::Install,
        ] {
            reports.push(match f.execute(action, true) {
                Ok(r) => serde_json::to_value(r).unwrap(),
                Err(e) => json!(e.to_string()),
            });
        }
        assert_eq!(
            serde_json::from_slice::<Value>(&old).unwrap(),
            json!([reports, manifest::load_manifest(&f.target, NOW).unwrap()])
        );
    }
    #[test]
    fn command_streams_summary_json_failures_and_warnings_match_python() {
        for summary in [false, true] {
            let f = Fixture::new();
            f.skill("personal", true);
            let body=format!("import json,contextlib,io\nm.manifest_io.now=lambda: {NOW:?}\nout=[]\nfor action in ['plan','preflight','install','check','reconcile','uninstall-managed','check','check-legacy-agy']:\n a=['--target',str(p),'--source',str(p.parent.parent/'skills'),'--custom-source',str(p.parent.parent/'custom'),action]+({}*['--summary'])\n stdout=io.StringIO();stderr=io.StringIO()\n with contextlib.redirect_stdout(stdout),contextlib.redirect_stderr(stderr): code=m.main(a)\n out.append([code,stdout.getvalue(),stderr.getvalue()])\nprint(json.dumps(out))",u8::from(summary));
            let Some(old) =
                crate::oracle::python("scripts/manage_installation.py", &body, &f.target)
            else {
                return;
            };
            f.reset_target();
            let mut results = vec![];
            for action in [
                Action::Plan,
                Action::Preflight,
                Action::Install,
                Action::Check,
                Action::Reconcile,
                Action::UninstallManaged,
                Action::Check,
                Action::CheckLegacyAgy,
            ] {
                let r = run(action, &f.source, &f.target, Some(&f.custom), summary, NOW);
                results.push(json!([r.exit_code, r.stdout, r.stderr]));
            }
            assert_eq!(
                serde_json::from_slice::<Value>(&old).unwrap(),
                json!(results)
            );
        }
    }
    #[test]
    fn relocation_rebinds_recorded_links_and_extras_only_warn() {
        let f = Fixture::new();
        f.execute(Action::Install, false).unwrap();
        let next = f.root.join("relocated");
        fs::rename(&f.source, &next).unwrap();
        let result = execute(Action::Install, &next, &f.target, None, NOW).unwrap();
        assert_eq!(result.changed, ["alpha", "zeta", "_shared"]);
        std::os::unix::fs::symlink(next.join("alpha"), f.target.join("extra")).unwrap();
        let result = run(Action::Check, &next, &f.target, None, true, NOW);
        assert_eq!(result.exit_code, 0);
        assert!(result.stderr.contains("extra=noncanonical"));
    }
}
