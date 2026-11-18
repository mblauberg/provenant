//! Host-local project identity indexed by the canonical Git common directory.
use crate::ConfigError;
use provenant_core::{Clock, ProjectId, Roots, WaitBudget};
use provenant_os::fs::{
    Directory, DirectoryPosition, FileKind, LockMode, Metadata, PublishMode, effective_uid,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

const PROJECTS_DIR: &str = "projects";
const RECORD: &str = "project.json";
const STAGE_PREFIX: &str = ".project-stage-";
const RECORD_LIMIT: u64 = 20 * 1024;
const MAX_PROJECTS: usize = 16_384;
const GC_SELECTION_SNAPSHOT_PREFIX: &str = "gc-project-selection-";
const GC_SELECTION_SNAPSHOT_VERSION: u64 = 4;
const GC_SELECTION_SNAPSHOT_LIMIT: u64 = 1024 * 1024;
const MAX_GC_SELECTION_SNAPSHOTS: usize = 64;
/// Reads of one name that may each meet a concurrent atomic replacement.
const REPLACEMENT_READS: usize = 16;

/// A canonical Git common directory, or the canonical directory for a
/// non-Git workspace. Discovery and canonicalisation belong to confinement.
#[derive(Clone, Debug)]
pub struct ProjectLocation {
    pub canonical_common_dir: PathBuf,
    canonical_primary_checkout_hint: Option<PathBuf>,
}

impl ProjectLocation {
    pub fn new(canonical_common_dir: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let location = Self {
            canonical_common_dir: canonical_common_dir.into(),
            canonical_primary_checkout_hint: None,
        };
        validate_location(&location.canonical_common_dir)?;
        Ok(location)
    }

    /// Preserve a primary checkout path proved by a Git-aware caller. The
    /// hint is not authority: consumers must re-open it with Git and confirm
    /// its primary private directory still is this common directory.
    pub fn from_verified_primary_checkout(
        canonical_common_dir: impl Into<PathBuf>,
        canonical_primary_checkout: impl Into<PathBuf>,
    ) -> Result<Self, ConfigError> {
        let mut location = Self::new(canonical_common_dir)?;
        let root = canonical_primary_checkout.into();
        validate_location(&root)?;
        location.canonical_primary_checkout_hint = Some(root);
        Ok(location)
    }

    pub fn common_dir(&self) -> &Path {
        &self.canonical_common_dir
    }

    pub fn primary_checkout_hint(&self) -> Option<&Path> {
        self.canonical_primary_checkout_hint.as_deref()
    }
}

impl PartialEq for ProjectLocation {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_common_dir == other.canonical_common_dir
    }
}

impl Eq for ProjectLocation {}

/// Persisted host-local identity for one canonical project location.
#[derive(Clone, Debug)]
pub struct ProjectBinding {
    project: ProjectId,
    canonical_common_dir: PathBuf,
    canonical_primary_checkout_hint: Option<PathBuf>,
}

impl ProjectBinding {
    pub fn project_id(&self) -> ProjectId {
        self.project
    }

    pub fn common_dir(&self) -> &Path {
        &self.canonical_common_dir
    }

    /// A previously proved primary checkout path, if any. This hint is not an
    /// authority grant and must be verified with Git before use.
    pub fn primary_checkout_hint(&self) -> Option<&Path> {
        self.canonical_primary_checkout_hint.as_deref()
    }
}

impl PartialEq for ProjectBinding {
    fn eq(&self, other: &Self) -> bool {
        self.project == other.project && self.canonical_common_dir == other.canonical_common_dir
    }
}

impl Eq for ProjectBinding {}

#[derive(Serialize, Deserialize)]
struct ProjectBindingRecord {
    schema_version: u64,
    project: ProjectId,
    /// Lowercase hex of native absolute path bytes, keeping the record
    /// portable across non-UTF-8 Unix directory names.
    canonical_common_dir: String,
    /// Additive layout hint from a checkout opened through UtilityRuntime.
    /// Older records intentionally remain common-dir-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_primary_checkout_hint: Option<String>,
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
}

/// Find or allocate the host-local identity for this canonical location.
/// The path-derived ID makes concurrent first bindings converge on one record.
pub fn bind_project(
    roots: &Roots,
    location: ProjectLocation,
) -> Result<ProjectBinding, ConfigError> {
    validate_location(&location.canonical_common_dir)?;
    if let Some(root) = &location.canonical_primary_checkout_hint {
        validate_location(root)?;
    }
    if transient_location(roots, location.common_dir()) {
        return transient_or_existing(roots, location);
    }

    let state = provenant_os::fs::Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, roots.state.as_path())?;
    let projects = open_or_create_projects(&state)?;
    let projects_path = roots.state.as_path().join(PROJECTS_DIR);
    verify_directory(&projects, &projects_path)?;

    let binding = match find_binding(&projects, &projects_path, &location.canonical_common_dir)? {
        Some(binding) => binding,
        None => create_binding(
            &projects,
            &projects_path,
            location.canonical_common_dir.clone(),
        )?,
    };
    match location.canonical_primary_checkout_hint {
        // Simultaneous launches from one checkout agree on the hint: only a
        // change takes the record lock and replaces the record.
        Some(root) if binding.primary_checkout_hint() == Some(root.as_path()) => Ok(binding),
        Some(root) => update_primary_checkout_hint(&projects, &projects_path, binding, root),
        None => Ok(binding),
    }
}

/// Lookup only: hooks never allocate identities or create state.
pub fn lookup_project(
    roots: &Roots,
    location: &ProjectLocation,
) -> Result<Option<ProjectBinding>, ConfigError> {
    lookup_project_at_state(roots.state.as_path(), location)
}

/// Resolve an existing host binding when an execution owner retains the state
/// anchor independently of the graph's attribution project.
pub fn lookup_project_at_state(
    state: &Path,
    location: &ProjectLocation,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let projects_path = state.join(PROJECTS_DIR);
    let binding = match existing_projects_at_state(state)? {
        Some(projects) => find_binding(&projects, &projects_path, location.common_dir())?,
        None => None,
    };
    Ok(binding.or_else(|| {
        transient_location_at_state(state, location.common_dir())
            .then(|| transient_binding(location.clone()))
    }))
}

/// Lookup only: every durable registered binding. Scheduled maintenance must
/// select Git-backed locations with registered_gc_projects instead.
pub fn registered_projects(roots: &Roots) -> Result<Vec<ProjectBinding>, ConfigError> {
    match existing_projects(roots)? {
        Some(projects) => bindings(&projects, &roots.state.as_path().join(PROJECTS_DIR)),
        None => Ok(Vec::new()),
    }
}

/// GC has no authority over non-Git workspaces or transient roots. Keep old
/// identity records for indexed runs, but never enumerate them as GC projects.
pub fn registered_gc_projects(roots: &Roots) -> Result<Vec<ProjectBinding>, ConfigError> {
    let selection = registered_gc_project_selection(roots)?;
    let mut projects = Vec::new();
    for project in selection.project_ids {
        if let GcProjectStatus::Git(binding) = lookup_gc_project(roots, project)? {
            projects.push(binding);
        }
    }
    Ok(projects)
}

#[derive(Debug)]
pub enum GcProjectStatus {
    Git(ProjectBinding),
    Transient(PathBuf),
    Excluded,
}

/// Resolve one selected ID against its current private record. Selection
/// snapshots carry IDs only; callers obtain paths fresh immediately before use.
pub fn lookup_gc_project(
    roots: &Roots,
    project: ProjectId,
) -> Result<GcProjectStatus, ConfigError> {
    let Some(registry) = existing_projects(roots)? else {
        return Ok(GcProjectStatus::Excluded);
    };
    let registry_path = roots.state.as_path().join(PROJECTS_DIR);
    let name = OsString::from(project.to_string());
    let directory = match registry.open_verified_directory(&name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(GcProjectStatus::Excluded),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&directory, &registry_path.join(&name))?;
    let Some(bytes) = read_record(&directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)? else {
        return Ok(GcProjectStatus::Excluded);
    };
    let record = decode_binding_record(&bytes)?;
    if record.schema_version != 1 || record.project != project {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let binding = binding_from_record(record)?;
    if transient_location(roots, binding.common_dir()) {
        Ok(GcProjectStatus::Transient(binding.common_dir().to_owned()))
    } else if git_location(binding.common_dir()) {
        Ok(GcProjectStatus::Git(binding))
    } else {
        Ok(GcProjectStatus::Excluded)
    }
}

/// Scheduled projects and transient registrations excluded from collection.
#[derive(Debug, Default)]
pub struct GcProjectSelection {
    pub project_ids: Vec<ProjectId>,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
    pub warnings: Vec<String>,
}

pub fn registered_gc_project_selection(roots: &Roots) -> Result<GcProjectSelection, ConfigError> {
    let mut keep_going = || true;
    registered_gc_project_selection_with_progress(
        roots,
        None,
        "0000000000000000",
        false,
        &mut keep_going,
    )
}

#[derive(Serialize, Deserialize)]
struct GcSelectionSnapshot {
    version: u64,
    epoch: String,
    root_device: u64,
    root_inode: u64,
    root_modified_ns: i128,
    offset: DirectoryPosition,
    next_key: Option<String>,
    scanned: usize,
    project_ids: BTreeSet<ProjectId>,
}

fn gc_selection_snapshot_name(epoch: &str) -> Option<String> {
    (epoch.len() == 16
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| format!("{GC_SELECTION_SNAPSHOT_PREFIX}{epoch}.json"))
}

fn gc_selection_snapshot_epoch(name: &str) -> Option<&str> {
    let epoch = name
        .strip_prefix(GC_SELECTION_SNAPSHOT_PREFIX)?
        .strip_suffix(".json")?;
    gc_selection_snapshot_name(epoch).map(|_| epoch)
}

fn gc_selection_tick(state_path: &Path, create: bool) -> Result<Option<Directory>, ConfigError> {
    let state = match Directory::open_anchor(state_path) {
        Ok(state) => state,
        Err(error) if error.is_not_found() && !create => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&state, state_path)?;
    let name = OsStr::new("tick");
    let tick = match state.open_verified_directory(name) {
        Ok(tick) => tick,
        Err(error) if error.is_not_found() && !create => return Ok(None),
        Err(error) if error.is_not_found() => {
            match state.mkdir(name, 0o700) {
                Ok(()) => state.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            state.open_verified_directory(name)?
        }
        Err(error) => return Err(error.into()),
    };
    verify_directory(&tick, &state_path.join("tick"))?;
    Ok(Some(tick))
}

fn prune_gc_selection_snapshots(tick: &Directory, active_detail: Option<&str>) -> Vec<String> {
    let mut warnings = Vec::new();
    let entries = match tick.entries() {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("project selection sidecar scan: {error}"));
            return warnings;
        }
    };
    for entry in entries.take(MAX_GC_SELECTION_SNAPSHOTS) {
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                warnings.push(format!("project selection sidecar entry: {error}"));
                continue;
            }
        };
        let Some(name_text) = name.to_str() else {
            continue;
        };
        if (gc_selection_snapshot_epoch(name_text).is_none()
            && !name_text.starts_with("gc-project-selection-page-"))
            || active_detail == Some(name_text)
        {
            continue;
        }
        match tick.metadata_at(&name, false) {
            Ok(metadata)
                if metadata.kind == FileKind::Regular && metadata.uid == effective_uid() =>
            {
                if let Err(error) = tick.unlink(&name) {
                    warnings.push(format!("project selection sidecar cleanup: {error}"));
                }
            }
            Ok(_) => warnings.push(format!("project selection sidecar retained: {name_text}")),
            Err(error) => warnings.push(format!("project selection sidecar stat: {error}")),
        }
    }
    warnings
}

fn save_gc_selection_snapshot(
    tick: &Directory,
    snapshot: &mut GcSelectionSnapshot,
    epoch: &str,
) -> Result<String, ConfigError> {
    let name = gc_selection_snapshot_name(epoch).ok_or(ConfigError::InvalidProjectBinding)?;
    snapshot.version = GC_SELECTION_SNAPSHOT_VERSION;
    snapshot.epoch = epoch.into();
    let bytes = serde_json::to_vec(snapshot).map_err(|_| ConfigError::InvalidProjectBinding)?;
    if bytes.len() as u64 > GC_SELECTION_SNAPSHOT_LIMIT {
        return Err(ConfigError::TooLarge {
            path: tick.current_path()?.join(&name),
            limit: GC_SELECTION_SNAPSHOT_LIMIT,
        });
    }
    tick.atomic_write(
        OsStr::new(&name),
        ".gc-project-selection-",
        &bytes,
        PublishMode::Replace,
    )?
    .require_durable()?;
    Ok(name)
}

fn checkpoint_gc_selection(
    selection: &mut GcProjectSelection,
    snapshot: &mut GcSelectionSnapshot,
    tick: Option<&Directory>,
    persist: bool,
    epoch: &str,
    candidate: PathBuf,
) {
    selection.interrupted_at = Some(candidate);
    if !persist {
        return;
    }
    let Some(tick) = tick else { return };
    match save_gc_selection_snapshot(tick, snapshot, epoch) {
        Ok(name) => {
            selection.continuation = Some(name.clone());
            selection
                .warnings
                .extend(prune_gc_selection_snapshots(tick, Some(&name)));
        }
        Err(error) => selection
            .warnings
            .push(format!("project selection cursor was not saved: {error}")),
    }
}

/// Enumerate registered project IDs in resumable batches. Paths are resolved
/// from their current records by callers immediately before use.
pub fn registered_gc_project_selection_with_progress(
    roots: &Roots,
    resume_detail: Option<&str>,
    epoch: &str,
    persist: bool,
    keep_going: &mut dyn FnMut() -> bool,
) -> Result<GcProjectSelection, ConfigError> {
    let mut selection = GcProjectSelection::default();
    let Some(registry) = existing_projects(roots)? else {
        return Ok(selection);
    };
    let registry_path = roots.state.as_path().join(PROJECTS_DIR);
    let root_metadata = registry.metadata()?;
    let mut snapshot = GcSelectionSnapshot {
        version: GC_SELECTION_SNAPSHOT_VERSION,
        epoch: epoch.into(),
        root_device: root_metadata.device,
        root_inode: root_metadata.inode,
        root_modified_ns: root_metadata.modified_ns,
        offset: DirectoryPosition::default(),
        next_key: None,
        scanned: 0,
        project_ids: BTreeSet::new(),
    };
    let tick = if persist {
        gc_selection_tick(roots.state.as_path(), true)?
    } else {
        None
    };
    if let (Some(tick), Some(detail)) = (tick.as_ref(), resume_detail) {
        if let Some(token_epoch) = gc_selection_snapshot_epoch(detail) {
            match tick.read_stable(OsStr::new(detail), GC_SELECTION_SNAPSHOT_LIMIT) {
                Ok(bytes) => match serde_json::from_slice::<GcSelectionSnapshot>(&bytes) {
                    Ok(saved)
                        if saved.version == GC_SELECTION_SNAPSHOT_VERSION
                            && saved.epoch == token_epoch =>
                    {
                        snapshot = saved;
                        if snapshot.root_device != root_metadata.device
                            || snapshot.root_inode != root_metadata.inode
                            || snapshot.root_modified_ns != root_metadata.modified_ns
                        {
                            selection.warnings.push(
                                "project registry changed; resuming after a bounded rescan".into(),
                            );
                            snapshot.offset = DirectoryPosition::default();
                            snapshot.next_key = None;
                            snapshot.scanned = 0;
                            snapshot.root_device = root_metadata.device;
                            snapshot.root_inode = root_metadata.inode;
                            snapshot.root_modified_ns = root_metadata.modified_ns;
                        }
                    }
                    Ok(_) => selection
                        .warnings
                        .push("project selection sidecar identity changed; restarting scan".into()),
                    Err(error) => selection
                        .warnings
                        .push(format!("project selection sidecar ignored: {error}")),
                },
                Err(error) => selection
                    .warnings
                    .push(format!("project selection sidecar unavailable: {error}")),
            }
        } else {
            selection
                .warnings
                .push("invalid project selection cursor; restarting registry scan".into());
        }
    }
    if let Some(tick) = tick.as_ref() {
        selection
            .warnings
            .extend(prune_gc_selection_snapshots(tick, resume_detail));
    }
    snapshot.epoch = epoch.into();
    let mut entries = registry.entries()?;
    let mut pending = None;
    if let Some(next_key) = snapshot.next_key.as_deref() {
        if entries.seek(snapshot.offset).is_ok() {
            let offset = entries.position()?;
            match entries.next() {
                Some(entry) => {
                    let name = entry?;
                    if provenant_core::hex(name.as_bytes()) == next_key {
                        pending = Some((name, offset));
                    } else {
                        selection.warnings.push(
                            "project registry cursor changed; rescanning without discarding selected IDs".into(),
                        );
                        snapshot.offset = DirectoryPosition::default();
                        snapshot.next_key = None;
                        snapshot.scanned = 0;
                        entries = registry.entries()?;
                    }
                }
                None => {
                    selection.warnings.push(
                        "project registry cursor changed; rescanning without discarding selected IDs".into(),
                    );
                    snapshot.offset = DirectoryPosition::default();
                    snapshot.next_key = None;
                    snapshot.scanned = 0;
                    entries = registry.entries()?;
                }
            }
        } else {
            selection.warnings.push(
                "project registry cursor expired; rescanning without discarding selected IDs"
                    .into(),
            );
            snapshot.offset = DirectoryPosition::default();
            snapshot.next_key = None;
            snapshot.scanned = 0;
            entries = registry.entries()?;
        }
    }

    loop {
        let (entry, entry_offset) = match pending.take() {
            Some(pending) => pending,
            None => {
                let entry_offset = entries.position()?;
                let Some(entry) = entries.next() else { break };
                (entry?, entry_offset)
            }
        };
        if !keep_going() {
            snapshot.offset = entry_offset;
            snapshot.next_key = Some(provenant_core::hex(entry.as_bytes()));
            checkpoint_gc_selection(
                &mut selection,
                &mut snapshot,
                tick.as_deref(),
                persist,
                epoch,
                registry_path.join(&entry),
            );
            return Ok(selection);
        }
        snapshot.offset = entries.position()?;
        snapshot.next_key = None;
        let Some(name) = entry.to_str() else {
            return Err(ConfigError::InvalidProjectBinding);
        };
        if let Some(id) = name.strip_prefix(STAGE_PREFIX)
            && id
                .parse::<ProjectId>()
                .is_ok_and(|project| format!("{STAGE_PREFIX}{project}") == name)
        {
            continue;
        }
        snapshot.scanned += 1;
        if snapshot.scanned > MAX_PROJECTS {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let project = name
            .parse::<ProjectId>()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        snapshot.project_ids.insert(project);
    }

    let final_metadata = registry.metadata()?;
    if final_metadata.device != root_metadata.device
        || final_metadata.inode != root_metadata.inode
        || final_metadata.modified_ns != root_metadata.modified_ns
    {
        selection.warnings.push(
            "project registry changed during discovery; preserving selected IDs for the next pass"
                .into(),
        );
        snapshot.root_device = final_metadata.device;
        snapshot.root_inode = final_metadata.inode;
        snapshot.root_modified_ns = final_metadata.modified_ns;
        snapshot.offset = DirectoryPosition::default();
        snapshot.next_key = None;
        snapshot.scanned = 0;
        checkpoint_gc_selection(
            &mut selection,
            &mut snapshot,
            tick.as_deref(),
            persist,
            epoch,
            registry_path,
        );
        return Ok(selection);
    }
    selection.project_ids = snapshot.project_ids.into_iter().collect();
    if let Some(tick) = tick.as_ref() {
        if let Some(detail) = resume_detail
            && gc_selection_snapshot_epoch(detail).is_some()
        {
            match tick.unlink(OsStr::new(detail)) {
                Ok(()) => {}
                Err(error) if error.is_not_found() => {}
                Err(error) => selection
                    .warnings
                    .push(format!("project selection sidecar cleanup: {error}")),
            }
        }
        selection
            .warnings
            .extend(prune_gc_selection_snapshots(tick, None));
    }
    Ok(selection)
}

fn git_location(path: &Path) -> bool {
    let Ok(directory) = Directory::open_anchor(path) else {
        return false;
    };
    matches!(directory.metadata_at(OsStr::new("HEAD"), false), Ok(meta) if meta.kind == FileKind::Regular)
        && directory
            .open_verified_directory(OsStr::new("objects"))
            .is_ok()
}

/// Whether a location is excluded from persistent scheduled collection.
pub fn transient_location(roots: &Roots, path: &Path) -> bool {
    transient_location_at_state(roots.state.as_path(), path)
}

fn transient_location_at_state(state: &Path, path: &Path) -> bool {
    if path.starts_with(state.join("attempts")) || path.starts_with(state.join("children")) {
        return true;
    }
    if path
        .strip_prefix(state)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| component.as_os_str().as_bytes().starts_with(b"probe-"))
    {
        return true;
    }
    if path.components().any(|component| {
        let name = component.as_os_str().as_bytes();
        name.starts_with(b".provenant-staging.") || name == b"scratchpads"
    }) {
        return true;
    }
    // Non-Git invocation roots (including the home directory and /tmp) need
    // a run identity without adding a persistent scheduled project.
    !git_location(path) && path.file_name() != Some(OsStr::new(".git"))
}

// Existing run evidence keeps its project identity when a checkout becomes
// transient. Only new locations use a non-persisted identity.
fn transient_or_existing(
    roots: &Roots,
    location: ProjectLocation,
) -> Result<ProjectBinding, ConfigError> {
    let projects_path = roots.state.as_path().join(PROJECTS_DIR);
    if let Some(projects) = existing_projects(roots)?
        && let Some(binding) = find_binding(&projects, &projects_path, location.common_dir())?
    {
        return Ok(binding);
    }
    Ok(transient_binding(location))
}

fn transient_binding(location: ProjectLocation) -> ProjectBinding {
    ProjectBinding {
        project: binding_id(location.common_dir()),
        canonical_common_dir: location.canonical_common_dir,
        canonical_primary_checkout_hint: location.canonical_primary_checkout_hint,
    }
}

fn binding_id(path: &Path) -> ProjectId {
    let digest = provenant_records::CanonicalV1::file_digest(path.as_os_str().as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&digest.bytes()[..16]);
    ProjectId::from_bytes(id)
}

fn existing_projects(roots: &Roots) -> Result<Option<Directory>, ConfigError> {
    existing_projects_at_state(roots.state.as_path())
}

fn existing_projects_at_state(state_path: &Path) -> Result<Option<Directory>, ConfigError> {
    let state = match provenant_os::fs::Directory::open_anchor(state_path) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&state, state_path)?;
    let projects = match state.open_verified_directory(OsStr::new(PROJECTS_DIR)) {
        Ok(projects) => projects,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let projects_path = state_path.join(PROJECTS_DIR);
    verify_directory(&projects, &projects_path)?;
    Ok(Some(projects))
}

fn find_binding(
    projects: &Directory,
    projects_path: &Path,
    target: &Path,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let mut found = None;
    for binding in bindings(projects, projects_path)? {
        if binding.common_dir() == target {
            if found.is_some() {
                return Err(ConfigError::InvalidProjectBinding);
            }
            found = Some(binding);
        }
    }
    Ok(found)
}

fn bindings(
    projects: &Directory,
    projects_path: &Path,
) -> Result<Vec<ProjectBinding>, ConfigError> {
    let mut count = 0;
    let mut found = Vec::new();
    for entry in projects.entries()? {
        let entry = entry?;
        let name = entry.to_str().ok_or(ConfigError::InvalidProjectBinding)?;
        // Old staging records and dotfiles are not binding directories.
        if let Some(id) = name.strip_prefix(STAGE_PREFIX)
            && id
                .parse::<ProjectId>()
                .is_ok_and(|project| format!("{STAGE_PREFIX}{project}") == name)
        {
            continue;
        }
        count += 1;
        if count > MAX_PROJECTS {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let project = name
            .parse::<ProjectId>()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        let directory = projects.open_verified_directory(&entry)?;
        verify_directory(&directory, &projects_path.join(name))?;
        // A concurrent first binding may have made the directory but not yet
        // atomically published its record. Its deterministic owner can finish
        // publication; readers leave it alone.
        let Some(bytes) = read_record(&directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)?
        else {
            continue;
        };
        let record = decode_binding_record(&bytes)?;
        if record.schema_version != 1 || record.project != project {
            return Err(ConfigError::InvalidProjectBinding);
        }
        found.push(binding_from_record(record)?);
    }
    Ok(found)
}

fn binding_from_record(record: ProjectBindingRecord) -> Result<ProjectBinding, ConfigError> {
    if record.schema_version != 1 {
        return Err(ConfigError::InvalidProjectBinding);
    }
    Ok(ProjectBinding {
        project: record.project,
        canonical_common_dir: decode_location(&record.canonical_common_dir)?,
        canonical_primary_checkout_hint: record
            .canonical_primary_checkout_hint
            .as_deref()
            .map(decode_location)
            .transpose()?,
    })
}

fn update_primary_checkout_hint(
    projects: &Directory,
    projects_path: &Path,
    binding: ProjectBinding,
    primary_checkout: PathBuf,
) -> Result<ProjectBinding, ConfigError> {
    let name = OsString::from(binding.project_id().to_string());
    let directory = projects.open_verified_directory(&name)?;
    verify_directory(&directory, &projects_path.join(&name))?;
    let clock = provenant_os::clock::SystemClock::new();
    let budget =
        WaitBudget::cli(clock.mono_now(), 5).map_err(|_| ConfigError::InvalidProjectBinding)?;
    let _lock = directory.lock(
        OsStr::new("project-binding.lock"),
        LockMode::Exclusive,
        budget,
        &provenant_core::CancelToken::new(),
        &clock,
    )?;
    let mut record = decode_binding_record(&published_record(&directory)?)?;
    if record.schema_version != 1
        || record.project != binding.project_id()
        || decode_location(&record.canonical_common_dir)? != binding.canonical_common_dir
    {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let encoded_primary_checkout = encode_location(&primary_checkout);
    if record.canonical_primary_checkout_hint.as_deref() != Some(encoded_primary_checkout.as_str())
    {
        record.canonical_primary_checkout_hint = Some(encoded_primary_checkout);
        let bytes =
            provenant_records::encode(&record).map_err(|_| ConfigError::InvalidProjectBinding)?;
        directory
            .atomic_write_with_permissions(
                OsStr::new(RECORD),
                ".project-",
                &bytes,
                0o600,
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        directory.sync()?;
    }
    let stored = decode_binding_record(&published_record(&directory)?)?;
    let stored = binding_from_record(stored)?;
    if stored.project_id() != binding.project_id()
        || stored.common_dir() != binding.common_dir()
        || stored.primary_checkout_hint() != Some(primary_checkout.as_path())
    {
        return Err(ConfigError::InvalidProjectBinding);
    }
    projects.sync()?;
    Ok(stored)
}

fn create_binding(
    projects: &Directory,
    projects_path: &Path,
    canonical_common_dir: PathBuf,
) -> Result<ProjectBinding, ConfigError> {
    let project = binding_id(&canonical_common_dir);
    let name = OsString::from(project.to_string());
    match projects.mkdir(&name, 0o700) {
        Ok(()) => projects.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let directory = projects.open_verified_directory(&name)?;
    verify_directory(&directory, &projects_path.join(&name))?;
    let record = ProjectBindingRecord {
        schema_version: 1,
        project,
        canonical_common_dir: encode_location(&canonical_common_dir),
        canonical_primary_checkout_hint: None,
        extra: Default::default(),
    };
    let bytes =
        provenant_records::encode(&record).map_err(|_| ConfigError::InvalidProjectBinding)?;
    match directory.atomic_write_with_permissions(
        OsStr::new(RECORD),
        ".project-",
        &bytes,
        0o600,
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication
            .require_durable()
            .map_err(|_| ConfigError::InvalidProjectBinding)?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    directory.sync()?;
    let stored = decode_binding_record(&published_record(&directory)?)?;
    if stored.schema_version != 1
        || stored.project != project
        || decode_location(&stored.canonical_common_dir)? != canonical_common_dir
    {
        return Err(ConfigError::InvalidProjectBinding);
    }
    projects.sync()?;
    Ok(ProjectBinding {
        project,
        canonical_common_dir,
        canonical_primary_checkout_hint: None,
    })
}

fn decode_binding_record(bytes: &[u8]) -> Result<ProjectBindingRecord, ConfigError> {
    let limits = provenant_records::Limits {
        max_bytes: RECORD_LIMIT,
        ..provenant_records::Limits::CONFIG
    };
    provenant_records::decode(bytes, limits).map_err(|_| ConfigError::InvalidProjectBinding)
}

fn open_or_create_projects(state: &Directory) -> Result<Directory, ConfigError> {
    match state.open_verified_directory(OsStr::new(PROJECTS_DIR)) {
        Ok(directory) => Ok(directory),
        Err(error) if error.is_not_found() => {
            match state.mkdir(OsStr::new(PROJECTS_DIR), 0o700) {
                Ok(()) => state.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            Ok(state.open_verified_directory(OsStr::new(PROJECTS_DIR))?)
        }
        Err(error) => Err(error.into()),
    }
}

fn verify_directory(directory: &Directory, path: &Path) -> Result<(), ConfigError> {
    let metadata = directory.metadata()?;
    if metadata.kind != FileKind::Directory {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let owner_matches = metadata.uid == effective_uid();
    let mode = metadata.mode & 0o7777;
    if !owner_matches || mode & 0o077 != 0 {
        return Err(ConfigError::ProjectBindingDirectoryUnsafe {
            path: path.to_owned(),
            mode,
            owner_matches,
        });
    }
    Ok(())
}

/// A binding record that must already be published in this directory.
fn published_record(directory: &Directory) -> Result<Vec<u8>, ConfigError> {
    read_record(directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)?
        .ok_or(ConfigError::InvalidProjectBinding)
}

/// Read one private record, or `None` when it is not published. Records are
/// only ever published by atomic rename, so a name whose identity changes
/// during the read, or that resolves to an inode the rename just unlinked, was
/// replaced by another complete record: resolve it again. A shape the caller
/// refuses, or a name that keeps changing, is invalid.
fn read_record(
    directory: &Directory,
    name: &OsStr,
    limit: u64,
    accept: impl Fn(&Metadata) -> bool,
) -> Result<Option<Vec<u8>>, ConfigError> {
    Ok(read_record_with_metadata(directory, name, limit, accept)?.map(|(bytes, _)| bytes))
}

fn read_record_with_metadata(
    directory: &Directory,
    name: &OsStr,
    limit: u64,
    accept: impl Fn(&Metadata) -> bool,
) -> Result<Option<(Vec<u8>, Metadata)>, ConfigError> {
    for _ in 0..REPLACEMENT_READS {
        let entry = match directory.metadata_at(name, false) {
            Ok(entry) => entry,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        // A stat that resolves the name while a rename replaces it can report
        // the old inode already unlinked. The name now holds the new record.
        if entry.links == 0 {
            continue;
        }
        if entry.kind != FileKind::Regular
            || entry.links != 1
            || entry.size > limit
            || !accept(&entry)
        {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let bytes = match directory.read_stable(name, limit) {
            Ok(bytes) => bytes,
            Err(provenant_os::fs::FsError::RaceRefused { .. }) => continue,
            // Replaced by a rename after the stat: the next read resolves it.
            Err(error) if error.is_not_found() => continue,
            Err(
                provenant_os::fs::FsError::TooLarge { .. }
                | provenant_os::fs::FsError::ReadIo { .. },
            ) => return Err(ConfigError::InvalidProjectBinding),
            Err(other) => return Err(other.into()),
        };
        match directory.metadata_at(name, false) {
            Ok(after) if entry.unchanged(&after) => return Ok(Some((bytes, after))),
            Ok(_) => continue,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(ConfigError::InvalidProjectBinding)
}

fn validate_location(path: &Path) -> Result<(), ConfigError> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || path == Path::new("/")
        || bytes.contains(&0)
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
        || bytes[1..]
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(ConfigError::InvalidProjectLocation);
    }
    Ok(())
}

fn encode_location(path: &Path) -> String {
    provenant_core::hex(path.as_os_str().as_bytes())
}

fn decode_location(value: &str) -> Result<PathBuf, ConfigError> {
    let bytes = provenant_core::unhex(value).ok_or(ConfigError::InvalidProjectBinding)?;
    let path = PathBuf::from(OsString::from_vec(bytes));
    validate_location(&path).map_err(|_| ConfigError::InvalidProjectBinding)?;
    Ok(path)
}

/// Initialise the host identity once with atomic no-replace publication.
/// Never reads legacy roots.
pub fn bind_host(roots: &Roots) -> Result<provenant_core::HostId, ConfigError> {
    let state = provenant_os::fs::Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, roots.state.as_path())?;
    let name = OsStr::new("host-id");
    match read_host(&state, name) {
        Ok(Some(host)) => Ok(host),
        Ok(None) => {
            let id = provenant_core::HostId::from_bytes(provenant_os::fs::entropy_128()?);
            match state.atomic_write_with_permissions(
                name,
                ".host-",
                id.to_string().as_bytes(),
                0o600,
                PublishMode::NoReplace,
            ) {
                Ok(publication) => {
                    publication
                        .require_durable()
                        .map_err(|_| ConfigError::InvalidProjectBinding)?;
                    Ok(id)
                }
                Err(error) if error.is_exists() => {
                    read_host(&state, name)?.ok_or(ConfigError::InvalidProjectBinding)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error),
    }
}

/// Read an existing host identity without creating state or a lock.
pub fn lookup_host(roots: &Roots) -> Result<Option<provenant_core::HostId>, ConfigError> {
    let state = provenant_os::fs::Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, roots.state.as_path())?;
    read_host(&state, OsStr::new("host-id"))
}

fn read_host(
    state: &Directory,
    name: &OsStr,
) -> Result<Option<provenant_core::HostId>, ConfigError> {
    let private =
        |meta: &Metadata| meta.uid == effective_uid() && meta.mode & 0o077 == 0 && meta.size == 32;
    let Some(bytes) = read_record(state, name, 32, private)? else {
        return Ok(None);
    };
    if bytes.len() != 32 {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
    text.parse()
        .map(Some)
        .map_err(|_| ConfigError::InvalidProjectBinding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::Roots;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn canonical_base(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().canonicalize().unwrap()
    }

    fn roots(temp: &tempfile::TempDir) -> Roots {
        let inputs = provenant_test_support::test_roots(temp.path()).unwrap();
        inputs
            .resolve(&provenant_test_support::IdentityPathResolver)
            .unwrap()
    }

    #[test]
    fn a_checkout_that_stops_being_git_keeps_its_existing_run_identity() {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("bare");
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        let location = ProjectLocation::new(&common).unwrap();
        let before = bind_project(&roots, location.clone()).unwrap();
        assert_eq!(before.common_dir(), common);
        fs::remove_file(common.join("HEAD")).unwrap();
        assert_eq!(bind_project(&roots, location.clone()).unwrap(), before);
        assert_eq!(lookup_project(&roots, &location).unwrap(), Some(before));
        assert!(registered_gc_projects(&roots).unwrap().is_empty());
    }

    #[test]
    fn registry_readers_skip_in_progress_stages_without_repair() {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("repo/.git");
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        let binding = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
        let stage = roots
            .state
            .as_path()
            .join(PROJECTS_DIR)
            .join(format!("{STAGE_PREFIX}{}", ProjectId::from_bytes([31; 16])));
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join(RECORD), "partial").unwrap();
        assert!(registered_gc_projects(&roots).unwrap().contains(&binding));

        let next = canonical_base(&temp).join("next/.git");
        fs::create_dir_all(next.join("objects")).unwrap();
        fs::write(next.join("HEAD"), "ref: refs/heads/main").unwrap();
        bind_project(&roots, ProjectLocation::new(&next).unwrap()).unwrap();
        assert_eq!(fs::read_to_string(stage.join(RECORD)).unwrap(), "partial");
    }

    #[test]
    fn transient_roots_have_identity_without_persistent_registration() {
        let temp = tempdir();
        let roots = roots(&temp);
        for path in [
            roots.state.as_path().join("probe-one"),
            roots.state.as_path().join("attempts/run"),
            canonical_base(&temp).join(".provenant-staging.one/.git"),
            canonical_base(&temp).join("scratchpads/x"),
            canonical_base(&temp).join("not-git"),
        ] {
            fs::create_dir_all(&path).unwrap();
            let location = ProjectLocation::new(&path).unwrap();
            let first = bind_project(&roots, location.clone()).unwrap();
            assert_eq!(
                lookup_project(&roots, &location).unwrap(),
                Some(first.clone())
            );
            assert_eq!(bind_project(&roots, location).unwrap(), first);
        }
        assert!(!roots.state.as_path().join(PROJECTS_DIR).exists());
        assert!(registered_gc_projects(&roots).unwrap().is_empty());
    }

    #[test]
    fn scheduled_projects_exclude_missing_non_git_and_old_transient_bindings() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let valid = base.join("repository/.git");
        fs::create_dir_all(valid.join("objects")).unwrap();
        fs::write(valid.join("HEAD"), "ref: refs/heads/main").unwrap();
        let good = bind_project(&roots, ProjectLocation::new(&valid).unwrap()).unwrap();
        let stale = base.join("stale/.git");
        fs::create_dir_all(&stale).unwrap();
        bind_project(&roots, ProjectLocation::new(&stale).unwrap()).unwrap();
        fs::remove_dir(&stale).unwrap();
        let mut excluded = Vec::new();
        let projects = existing_projects(&roots).unwrap().unwrap();
        for relative in ["scratchpads/repo/.git", ".provenant-staging.repo/.git"] {
            let common = base.join(relative);
            fs::create_dir_all(common.join("objects")).unwrap();
            fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
            // Simulate a record created before transient registration filtering.
            create_binding(
                &projects,
                &roots.state.as_path().join(PROJECTS_DIR),
                common.clone(),
            )
            .unwrap();
            excluded.push(common);
        }
        // Existing identity records are preserved for indexed run evidence.
        assert_eq!(registered_projects(&roots).unwrap().len(), 4);
        assert_eq!(registered_gc_projects(&roots).unwrap(), vec![good.clone()]);
        let selection = registered_gc_project_selection(&roots).unwrap();
        let mut selected = Vec::new();
        let mut skipped_transient = Vec::new();
        for project in selection.project_ids {
            match lookup_gc_project(&roots, project).unwrap() {
                GcProjectStatus::Git(binding) => selected.push(binding),
                GcProjectStatus::Transient(path) => skipped_transient.push(path),
                GcProjectStatus::Excluded => {}
            }
        }
        assert_eq!(selected, vec![good]);
        skipped_transient.sort();
        excluded.sort();
        assert_eq!(skipped_transient, excluded);
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn scheduled_registry_scan_ids_resume_after_root_change_without_stale_bindings() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        for index in 0..140 {
            let common = base.join(format!("repository-{index:02}/.git"));
            fs::create_dir_all(common.join("objects")).unwrap();
            fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
            bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
        }

        let mut visited = 0;
        let partial = registered_gc_project_selection_with_progress(
            &roots,
            None,
            "0000000000000001",
            true,
            &mut || {
                visited += 1;
                visited <= 135
            },
        )
        .unwrap();
        assert!(partial.interrupted_at.is_some());
        assert!(partial.project_ids.is_empty());
        let resume = partial
            .continuation
            .as_deref()
            .expect("bounded scan saves an epoch sidecar");
        let tick_path = roots.state.as_path().join("tick");
        assert!(tick_path.join(resume).exists());

        let replaced_common = base.join("repository-00/.git");
        let replacement = base.join("repository-replaced/.git");
        fs::create_dir_all(replacement.join("objects")).unwrap();
        fs::write(replacement.join("HEAD"), "ref: refs/heads/main").unwrap();
        let project = binding_id(&replaced_common);
        let registry = existing_projects(&roots).unwrap().unwrap();
        let project_directory = registry
            .open_verified_directory(OsStr::new(&project.to_string()))
            .unwrap();
        let replacement_record = ProjectBindingRecord {
            schema_version: 1,
            project,
            canonical_common_dir: encode_location(&replacement),
            canonical_primary_checkout_hint: None,
            extra: Default::default(),
        };
        let bytes = provenant_records::encode(&replacement_record).unwrap();
        project_directory
            .atomic_write_with_permissions(
                OsStr::new(RECORD),
                ".project-",
                &bytes,
                0o600,
                PublishMode::Replace,
            )
            .unwrap()
            .require_durable()
            .unwrap();

        let new_common = base.join("repository-new/.git");
        fs::create_dir_all(new_common.join("objects")).unwrap();
        fs::write(new_common.join("HEAD"), "ref: refs/heads/main").unwrap();
        bind_project(&roots, ProjectLocation::new(&new_common).unwrap()).unwrap();

        let resumed = registered_gc_project_selection_with_progress(
            &roots,
            Some(resume),
            "0000000000000002",
            true,
            &mut || true,
        )
        .unwrap();
        assert!(resumed.interrupted_at.is_none());
        assert_eq!(resumed.project_ids.len(), 141);
        let bindings = resumed
            .project_ids
            .iter()
            .filter_map(
                |project| match lookup_gc_project(&roots, *project).unwrap() {
                    GcProjectStatus::Git(binding) => Some(binding),
                    GcProjectStatus::Transient(_) | GcProjectStatus::Excluded => None,
                },
            )
            .collect::<Vec<_>>();
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.common_dir() == replaced_common)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.common_dir() == new_common)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.common_dir() == replacement)
        );
        assert!(!roots.state.as_path().join("tick").join(resume).exists());
        assert!(
            !fs::read_dir(&tick_path)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains("gc-project-selection"))
        );
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn registry_id_scan_resumes_from_saved_directory_cookie() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        for index in 0..140 {
            let common = base.join(format!("project-{index:03}/.git"));
            fs::create_dir_all(common.join("objects")).unwrap();
            fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
            bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
        }

        let mut checks = 0;
        let partial = registered_gc_project_selection_with_progress(
            &roots,
            None,
            "0000000000000011",
            true,
            &mut || {
                checks += 1;
                checks <= 70
            },
        )
        .unwrap();
        assert!(partial.interrupted_at.is_some());
        assert!(partial.project_ids.is_empty());
        let resume = partial.continuation.as_deref().unwrap();

        let mut resumed_checks = 0;
        let resumed = registered_gc_project_selection_with_progress(
            &roots,
            Some(resume),
            "0000000000000012",
            true,
            &mut || {
                resumed_checks += 1;
                true
            },
        )
        .unwrap();
        assert!(resumed.interrupted_at.is_none());
        assert_eq!(resumed.project_ids.len(), 140);
        assert!(resumed_checks < 75);
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn resumed_project_ids_resolve_child_records_fresh() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let original = base.join("repository-original/.git");
        let other = base.join("repository-other/.git");
        for common in [&original, &other] {
            fs::create_dir_all(common.join("objects")).unwrap();
            fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        }
        bind_project(&roots, ProjectLocation::new(&original).unwrap()).unwrap();
        bind_project(&roots, ProjectLocation::new(&other).unwrap()).unwrap();
        let registry = existing_projects(&roots).unwrap().unwrap();
        let root_stamp = registry.metadata().unwrap();

        let mut checks = 0;
        let partial = registered_gc_project_selection_with_progress(
            &roots,
            None,
            "0000000000000021",
            true,
            &mut || {
                checks += 1;
                checks <= 1
            },
        )
        .unwrap();
        assert!(partial.interrupted_at.is_some());
        let resume = partial.continuation.as_deref().unwrap();
        let saved: GcSelectionSnapshot = serde_json::from_slice(
            &fs::read(roots.state.as_path().join("tick").join(resume)).unwrap(),
        )
        .unwrap();
        let changed_id = *saved.project_ids.iter().next().unwrap();
        assert_eq!(saved.project_ids.len(), 1);

        let replacement = base.join("repository-replacement/.git");
        fs::create_dir_all(replacement.join("objects")).unwrap();
        fs::write(replacement.join("HEAD"), "ref: refs/heads/main").unwrap();
        let project_directory = registry
            .open_verified_directory(OsStr::new(&changed_id.to_string()))
            .unwrap();
        let replacement_record = ProjectBindingRecord {
            schema_version: 1,
            project: changed_id,
            canonical_common_dir: encode_location(&replacement),
            canonical_primary_checkout_hint: None,
            extra: Default::default(),
        };
        let bytes = provenant_records::encode(&replacement_record).unwrap();
        project_directory
            .atomic_write_with_permissions(
                OsStr::new(RECORD),
                ".project-",
                &bytes,
                0o600,
                PublishMode::Replace,
            )
            .unwrap()
            .require_durable()
            .unwrap();
        let unchanged_root = registry.metadata().unwrap();
        assert_eq!(root_stamp.device, unchanged_root.device);
        assert_eq!(root_stamp.inode, unchanged_root.inode);
        assert_eq!(root_stamp.modified_ns, unchanged_root.modified_ns);

        let resumed = registered_gc_project_selection_with_progress(
            &roots,
            Some(resume),
            "0000000000000022",
            true,
            &mut || true,
        )
        .unwrap();
        assert_eq!(resumed.project_ids.len(), 2);
        let GcProjectStatus::Git(binding) = lookup_gc_project(&roots, changed_id).unwrap() else {
            panic!("current child record is a Git project")
        };
        assert_eq!(binding.common_dir(), replacement);
        assert_ne!(binding.common_dir(), original);
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn preview_registry_scan_does_not_publish_a_resume_sidecar() {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("repository/.git");
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();

        let partial = registered_gc_project_selection_with_progress(
            &roots,
            None,
            "0000000000000003",
            false,
            &mut || false,
        )
        .unwrap();
        assert!(partial.interrupted_at.is_some());
        assert!(partial.continuation.is_none());
        assert!(!roots.state.as_path().join("tick").exists());
    }

    // Competing no-replace publishers converge by validating the winning record.
    // Spec: docs/specs/state.md#configuration
    #[test]
    fn binding_creation_and_lookup_share_private_state_authority() {
        for mode in [0o700, 0o750, 0o755, 0o777] {
            let temp = tempdir();
            let roots = roots(&temp);
            let common = canonical_base(&temp).join("project/.git");
            fs::create_dir_all(&common).unwrap();
            let location = ProjectLocation::new(common).unwrap();
            fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(mode)).unwrap();
            let created = bind_project(&roots, location.clone());
            let found = lookup_project(&roots, &location);
            if mode & 0o077 == 0 {
                assert_eq!(found.unwrap(), Some(created.unwrap()));
            } else {
                assert!(matches!(
                    created,
                    Err(ConfigError::ProjectBindingDirectoryUnsafe {
                        mode: observed,
                        owner_matches: true,
                        ..
                    }) if observed == mode
                ));
                assert!(matches!(
                    found,
                    Err(ConfigError::ProjectBindingDirectoryUnsafe {
                        mode: observed,
                        owner_matches: true,
                        ..
                    }) if observed == mode
                ));
                assert_eq!(
                    fs::metadata(roots.state.as_path())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777,
                    mode
                );
                assert!(!roots.state.as_path().join(PROJECTS_DIR).exists());
            }
        }
    }

    #[test]
    fn concurrent_first_bindings_converge_on_one_atomic_record() {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("repo/.git");
        fs::create_dir_all(&common).unwrap();
        let location = ProjectLocation::new(&common).unwrap();
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let roots = &roots;
                    let location = location.clone();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        bind_project(roots, location)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(results.iter().all(|binding| binding == &results[0]));
        assert_eq!(
            registered_projects(&roots).unwrap(),
            vec![results[0].clone()]
        );
    }

    // Spec: docs/specs/state.md#configuration
    // Writers bind with a primary checkout hint, and a hint update replaces
    // the record by rename while others read it. No reader or binder may see
    // that replacement as invalid state.
    #[test]
    fn concurrent_first_bindings_with_hints_never_read_invalid_state() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let common = base.join("repo/.git");
        fs::create_dir_all(&common).unwrap();
        let checkouts = [base.join("repo"), base.join("moved")];
        let plain = ProjectLocation::new(&common).unwrap();
        let threads = 12;
        let rounds = 40;
        let barrier = std::sync::Barrier::new(threads);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|worker| {
                    let (roots, plain, barrier) = (&roots, &plain, &barrier);
                    let checkouts = &checkouts;
                    let common = &common;
                    scope.spawn(move || -> Result<Vec<ProjectId>, ConfigError> {
                        barrier.wait();
                        let mut seen = Vec::new();
                        for round in 0..rounds {
                            let binding = match (worker + round) % 4 {
                                // The hint moves every few rounds, so binders
                                // replace the record while others read it.
                                0 | 1 => bind_project(
                                    roots,
                                    ProjectLocation::from_verified_primary_checkout(
                                        common,
                                        &checkouts[(round / 4 + worker % 2) % 2],
                                    )?,
                                )?,
                                2 => bind_project(roots, plain.clone())?,
                                _ => match lookup_project(roots, plain)? {
                                    Some(binding) => binding,
                                    None => continue,
                                },
                            };
                            seen.push(binding.project_id());
                            for listed in registered_projects(roots)? {
                                seen.push(listed.project_id());
                            }
                        }
                        Ok(seen)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        let expected = bind_project(&roots, plain.clone()).unwrap().project_id();
        for result in results {
            let seen = result.expect("a concurrent binder or reader saw invalid state");
            assert!(seen.iter().all(|project| *project == expected));
        }
        assert_eq!(registered_projects(&roots).unwrap().len(), 1);
    }

    #[test]
    fn bind_project_assigns_distinct_ids_to_distinct_common_directories() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let first_location = base.join("one/.git");
        let second_location = base.join("two/.git");
        fs::create_dir_all(&first_location).unwrap();
        fs::create_dir_all(&second_location).unwrap();

        let first = bind_project(&roots, ProjectLocation::new(first_location).unwrap()).unwrap();
        let second = bind_project(&roots, ProjectLocation::new(second_location).unwrap()).unwrap();

        assert_ne!(first.project, second.project);
        fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(0o700)).unwrap();
        let listed = registered_projects(&roots).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&first));
        assert!(listed.contains(&second));
    }

    #[test]
    fn unrelated_duplicate_binding_does_not_block_lookup_or_bind() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let target_dir = base.join("target/.git");
        let duplicated_dir = base.join("duplicated/.git");
        let new_dir = base.join("new/.git");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&duplicated_dir).unwrap();
        fs::create_dir_all(&new_dir).unwrap();

        let target = bind_project(&roots, ProjectLocation::new(&target_dir).unwrap()).unwrap();
        let duplicated =
            bind_project(&roots, ProjectLocation::new(&duplicated_dir).unwrap()).unwrap();
        fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(0o700)).unwrap();
        let projects = roots.state.as_path().join(PROJECTS_DIR);
        let duplicate_id = ProjectId::from_bytes([11; 16]);
        let duplicate = projects.join(duplicate_id.to_string());
        fs::create_dir(&duplicate).unwrap();
        fs::set_permissions(&duplicate, fs::Permissions::from_mode(0o700)).unwrap();
        let record = ProjectBindingRecord {
            schema_version: 1,
            project: duplicate_id,
            canonical_common_dir: encode_location(&duplicated_dir),
            canonical_primary_checkout_hint: None,
            extra: Default::default(),
        };
        fs::write(
            duplicate.join(RECORD),
            provenant_records::encode(&record).unwrap(),
        )
        .unwrap();
        fs::set_permissions(duplicate.join(RECORD), fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(target.project_id(), duplicate_id);
        assert_ne!(duplicated.project_id(), duplicate_id);
        assert_eq!(registered_projects(&roots).unwrap().len(), 3);

        assert_eq!(
            lookup_project(&roots, &ProjectLocation::new(&target_dir).unwrap()).unwrap(),
            Some(target)
        );
        let new = bind_project(&roots, ProjectLocation::new(&new_dir).unwrap()).unwrap();
        assert_eq!(new.common_dir(), new_dir);
        assert_ne!(duplicated.project_id(), duplicate_id);

        let duplicate_target_id = ProjectId::from_bytes([12; 16]);
        let duplicate_target = projects.join(duplicate_target_id.to_string());
        fs::create_dir(&duplicate_target).unwrap();
        fs::set_permissions(&duplicate_target, fs::Permissions::from_mode(0o700)).unwrap();
        let record = ProjectBindingRecord {
            schema_version: 1,
            project: duplicate_target_id,
            canonical_common_dir: encode_location(&target_dir),
            canonical_primary_checkout_hint: None,
            extra: Default::default(),
        };
        fs::write(
            duplicate_target.join(RECORD),
            provenant_records::encode(&record).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            duplicate_target.join(RECORD),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let target_location = ProjectLocation::new(&target_dir).unwrap();
        assert!(lookup_project(&roots, &target_location).is_err());
        assert!(bind_project(&roots, target_location).is_err());
    }

    #[test]
    fn project_binding_record_preserves_unknown_fields_with_bounded_decode() {
        let key = format!("future_{}", "field");
        let project = ProjectId::from_bytes([24; 16]);
        let mut payload = serde_json::json!({
            "schema_version": 1,
            "project": project,
            "canonical_common_dir": "2f746d702f7265706f",
        });
        let unknown = serde_json::json!({"nested": [true, 7]});
        payload
            .as_object_mut()
            .unwrap()
            .insert(key.clone(), unknown.clone());
        let bytes = serde_json::to_vec(&payload).unwrap();
        let decoded = decode_binding_record(&bytes).unwrap();
        assert_eq!(decoded.extra.get(&key), Some(&unknown));

        let encoded = provenant_records::encode(&decoded).unwrap();
        let round_trip = decode_binding_record(&encoded).unwrap();
        assert_eq!(round_trip.extra.get(&key), Some(&unknown));
        assert_eq!(round_trip.project, project);
    }

    // Spec: docs/specs/run-git.md#project-layout-ownership
    #[test]
    fn verified_checkout_hint_is_additive_to_the_common_directory_identity() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let common = base.join("metadata/external.git");
        let checkout = base.join("checkout");
        fs::create_dir_all(&common).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        // Persistent project bindings are reserved for Git-backed locations.
        // This registry-layer test supplies the minimal common-dir marker;
        // Git ownership of the checkout hint is proved by confinement callers.
        fs::write(common.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::create_dir(common.join("objects")).unwrap();

        let before = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
        assert_eq!(before.primary_checkout_hint(), None);
        let updated = bind_project(
            &roots,
            ProjectLocation::from_verified_primary_checkout(&common, &checkout).unwrap(),
        )
        .unwrap();

        assert_eq!(updated.project_id(), before.project_id());
        assert_eq!(updated.common_dir(), before.common_dir());
        assert_eq!(updated.primary_checkout_hint(), Some(checkout.as_path()));
        assert_eq!(
            registered_projects(&roots).unwrap()[0].primary_checkout_hint(),
            Some(checkout.as_path())
        );
        assert_eq!(
            lookup_project(&roots, &ProjectLocation::new(&common).unwrap())
                .unwrap()
                .unwrap()
                .primary_checkout_hint(),
            Some(checkout.as_path())
        );
    }

    #[test]
    fn project_location_rejects_noncanonical_path_shapes() {
        for path in [
            PathBuf::from("relative"),
            PathBuf::from("/"),
            PathBuf::from("/a/../b"),
        ] {
            assert!(ProjectLocation::new(path).is_err());
        }
    }

    #[test]
    fn bind_project_ignores_a_stage_symlink_without_touching_its_target() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let projects = roots.state.as_path().join(PROJECTS_DIR);
        fs::create_dir(&projects).unwrap();
        fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();

        let outside = base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"outside contents").unwrap();
        let project = ProjectId::from_bytes([9; 16]);
        let stage = projects.join(format!("{STAGE_PREFIX}{project}"));
        std::os::unix::fs::symlink(&outside, &stage).unwrap();

        let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();

        assert!(bind_project(&roots, location).is_ok());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside contents");
        assert!(
            fs::symlink_metadata(&stage)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
