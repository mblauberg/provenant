//! Host-local project identity indexed by the canonical Git common directory.
use crate::ConfigError;
use provenant_core::{CancelToken, Clock, ProjectId, Roots, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, FileKind, LockMode, Metadata, PublishMode, effective_uid, entry_names},
};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

const PROJECTS_DIR: &str = "projects";
const BINDING_LOCK: &str = "project-binding.lock";
const RECORD: &str = "project.json";
const STAGE_PREFIX: &str = ".project-stage-";
const RECORD_LIMIT: u64 = 4096;
const MAX_PROJECTS: usize = 16_384;
const LOCK_WAIT_SECONDS: u64 = 60;

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

/// Find or allocate the host-local identity while holding the project-binding
/// lock. A malformed record aborts the scan rather than allowing a second ID
/// to be assigned to a location whose existing binding cannot be read.
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

    let clock = SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), LOCK_WAIT_SECONDS)
        .map_err(|_| ConfigError::ProjectBindingBudget)?;
    let cancel = CancelToken::new();
    let _lock = projects.lock(
        OsStr::new(BINDING_LOCK),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;

    remove_stale_stages(&projects)?;

    if let Some(binding) = find_binding(&projects, &location.canonical_common_dir)? {
        return Ok(binding);
    }

    create_binding(&projects, location.canonical_common_dir)
}

fn remove_stale_stages(projects: &Directory) -> Result<(), ConfigError> {
    let mut stages = Vec::new();
    for entry in projects.entries()? {
        let entry = entry?;
        if let Some(name) = entry.to_str()
            && let Some(project) = name.strip_prefix(STAGE_PREFIX)
        {
            let project =
                ProjectId::parse(project).map_err(|_| ConfigError::InvalidProjectBinding)?;
            if format!("{STAGE_PREFIX}{project}") != name {
                return Err(ConfigError::InvalidProjectBinding);
            }
            stages.push(entry);
            if stages.len() > MAX_PROJECTS {
                return Err(ConfigError::InvalidProjectBinding);
            }
        }
    }
    for stage in stages {
        remove_stale_stage(projects, &stage)?;
    }
    Ok(())
}

fn remove_stale_stage(projects: &Directory, stage: &OsStr) -> Result<(), ConfigError> {
    let directory = projects.open_verified_directory(stage)?;
    verify_directory(&directory, true)?;
    for entry in entry_names(&directory, MAX_PROJECTS)? {
        if entry != OsStr::new(RECORD) && !is_staging_record(&entry) {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let metadata = directory.metadata_at(&entry, false)?;
        if metadata.kind != FileKind::Regular
            || metadata.uid != effective_uid()
            || metadata.links != 1
            || metadata.mode & 0o077 != 0
        {
            return Err(ConfigError::InvalidProjectBinding);
        }
        directory.unlink(&entry)?;
    }
    directory.sync()?;
    let opened = directory.metadata()?;
    let named = projects.metadata_at(stage, false)?;
    if !opened.same_identity(&named) {
        return Err(ConfigError::InvalidProjectBinding);
    }
    projects.rmdir(stage)?;
    projects.sync()?;
    Ok(())
}

fn is_staging_record(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(random) = name
        .strip_prefix(".project-.provenant-staging.")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    random.len() == 24
        && random
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Lookup only: hooks never allocate identities or create state.
pub fn lookup_project(
    roots: &Roots,
    location: &ProjectLocation,
) -> Result<Option<ProjectBinding>, ConfigError> {
    if transient_location(roots, location.common_dir()) {
        return transient_or_existing(roots, location.clone()).map(Some);
    }
    match existing_projects(roots)? {
        Some(projects) => find_binding(&projects, location.common_dir()),
        None => Ok(None),
    }
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
    Ok(registered_projects(roots)?
        .into_iter()
        .filter(|binding| {
            !transient_location(roots, binding.common_dir()) && git_location(binding.common_dir())
        })
        .collect())
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

fn transient_location(roots: &Roots, path: &Path) -> bool {
    let state = roots.state.as_path();
    if path.starts_with(state.join("runs")) || path.starts_with(state.join("children")) {
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
    let digest =
        provenant_records::CanonicalV1::file_digest(location.common_dir().as_os_str().as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&digest.bytes()[..16]);
    ProjectBinding {
        project: ProjectId::from_bytes(id),
        canonical_common_dir: location.canonical_common_dir,
    }
}

fn existing_projects(roots: &Roots) -> Result<Option<Directory>, ConfigError> {
    let state = match provenant_os::fs::Directory::open_anchor(roots.state.as_path()) {
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
        if entry == OsStr::new(BINDING_LOCK) {
            continue;
        }
        count += 1;
        if count > MAX_PROJECTS {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let name = entry.to_str().ok_or(ConfigError::InvalidProjectBinding)?;
        // In-progress binding publication is not a project record. Readers
        // must not repair it or block unrelated registered projects.
        if let Some(id) = name.strip_prefix(STAGE_PREFIX)
            && id
                .parse::<ProjectId>()
                .is_ok_and(|project| format!("{STAGE_PREFIX}{project}") == name)
        {
            continue;
        }
        let project = name
            .parse::<ProjectId>()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        let directory = projects.open_verified_directory(&entry)?;
        verify_directory(&directory, true)?;
        let entry = directory.metadata_at(OsStr::new(RECORD), false)?;
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
    for _ in 0..16 {
        let project = ProjectId::from_bytes(provenant_os::fs::entropy_128()?);
        let name = OsString::from(project.to_string());
        let stage = OsString::from(format!(".project-stage-{project}"));
        match projects.mkdir(&stage, 0o700) {
            Ok(()) => projects.sync()?,
            Err(error) if error.is_exists() => continue,
            Err(error) => return Err(error.into()),
        }
        let directory = projects.open_verified_directory(&stage)?;
        verify_directory(&directory, true)?;
        let record = ProjectBindingRecord {
            schema_version: 1,
            project,
            canonical_common_dir: encode_location(&canonical_common_dir),
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| ConfigError::InvalidProjectBinding)?;
        directory
            .atomic_write_with_permissions(
                OsStr::new(RECORD),
                ".project-",
                &bytes,
                0o600,
                PublishMode::NoReplace,
            )?
            .require_durable()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        match projects.rename_noreplace(&stage, projects, &name) {
            Ok(()) => {}
            Err(error) if error.is_exists() => continue,
            Err(error) => return Err(error.into()),
        }
        projects.sync()?;
        return Ok(ProjectBinding {
            project,
            canonical_common_dir,
        });
    }
    Err(ConfigError::InvalidProjectBinding)
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

/// Initialise the host's random identity once, using the same safe state
/// anchor and locking contract as project bindings. Never reads legacy roots.
pub fn bind_host(roots: &Roots) -> Result<provenant_core::HostId, ConfigError> {
    let state = provenant_os::fs::Directory::open_anchor(roots.state.as_path())?;
    verify_directory(&state, true)?;
    let clock = SystemClock::new();
    let budget = WaitBudget::cli(clock.mono_now(), LOCK_WAIT_SECONDS)
        .map_err(|_| ConfigError::ProjectBindingBudget)?;
    let cancel = CancelToken::new();
    let _lock = state.lock(
        OsStr::new("host-binding.lock"),
        LockMode::Exclusive,
        budget,
        &cancel,
        &clock,
    )?;
    let name = OsStr::new("host-id");
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
            let bytes = read_record(&state, name, &meta, 32)?;
            if bytes.len() != 32 {
                return Err(ConfigError::InvalidProjectBinding);
            }
            let text =
                std::str::from_utf8(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
            text.parse().map_err(|_| ConfigError::InvalidProjectBinding)
        }
        Err(error) if error.is_not_found() => {
            let id = provenant_core::HostId::from_bytes(provenant_os::fs::entropy_128()?);
            state
                .atomic_write_with_permissions(
                    name,
                    ".host-",
                    id.to_string().as_bytes(),
                    0o600,
                    PublishMode::NoReplace,
                )?
                .require_durable()
                .map_err(|_| ConfigError::InvalidProjectBinding)?;
            Ok(id)
        }
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
            roots.state.as_path().join("runs/run"),
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
        // Existing identity records are preserved for indexed run evidence.
        assert_eq!(registered_projects(&roots).unwrap().len(), 2);
        assert_eq!(registered_gc_projects(&roots).unwrap(), vec![good]);
    }

    #[test]
    fn bind_project_reuses_random_identity_for_the_same_canonical_common_dir() {
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
    fn bind_project_removes_only_owned_stale_stage_contents() {
        let temp = tempdir();
        let roots = roots(&temp);
        let base = canonical_base(&temp);
        let projects = roots.state.as_path().join(PROJECTS_DIR);
        fs::create_dir(&projects).unwrap();
        fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();

        let project = ProjectId::from_bytes([8; 16]);
        let stage = projects.join(format!(".project-stage-{project}"));
        fs::create_dir(&stage).unwrap();
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(stage.join(RECORD), b"partial record").unwrap();
        fs::set_permissions(stage.join(RECORD), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            stage.join(".project-.provenant-staging.aaaaaaaaaaaaaaaaaaaaaaaa.tmp"),
            b"partial staging file",
        )
        .unwrap();
        fs::set_permissions(
            stage.join(".project-.provenant-staging.aaaaaaaaaaaaaaaaaaaaaaaa.tmp"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        for index in 0..1024 {
            let name = format!(".project-.provenant-staging.{index:024x}.tmp");
            let file = stage.join(name);
            fs::write(&file, b"partial staging file").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();
        bind_project(&roots, location).unwrap();

        assert!(!stage.exists());
    }

    #[test]
    fn bind_project_refuses_a_stale_stage_symlink_without_touching_its_target() {
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
        let stage = projects.join(format!(".project-stage-{project}"));
        std::os::unix::fs::symlink(&outside, &stage).unwrap();

        let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();

        assert!(bind_project(&roots, location).is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside contents");
        assert!(
            fs::symlink_metadata(&stage)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
