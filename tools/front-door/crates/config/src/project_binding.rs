//! Host-local project identity indexed by the canonical Git common directory.
use crate::ConfigError;
use provenant_core::{CancelToken, Clock, ProjectId, Roots, WaitBudget};
use provenant_supervisor::clock::SystemClock;
use provenant_supervisor::fs::{
    FileKind, LockMode, Metadata, PublishMode, SafeDir, SafeFs, effective_uid,
};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::Read;
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
    fs: &dyn SafeFs,
) -> Result<ProjectBinding, ConfigError> {
    validate_location(&location.canonical_common_dir)?;

    let state = fs.open_anchor(roots.state.as_path())?;
    verify_directory(&*state, false)?;
    let projects = open_or_create_projects(&*state)?;
    verify_directory(&*projects, true)?;

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

    remove_stale_stages(&*projects)?;

    if let Some(binding) = find_binding(&*projects, &location.canonical_common_dir)? {
        return Ok(binding);
    }

    create_binding(&*projects, location.canonical_common_dir, fs)
}

fn remove_stale_stages(projects: &dyn SafeDir) -> Result<(), ConfigError> {
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

fn remove_stale_stage(projects: &dyn SafeDir, stage: &OsStr) -> Result<(), ConfigError> {
    let directory = projects.open_verified_directory(stage)?;
    verify_directory(&*directory, true)?;
    for entry in directory.entries()? {
        let entry = entry?;
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
    fs: &dyn SafeFs,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let state = match fs.open_anchor(roots.state.as_path()) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&*state, true)?;
    let projects = match state.open_verified_directory(OsStr::new(PROJECTS_DIR)) {
        Ok(projects) => projects,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_directory(&*projects, true)?;
    find_binding(&*projects, location.common_dir())
}

fn find_binding(
    projects: &dyn SafeDir,
    target: &Path,
) -> Result<Option<ProjectBinding>, ConfigError> {
    let mut count = 0;
    let mut found = None;
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
        let project = name
            .parse::<ProjectId>()
            .map_err(|_| ConfigError::InvalidProjectBinding)?;
        let directory = projects.open_verified_directory(&entry)?;
        verify_directory(&*directory, true)?;
        let entry = directory.metadata_at(OsStr::new(RECORD), false)?;
        let bytes = read_record(&*directory, OsStr::new(RECORD), &entry, RECORD_LIMIT)?;
        let record: ProjectBindingRecord =
            serde_json::from_slice(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
        if record.schema_version != 1 || record.project != project {
            return Err(ConfigError::InvalidProjectBinding);
        }
        let common_dir = decode_location(&record.canonical_common_dir)?;
        if found
            .as_ref()
            .is_some_and(|binding: &ProjectBinding| binding.common_dir() == common_dir)
        {
            return Err(ConfigError::InvalidProjectBinding);
        }
        if common_dir == target {
            found = Some(ProjectBinding {
                project,
                canonical_common_dir: common_dir,
            });
        }
    }
    Ok(found)
}

fn create_binding(
    projects: &dyn SafeDir,
    canonical_common_dir: PathBuf,
    fs: &dyn SafeFs,
) -> Result<ProjectBinding, ConfigError> {
    for _ in 0..16 {
        let project = ProjectId::from_bytes(fs.entropy_128()?);
        let name = OsString::from(project.to_string());
        let stage = OsString::from(format!(".project-stage-{project}"));
        match projects.mkdir(&stage, 0o700) {
            Ok(()) => projects.sync()?,
            Err(error) if error.is_exists() => continue,
            Err(error) => return Err(error.into()),
        }
        let directory = projects.open_verified_directory(&stage)?;
        verify_directory(&*directory, true)?;
        let record = ProjectBindingRecord {
            schema_version: 1,
            project,
            canonical_common_dir: encode_location(&canonical_common_dir),
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| ConfigError::InvalidProjectBinding)?;
        match directory.atomic_write(
            OsStr::new(RECORD),
            ".project-",
            &bytes,
            0o600,
            PublishMode::NoReplace,
        )? {
            provenant_supervisor::fs::Publication::Durable => {}
            provenant_supervisor::fs::Publication::VisibleDurabilityUnconfirmed { .. } => {
                return Err(ConfigError::InvalidProjectBinding);
            }
        }
        match projects.rename_noreplace(&stage, &name) {
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

fn open_or_create_projects(state: &dyn SafeDir) -> Result<Box<dyn SafeDir>, ConfigError> {
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

fn verify_directory(directory: &dyn SafeDir, private: bool) -> Result<(), ConfigError> {
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
    directory: &dyn SafeDir,
    name: &OsStr,
    entry: &Metadata,
    limit: u64,
) -> Result<Vec<u8>, ConfigError> {
    if entry.kind != FileKind::Regular || entry.links != 1 || entry.size > limit {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let mut file = directory.open_read(name)?;
    let opened = file.metadata()?;
    if !same_metadata(entry, &opened) {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let mut bytes = Vec::new();
    (&mut *file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::InvalidProjectBinding)?;
    if bytes.len() as u64 > limit {
        return Err(ConfigError::InvalidProjectBinding);
    }
    let after = file.metadata()?;
    let path_after = directory.metadata_at(name, false)?;
    if !same_metadata(&opened, &after)
        || after.size != bytes.len() as u64
        || !same_metadata(&after, &path_after)
    {
        return Err(ConfigError::InvalidProjectBinding);
    }
    Ok(bytes)
}

fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    left.kind == right.kind
        && left.device == right.device
        && left.inode == right.inode
        && left.links == right.links
        && left.size == right.size
        && left.modified_ns == right.modified_ns
        && left.mode == right.mode
        && left.uid == right.uid
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
pub fn bind_host(roots: &Roots, fs: &dyn SafeFs) -> Result<provenant_core::HostId, ConfigError> {
    let state = fs.open_anchor(roots.state.as_path())?;
    verify_directory(&*state, true)?;
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
            let bytes = read_record(&*state, name, &meta, 32)?;
            if bytes.len() != 32 {
                return Err(ConfigError::InvalidProjectBinding);
            }
            let text =
                std::str::from_utf8(&bytes).map_err(|_| ConfigError::InvalidProjectBinding)?;
            text.parse().map_err(|_| ConfigError::InvalidProjectBinding)
        }
        Err(error) if error.is_not_found() => {
            let id = provenant_core::HostId::from_bytes(fs.entropy_128()?);
            match state.atomic_write(
                name,
                ".host-",
                id.to_string().as_bytes(),
                0o600,
                PublishMode::NoReplace,
            )? {
                provenant_supervisor::fs::Publication::Durable => Ok(id),
                _ => Err(ConfigError::InvalidProjectBinding),
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{RootInputs, Roots};
    use provenant_supervisor::fs::LocalFs;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
    }

    fn roots(temp: &tempfile::TempDir) -> Roots {
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let checkout = temp.path().join("checkout");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(state);
        inputs.resolve(&LocalFs).unwrap()
    }

    #[test]
    fn bind_project_reuses_random_identity_for_the_same_canonical_common_dir() {
        let temp = tempdir();
        let roots = roots(&temp);
        let location = ProjectLocation::new(temp.path().join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();

        let first = bind_project(&roots, location.clone(), &LocalFs).unwrap();
        let second = bind_project(&roots, location, &LocalFs).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.canonical_common_dir,
            temp.path().join("repository/.git")
        );
    }

    #[test]
    fn bind_project_assigns_distinct_ids_to_distinct_common_directories() {
        let temp = tempdir();
        let roots = roots(&temp);
        let first_location = temp.path().join("one/.git");
        let second_location = temp.path().join("two/.git");
        fs::create_dir_all(&first_location).unwrap();
        fs::create_dir_all(&second_location).unwrap();

        let first = bind_project(
            &roots,
            ProjectLocation::new(first_location).unwrap(),
            &LocalFs,
        )
        .unwrap();
        let second = bind_project(
            &roots,
            ProjectLocation::new(second_location).unwrap(),
            &LocalFs,
        )
        .unwrap();

        assert_ne!(first.project, second.project);
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

        let location = ProjectLocation::new(temp.path().join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();
        bind_project(&roots, location, &LocalFs).unwrap();

        assert!(!stage.exists());
    }

    #[test]
    fn bind_project_refuses_a_stale_stage_symlink_without_touching_its_target() {
        let temp = tempdir();
        let roots = roots(&temp);
        let projects = roots.state.as_path().join(PROJECTS_DIR);
        fs::create_dir(&projects).unwrap();
        fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();

        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"outside contents").unwrap();
        let project = ProjectId::from_bytes([9; 16]);
        let stage = projects.join(format!(".project-stage-{project}"));
        std::os::unix::fs::symlink(&outside, &stage).unwrap();

        let location = ProjectLocation::new(temp.path().join("repository/.git")).unwrap();
        fs::create_dir_all(location.common_dir()).unwrap();

        assert!(bind_project(&roots, location, &LocalFs).is_err());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside contents");
        assert!(
            fs::symlink_metadata(&stage)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
