//! Host-local project identity indexed by the canonical Git common directory.
use crate::ConfigError;
use provenant_core::{Clock, DurationMicros, ProjectId, Roots, WaitBudget, WallTime};
use provenant_os::fs::{
    Directory, DirectoryPosition, FileKind, LockMode, Metadata, PublishMode, effective_uid,
};
use provenant_records::warning::{PROJECT_DIRECTORY_IGNORED, Warning, warn};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

/// One scheduler's classification of a registered project location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectPresence {
    Present,
    Absent { since: WallTime },
    Unreachable { error: String, retry_at: WallTime },
}

/// Classify one metadata probe without turning permission and I/O failures
/// into a false `Present` result. Callers retain incarnation timing and choose
/// the bounded retry time.
pub fn classify_project_presence(
    metadata: std::io::Result<std::fs::Metadata>,
    absent_since: WallTime,
    retry_at: WallTime,
) -> ProjectPresence {
    match metadata {
        Ok(_) => ProjectPresence::Present,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            ProjectPresence::Absent {
                since: absent_since,
            }
        }
        Err(error) => ProjectPresence::Unreachable {
            error: error.to_string(),
            retry_at,
        },
    }
}

/// The stable identity of one project registration.
///
/// A condition about a project's checkout is reported once per incarnation:
/// this key changes only when the registration record is replaced, so a
/// transient permission error is not reported on every tick, but a recreated
/// registration is reported again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectIncarnation {
    /// Device of the host-project registration record.
    pub device: u64,
    /// Inode of the host-project registration record.
    pub inode: u64,
    /// Registration time of that record, in nanoseconds.
    pub registered_ns: i128,
}

impl ProjectIncarnation {
    /// Stable dedup key for a once-per-incarnation warning.
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.device, self.inode, self.registered_ns)
    }
}

/// How long an unreachable checkout is skipped before it is probed again. The
/// retry is bounded so a restored checkout is rediscovered promptly while a
/// persistently unreadable one is not re-probed on every tick.
pub const PROJECT_RETRY_SECONDS: u64 = 60;

/// The single presence classification settlement, forge and graph scheduling
/// read for a registered project.
///
/// A permission or I/O failure is `Unreachable` with a bounded retry time,
/// never a false `Present`. The returned incarnation identifies this
/// registration so callers report the condition once per incarnation.
pub fn project_presence(
    roots: &Roots,
    binding: &ProjectBinding,
    now: WallTime,
    retry_after: DurationMicros,
) -> Result<(ProjectPresence, ProjectIncarnation), ConfigError> {
    let state = Directory::open_anchor(roots.state.as_path())?;
    let projects = state.open_verified_directory(OsStr::new(PROJECTS_DIR))?;
    let name = OsString::from(binding.project_id().to_string());
    let directory = projects.open_verified_directory(&name)?;
    let registration = directory.metadata_at(OsStr::new(RECORD), false)?;
    let incarnation = ProjectIncarnation {
        device: registration.device,
        inode: registration.inode,
        registered_ns: registration.modified_ns,
    };
    let presence = classify_project_presence(
        std::fs::metadata(binding.common_dir()),
        now,
        now.checked_add(retry_after).unwrap_or(WallTime::MAX),
    );
    Ok((presence, incarnation))
}

pub(crate) const PROJECTS_DIR: &str = "projects";
pub(crate) const RECORD: &str = "project.json";
const STAGE_PREFIX: &str = ".project-stage-";
const RECORD_LIMIT: u64 = 20 * 1024;
const MAX_PROJECTS: usize = 16_384;
const GC_SELECTION_SNAPSHOT_PREFIX: &str = "gc-project-selection-";
const GC_SELECTION_SNAPSHOT_VERSION: u64 = 4;
const GC_SELECTION_SNAPSHOT_LIMIT: u64 = 1024 * 1024;
const MAX_GC_SELECTION_SNAPSHOTS: usize = 64;
/// Reads of one name that may each meet a concurrent atomic replacement.
const REPLACEMENT_READS: usize = 16;
/// Serialises record publication, replacement and removal in a binding directory.
const BINDING_LOCK: &str = "project-binding.lock";

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

    /// Whether the bound common directory still exists. A vanished location
    /// protects nothing, cannot be indexed or settled, and is skipped rather
    /// than treated as an inventory error. An entry that exists but cannot be
    /// read is present: callers protect it rather than skipping it.
    pub fn common_dir_present(&self) -> bool {
        !absent(&self.canonical_common_dir)
    }

    /// Whether the verified primary checkout still exists. A temporary
    /// checkout can vanish while its separate Git common directory survives;
    /// the binding then has no project root to settle or poll. A binding with
    /// no checkout hint (a bare or common-only project) has no checkout to
    /// have vanished and is always present by this check. An entry that exists
    /// but cannot be read is present: callers protect it rather than skipping.
    pub fn primary_checkout_present(&self) -> bool {
        self.canonical_primary_checkout_hint
            .as_deref()
            .is_none_or(|root| !absent(root))
    }
}

/// A path is absent only when the filesystem proves it is gone. A permission
/// or I/O failure leaves presence unknown, so callers protect the entry rather
/// than treating it as vanished.
fn absent(path: &Path) -> bool {
    matches!(
        std::fs::metadata(path),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            )
    )
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

/// The deterministic project ID for a checkout path, derived from its
/// canonical Git common directory. `None` when the path has no usable marker.
pub fn project_id_for_path(path: &Path) -> Option<ProjectId> {
    let marker = provenant_os::git::repository_marker(path)?;
    let common_dir = marker.common_dir?;
    Some(binding_id(&std::fs::canonicalize(common_dir).ok()?))
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

/// Lookup only: the published binding for a project id the caller already
/// holds, such as the id a lane's launch declared. Absent when no record is
/// published; a record that names another project is invalid.
pub fn lookup_project_by_id(
    state: &Path,
    project: ProjectId,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let Some(projects) = existing_projects_at_state(state)? else {
        return Ok(None);
    };
    let name = OsString::from(project.to_string());
    let directory = match projects.open_verified_directory(&name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&directory, &project_binding_dir(state, project))?;
    let Some(bytes) = read_binding_record(&directory)? else {
        return Ok(None);
    };
    let record = decode_binding_record(&bytes)?;
    if record.project != project {
        return Err(ConfigError::InvalidProjectBinding);
    }
    binding_from_record(record).map(Some)
}

/// Remove one binding record so the host stops treating the project as
/// registered. The private host-project directory and its settings stay, so a
/// later bind of the same common directory reuses them; scheduled collection
/// treats a directory without a record as absent. Returns whether a record was
/// removed; a missing binding or an unpublished stage is not an error.
pub fn forget_project(roots: &Roots, project: ProjectId) -> Result<bool, ConfigError> {
    forget_project_at_state(roots.state.as_path(), project)
}

/// Forget a binding named by a state root owned by a caller that has no
/// `Roots`, such as the verify edge cleaning up its scratch project.
pub fn forget_project_at_state(state: &Path, project: ProjectId) -> Result<bool, ConfigError> {
    let Some(projects) = existing_projects_at_state(state)? else {
        return Ok(false);
    };
    let registry_path = state.join(PROJECTS_DIR);
    let name = OsString::from(project.to_string());
    let directory = match projects.open_verified_directory(&name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&directory, &registry_path.join(&name))?;
    if read_binding_record(&directory)?.is_none() {
        return Ok(false);
    }
    // Serialise removal with hint replacement; both publish the one record.
    let clock = provenant_os::clock::SystemClock::new();
    let budget =
        WaitBudget::cli(clock.mono_now(), 5).map_err(|_| ConfigError::InvalidProjectBinding)?;
    let _lock = directory.lock(
        OsStr::new(BINDING_LOCK),
        LockMode::Exclusive,
        budget,
        &provenant_core::CancelToken::new(),
        &clock,
    )?;
    let Some(bytes) = read_record(&directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)? else {
        return Ok(false);
    };
    let record = decode_binding_record(&bytes)?;
    if record.schema_version != 1 || record.project != project {
        return Err(ConfigError::InvalidProjectBinding);
    }
    match directory.unlink(OsStr::new(RECORD)) {
        Ok(()) => {}
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    directory.sync()?;
    Ok(true)
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
    /// The binding's project root is gone: either its common directory or its
    /// verified primary checkout has vanished. `record_modified_ns` dates the
    /// last host record of the project, the only age evidence left once the
    /// root itself is unavailable.
    Vanished {
        binding: ProjectBinding,
        record_modified_ns: i128,
    },
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
    let Some((bytes, metadata)) = read_binding_record_with_metadata(&directory)? else {
        return Ok(GcProjectStatus::Excluded);
    };
    let record = decode_binding_record(&bytes)?;
    if record.schema_version != 1 || record.project != project {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let binding = binding_from_record(record)?;
    if !binding.common_dir_present() || !binding.primary_checkout_present() {
        Ok(GcProjectStatus::Vanished {
            binding,
            record_modified_ns: metadata.modified_ns,
        })
    } else if transient_location(roots, binding.common_dir()) {
        Ok(GcProjectStatus::Transient(binding.common_dir().to_owned()))
    } else if git_location(binding.common_dir()) {
        Ok(GcProjectStatus::Git(binding))
    } else {
        Ok(GcProjectStatus::Excluded)
    }
}

/// Retirement of bindings whose project root is gone. A preview names what
/// apply would retire. Per-binding read or removal failures are warnings:
/// one unreadable registration never aborts the pass, which fails only when the
/// registry itself cannot be enumerated.
#[derive(Debug, Default)]
pub struct Retirement {
    pub retired: Vec<ProjectBinding>,
    pub warnings: Vec<String>,
}

/// Retire bindings whose common directory or verified primary checkout is
/// gone and whose record has not changed for `idle_seconds`. A vanished root
/// leaves the record's last modification as the only age evidence; preview
/// reports without removing.
pub fn retire_vanished_projects(
    roots: &Roots,
    idle_seconds: u64,
    dry_run: bool,
) -> Result<Retirement, ConfigError> {
    let now_ns = i128::from(
        provenant_os::clock::SystemClock::new()
            .try_unix_millis()
            .map_err(|_| ConfigError::InvalidProjectBinding)?,
    ) * 1_000_000;
    let idle_ns = i128::from(idle_seconds) * 1_000_000_000;
    let selection = registered_gc_project_selection(roots)?;
    let mut retirement = Retirement::default();
    for project in selection.project_ids {
        let status = match lookup_gc_project(roots, project) {
            Ok(status) => status,
            Err(error) => {
                retirement.warnings.push(format!(
                    "project {} lookup failed while retiring vanished bindings: {error}",
                    project_binding_dir(roots.state.as_path(), project).display()
                ));
                continue;
            }
        };
        let GcProjectStatus::Vanished {
            binding,
            record_modified_ns,
        } = status
        else {
            continue;
        };
        if now_ns.saturating_sub(record_modified_ns) < idle_ns
            || (binding.common_dir_present() && binding.primary_checkout_present())
        {
            continue;
        }
        if !dry_run {
            match forget_project(roots, project) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    retirement.warnings.push(format!(
                        "project {} could not be retired: {error}",
                        project_binding_dir(roots.state.as_path(), project).display()
                    ));
                    continue;
                }
            }
        }
        retirement.retired.push(binding);
    }
    Ok(retirement)
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

/// Enumerate registered project IDs in resumable batches. A directory without
/// a published record is absent, so a retired binding is not selected. Paths
/// are resolved from their current records by callers immediately before use.
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
                tick.as_ref(),
                persist,
                epoch,
                registry_path.join(&entry),
            );
            return Ok(selection);
        }
        snapshot.offset = entries.position()?;
        snapshot.next_key = None;
        let Some(name) = entry.to_str() else {
            warn_unrecognised_project(&registry_path.join(&entry));
            continue;
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
        // Keep the bounded scan resilient to stray entries, matching bindings().
        let Ok(project) = name.parse::<ProjectId>() else {
            warn_unrecognised_project(&registry_path.join(&entry));
            continue;
        };
        // A directory without a record is what a forgotten binding leaves
        // behind. It is absent to collection: selecting its ID would make a
        // retired project look like live custody.
        if !project_directory_is_registered(&registry, &entry) {
            continue;
        }
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
            tick.as_ref(),
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

/// The host project directory for a binding. This is the single owner of the
/// `state/projects/<id>` layout: host-project ports.json and config.toml
/// readers and writers all resolve their directory here.
pub fn project_binding_dir(state: &Path, project: ProjectId) -> PathBuf {
    state.join(PROJECTS_DIR).join(project.to_string())
}

/// The host project directory for a checkout, named by its ProjectId binding
/// rather than the 64-hex target-seed key. The id is derived from the canonical
/// common Git dir, so linked checkouts share one directory. Returns `None`
/// when the path has no usable Git marker or common dir.
pub fn host_project_dir(state: &Path, project: &Path) -> Option<PathBuf> {
    let marker = provenant_os::git::repository_marker(project)?;
    let common_dir = marker.common_dir?;
    let canonical = std::fs::canonicalize(common_dir).ok()?;
    Some(project_binding_dir(state, binding_id(&canonical)))
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

/// A directory that exists but has no published record. A forgotten or
/// retired binding leaves its host-project directory in place so a later bind
/// reuses the settings; collection must treat that directory as absent.
/// Anything that exists but cannot be inspected stays selected, so the caller
/// marks a genuinely unreadable registration unobserved rather than skipping
/// it.
fn project_directory_is_registered(registry: &Directory, entry: &OsStr) -> bool {
    match registry.open_verified_directory(entry) {
        Ok(directory) => match directory.metadata_at(OsStr::new(RECORD), false) {
            Ok(_) => true,
            Err(error) => !error.is_not_found(),
        },
        Err(error) => !error.is_not_found(),
    }
}

/// One clear warning for an entry that cannot be a binding directory. The
/// path is named so an operator can move or remove the stray entry.
fn warn_unrecognised_project(path: &Path) {
    warn(
        Warning::new(
            PROJECT_DIRECTORY_IGNORED,
            format!(
                "ignoring unrecognised project directory at {}; it is not a project binding",
                path.display()
            ),
            provenant_records::warning::detected_source(PROJECT_DIRECTORY_IGNORED),
        )
        .subject(path.display().to_string())
        .repair("move or remove the stray entry from the projects registry")
        .once_per_change(),
    );
}

fn bindings(
    projects: &Directory,
    projects_path: &Path,
) -> Result<Vec<ProjectBinding>, ConfigError> {
    let mut count = 0;
    let mut found = Vec::new();
    for entry in projects.entries()? {
        let entry = entry?;
        let path = projects_path.join(&entry);
        let Some(name) = entry.to_str() else {
            warn_unrecognised_project(&path);
            continue;
        };
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
        // A stray directory (for example one made from an obsolete keyed
        // ports path) is not a binding. Warn and skip it; only a recognised
        // binding with a corrupt record refuses the whole inventory.
        let Ok(project) = name.parse::<ProjectId>() else {
            warn_unrecognised_project(&path);
            continue;
        };
        let directory = projects.open_verified_directory(&entry)?;
        verify_directory(&directory, &projects_path.join(name))?;
        // A concurrent first binding may have made the directory but not yet
        // atomically published its record. Its deterministic owner can finish
        // publication; readers leave it alone.
        let Some(bytes) = read_binding_record(&directory)? else {
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
        OsStr::new(BINDING_LOCK),
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
    // A first publisher can race both hint replacement and removal as soon as
    // its record is visible. Hold the same lock through publication and readback.
    let clock = provenant_os::clock::SystemClock::new();
    let budget =
        WaitBudget::cli(clock.mono_now(), 5).map_err(|_| ConfigError::InvalidProjectBinding)?;
    let _lock = directory.lock(
        OsStr::new(BINDING_LOCK),
        LockMode::Exclusive,
        budget,
        &provenant_core::CancelToken::new(),
        &clock,
    )?;
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

pub(crate) fn verify_directory(directory: &Directory, path: &Path) -> Result<(), ConfigError> {
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

/// A lockless binding read. A burst of hint updates can replace the record
/// during every optimistic read; that is contention, not invalid state, so
/// read once more under the writers' lock in shared mode. Replacement takes
/// that lock, so it already exists then and a lookup creates no state.
fn read_binding_record(directory: &Directory) -> Result<Option<Vec<u8>>, ConfigError> {
    Ok(read_binding_record_with_metadata(directory)?.map(|(bytes, _)| bytes))
}

fn read_binding_record_with_metadata(
    directory: &Directory,
) -> Result<Option<(Vec<u8>, Metadata)>, ConfigError> {
    match read_record_attempts(directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)? {
        RecordRead::Absent => Ok(None),
        RecordRead::Read(bytes, metadata) => Ok(Some((bytes, metadata))),
        RecordRead::KeptChanging => {
            let clock = provenant_os::clock::SystemClock::new();
            let budget = WaitBudget::cli(clock.mono_now(), 5)
                .map_err(|_| ConfigError::InvalidProjectBinding)?;
            let _shared = directory.lock_existing(
                OsStr::new(BINDING_LOCK),
                LockMode::Shared,
                budget,
                &provenant_core::CancelToken::new(),
                &clock,
            )?;
            read_record_with_metadata(directory, OsStr::new(RECORD), RECORD_LIMIT, |_| true)
        }
    }
}

/// A binding record that must already be published in this directory. Its
/// callers hold the binding lock, so they never need the shared-lock read.
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
    match read_record_attempts(directory, name, limit, accept)? {
        RecordRead::Absent => Ok(None),
        RecordRead::Read(bytes, metadata) => Ok(Some((bytes, metadata))),
        RecordRead::KeptChanging => Err(ConfigError::InvalidProjectBinding),
    }
}

enum RecordRead {
    Absent,
    Read(Vec<u8>, Metadata),
    /// Every optimistic read met a replacement.
    KeptChanging,
}

fn read_record_attempts(
    directory: &Directory,
    name: &OsStr,
    limit: u64,
    accept: impl Fn(&Metadata) -> bool,
) -> Result<RecordRead, ConfigError> {
    for _ in 0..REPLACEMENT_READS {
        let entry = match directory.metadata_at(name, false) {
            Ok(entry) => entry,
            Err(error) if error.is_not_found() => return Ok(RecordRead::Absent),
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
            Ok(after) if entry.unchanged(&after) => return Ok(RecordRead::Read(bytes, after)),
            Ok(_) => continue,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(RecordRead::KeptChanging)
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
mod tests;
