//! Host-local project identity indexed by the canonical Git common directory.
use crate::ConfigError;
use provenant_core::{ProjectId, Roots};
use provenant_os::fs::{Directory, FileKind, Metadata, PublishMode, effective_uid};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

const PROJECTS_DIR: &str = "projects";
const RECORD: &str = "project.json";
const STAGE_PREFIX: &str = ".project-stage-";
const RECORD_LIMIT: u64 = 4096;
const MAX_PROJECTS: usize = 16_384;

/// A canonical Git common directory, or the canonical directory for a
/// non-Git workspace. Discovery and canonicalisation belong to confinement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectLocation {
    pub canonical_common_dir: PathBuf,
}

impl ProjectLocation {
    pub fn new(canonical_common_dir: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let location = Self {
            canonical_common_dir: canonical_common_dir.into(),
        };
        validate_location(&location.canonical_common_dir)?;
        Ok(location)
    }

    pub fn common_dir(&self) -> &Path {
        &self.canonical_common_dir
    }
}

/// Persisted host-local identity for one canonical project location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectBinding {
    project: ProjectId,
    canonical_common_dir: PathBuf,
}

impl ProjectBinding {
    pub fn project_id(&self) -> ProjectId {
        self.project
    }

    pub fn common_dir(&self) -> &Path {
        &self.canonical_common_dir
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectBindingRecord {
    schema_version: u64,
    project: ProjectId,
    /// Lowercase hex of native absolute path bytes, keeping the record
    /// portable across non-UTF-8 Unix directory names.
    canonical_common_dir: String,
}

/// Find or allocate the host-local identity for this canonical location.
/// The path-derived ID makes concurrent first bindings converge on one record.
pub fn bind_project(
    roots: &Roots,
    location: ProjectLocation,
) -> Result<ProjectBinding, ConfigError> {
    validate_location(&location.canonical_common_dir)?;
    if transient_location(roots, location.common_dir()) {
        return transient_or_existing(roots, location);
    }

    let state = provenant_os::fs::Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, false)?;
    let projects = open_or_create_projects(&state)?;
    verify_directory(&projects, true)?;

    if let Some(binding) = find_binding(&projects, &location.canonical_common_dir)? {
        return Ok(binding);
    }

    create_binding(&projects, location.canonical_common_dir)
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
    let binding = match existing_projects_at_state(state)? {
        Some(projects) => find_binding(&projects, location.common_dir())?,
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
        Some(projects) => bindings(&projects),
        None => Ok(Vec::new()),
    }
}

/// GC has no authority over non-Git workspaces or transient roots. Keep old
/// identity records for indexed runs, but never enumerate them as GC projects.
pub fn registered_gc_projects(roots: &Roots) -> Result<Vec<ProjectBinding>, ConfigError> {
    Ok(registered_gc_project_selection(roots)?.projects)
}

/// Scheduled projects and transient registrations excluded from collection.
#[derive(Debug, Default)]
pub struct GcProjectSelection {
    pub projects: Vec<ProjectBinding>,
    /// Canonical common directories, preserving the registry's location.
    pub skipped_transient: Vec<PathBuf>,
}

pub fn registered_gc_project_selection(roots: &Roots) -> Result<GcProjectSelection, ConfigError> {
    let mut selection = GcProjectSelection::default();
    for binding in registered_projects(roots)? {
        if transient_location(roots, binding.common_dir()) {
            selection
                .skipped_transient
                .push(binding.common_dir().to_owned());
        } else if git_location(binding.common_dir()) {
            selection.projects.push(binding);
        }
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
    if let Some(projects) = existing_projects(roots)?
        && let Some(binding) = find_binding(&projects, location.common_dir())?
    {
        return Ok(binding);
    }
    Ok(transient_binding(location))
}

fn transient_binding(location: ProjectLocation) -> ProjectBinding {
    ProjectBinding {
        project: binding_id(location.common_dir()),
        canonical_common_dir: location.canonical_common_dir,
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

fn existing_projects_at_state(state: &Path) -> Result<Option<Directory>, ConfigError> {
    let state = match provenant_os::fs::Directory::open_anchor(state) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&state, true)?;
    let projects = match state.open_verified_directory(OsStr::new(PROJECTS_DIR)) {
        Ok(projects) => projects,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&projects, true)?;
    Ok(Some(projects))
}

fn find_binding(
    projects: &Directory,
    target: &Path,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let mut found = None;
    for binding in bindings(projects)? {
        if binding.common_dir() == target {
            if found.is_some() {
                return Err(ConfigError::InvalidProjectBinding);
            }
            found = Some(binding);
        }
    }
    Ok(found)
}

fn bindings(projects: &Directory) -> Result<Vec<ProjectBinding>, ConfigError> {
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
        verify_directory(&directory, true)?;
        let entry = match directory.metadata_at(OsStr::new(RECORD), false) {
            Ok(entry) => entry,
            // A concurrent first binding may have made the directory but not
            // yet atomically published its record. Its deterministic owner
            // can finish publication; readers leave it alone.
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        };
        let bytes = read_record(&directory, OsStr::new(RECORD), &entry, RECORD_LIMIT)?;
        let record: ProjectBindingRecord =
            serde_json::from_slice(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
        if record.schema_version != 1 || record.project != project {
            return Err(ConfigError::InvalidProjectBinding);
        }
        found.push(ProjectBinding {
            project,
            canonical_common_dir: decode_location(&record.canonical_common_dir)?,
        });
    }
    Ok(found)
}

fn create_binding(
    projects: &Directory,
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
    verify_directory(&directory, true)?;
    let record = ProjectBindingRecord {
        schema_version: 1,
        project,
        canonical_common_dir: encode_location(&canonical_common_dir),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| ConfigError::InvalidProjectBinding)?;
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
    let entry = directory.metadata_at(OsStr::new(RECORD), false)?;
    let stored = serde_json::from_slice::<ProjectBindingRecord>(&read_record(
        &directory,
        OsStr::new(RECORD),
        &entry,
        RECORD_LIMIT,
    )?)
    .map_err(|_| ConfigError::InvalidProjectBinding)?;
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
    })
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

fn verify_directory(directory: &Directory, private: bool) -> Result<(), ConfigError> {
    let metadata = directory.metadata()?;
    if metadata.kind != FileKind::Directory
        || metadata.uid != effective_uid()
        || metadata.mode & 0o022 != 0
        || (private && metadata.mode & 0o077 != 0)
    {
        return Err(ConfigError::InvalidProjectBinding);
    }
    Ok(())
}

fn read_record(
    directory: &Directory,
    name: &OsStr,
    entry: &Metadata,
    limit: u64,
) -> Result<Vec<u8>, ConfigError> {
    if entry.kind != FileKind::Regular || entry.links != 1 || entry.size > limit {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let bytes = directory
        .read_stable(name, limit)
        .map_err(|error| match error {
            provenant_os::fs::FsError::RaceRefused { .. }
            | provenant_os::fs::FsError::TooLarge { .. }
            | provenant_os::fs::FsError::ReadIo { .. } => ConfigError::InvalidProjectBinding,
            other => other.into(),
        })?;
    if !entry.unchanged(&directory.metadata_at(name, false)?) {
        return Err(ConfigError::InvalidProjectBinding);
    }
    Ok(bytes)
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
    verify_directory(&state, true)?;
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
    verify_directory(&state, true)?;
    read_host(&state, OsStr::new("host-id"))
}

fn read_host(
    state: &Directory,
    name: &OsStr,
) -> Result<Option<provenant_core::HostId>, ConfigError> {
    match state.metadata_at(name, false) {
        Ok(meta) => {
            if meta.kind != FileKind::Regular
                || meta.links != 1
                || meta.uid != effective_uid()
                || meta.mode & 0o077 != 0
                || meta.size != 32
            {
                return Err(ConfigError::InvalidProjectBinding);
            }
            let bytes = read_record(state, name, &meta, 32)?;
            if bytes.len() != 32 {
                return Err(ConfigError::InvalidProjectBinding);
            }
            let text =
                std::str::from_utf8(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
            text.parse()
                .map(Some)
                .map_err(|_| ConfigError::InvalidProjectBinding)
        }
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{RootInputs, Roots};

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn canonical_base(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().canonicalize().unwrap()
    }

    fn roots(temp: &tempfile::TempDir) -> Roots {
        let base = canonical_base(temp);
        let home = base.join("home");
        let state = base.join("state");
        let checkout = base.join("checkout");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(state);
        inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
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
        assert_eq!(registered_gc_projects(&roots).unwrap(), vec![binding]);
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
            create_binding(&projects, common.clone()).unwrap();
            excluded.push(common);
        }
        // Existing identity records are preserved for indexed run evidence.
        assert_eq!(registered_projects(&roots).unwrap().len(), 4);
        assert_eq!(registered_gc_projects(&roots).unwrap(), vec![good.clone()]);
        let mut selection = registered_gc_project_selection(&roots).unwrap();
        assert_eq!(selection.projects, vec![good]);
        selection.skipped_transient.sort();
        excluded.sort();
        assert_eq!(selection.skipped_transient, excluded);
    }

    #[test]
    fn bind_project_reuses_identity_for_the_same_canonical_common_dir() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();

        let first = bind_project(&roots, location.clone()).unwrap();
        let second = bind_project(&roots, location).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.canonical_common_dir, base.join("repository/.git"));
    }

    // Competing no-replace publishers converge by validating the winning record.
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
        };
        fs::write(duplicate.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
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
        };
        fs::write(
            duplicate_target.join(RECORD),
            serde_json::to_vec(&record).unwrap(),
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
    fn bind_project_leaves_unclaimed_stage_contents_untouched() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let projects = roots.state.as_path().join(PROJECTS_DIR);
        fs::create_dir(&projects).unwrap();
        fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();

        let project = ProjectId::from_bytes([8; 16]);
        let stage = projects.join(format!("{STAGE_PREFIX}{project}"));
        fs::create_dir(&stage).unwrap();
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(stage.join(RECORD), b"partial record").unwrap();
        fs::set_permissions(stage.join(RECORD), fs::Permissions::from_mode(0o600)).unwrap();
        let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();
        bind_project(&roots, location).unwrap();

        assert_eq!(fs::read(stage.join(RECORD)).unwrap(), b"partial record");
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
