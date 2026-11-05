//! Private lane Git state. Paths stay compatible with provider confinement;
//! only host-sealed directory identities may be reopened for settlement.
use super::*;

pub(crate) fn prepare_refs(
    git: &Path,
    repo: &VerifiedRepo,
    scratch: &Directory,
    branch: &str,
    base: GitOid,
) -> Result<()> {
    let common = directory(scratch, OsStr::new("git-common"))?;
    let write = |directory: &Directory, name: &str, bytes: &[u8]| -> Result<()> {
        directory
            .atomic_write(OsStr::new(name), ".objects-", bytes, PublishMode::NoReplace)?
            .require_durable()?;
        Ok(())
    };
    // Repository open refuses config includes. Keep the host's validated
    // remotes and branch upstream, while the host never reads this mutable copy.
    write(
        &common,
        "config",
        &repo
            .common_dir()
            .read_stable(OsStr::new("config"), 1024 * 1024)?,
    )?;
    match repo
        .common_dir()
        .read_stable(OsStr::new("shallow"), 1024 * 1024)
    {
        Ok(bytes) => write(&common, "shallow", &bytes)?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    write(&common, "HEAD", format!("ref: {branch}\n").as_bytes())?;
    directory(&common, OsStr::new("objects"))?;
    match repo
        .private_dir()
        .read_stable(OsStr::new("index"), 64 * 1024 * 1024)
    {
        Ok(bytes) => write(&common, "index", &bytes)?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let refs = command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&[
            "for-each-ref",
            "--sort=refname",
            "--format=%(objectname) %(refname)",
        ]),
        None,
    )?;
    let mut packed = b"# pack-refs with: sorted\n".to_vec();
    packed.extend_from_slice(&refs);
    write(&common, "packed-refs", &packed)?;
    let (parent, leaf) = branch.rsplit_once('/').ok_or_else(invalid)?;
    let mut parent_dir = common.try_clone()?;
    for component in parent.split('/') {
        parent_dir = directory(&parent_dir, OsStr::new(component))?;
    }
    write(&parent_dir, leaf, format!("{base}\n").as_bytes())?;
    if let Ok(info) = repo
        .common_dir()
        .open_verified_directory(OsStr::new("info"))
    {
        match info.read_stable(OsStr::new("exclude"), 1024 * 1024) {
            Ok(bytes) => write(&directory(&common, OsStr::new("info"))?, "exclude", &bytes)?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    common.sync()?;
    Ok(())
}

pub(crate) struct ObjectSource {
    pub objects: Directory,
    refs: Directory,
}

pub(crate) fn private_tip(source: &ObjectSource, branch: &str) -> Result<GitOid> {
    let head = source.refs.read_stable(OsStr::new("HEAD"), 1024)?;
    if std::str::from_utf8(&head).map(str::trim).ok() != Some(format!("ref: {branch}").as_str()) {
        return Err(invalid());
    }
    let read_loose = || -> Result<Vec<u8>> {
        let (parent, leaf) = branch.rsplit_once('/').ok_or_else(invalid)?;
        let mut directory = source.refs.try_clone()?;
        for component in parent.split('/') {
            directory = directory.open_verified_directory(OsStr::new(component))?;
        }
        Ok(directory.read_stable(OsStr::new(leaf), 128)?)
    };
    let bytes = match read_loose() {
        Ok(bytes) => bytes,
        Err(RunError::Filesystem(error)) if error.is_not_found() => {
            let packed = source
                .refs
                .read_stable(OsStr::new("packed-refs"), 1024 * 1024)?;
            let mut found = None;
            for line in packed.split(|byte| *byte == b'\n') {
                if let Some(index) = line.iter().position(|byte| *byte == b' ')
                    && &line[index + 1..] == branch.as_bytes()
                {
                    if found.is_some() {
                        return Err(invalid());
                    }
                    found = Some(line[..index].to_vec());
                }
            }
            found.ok_or_else(invalid)?
        }
        Err(error) => return Err(error),
    };
    GitOid::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?.trim()).map_err(|_| invalid())
}

pub(crate) fn bind(attempt: &Directory, scratch: &Path) -> Result<()> {
    let candidate = scratch.file_name().ok_or_else(invalid)?;
    let candidate = candidate
        .to_str()
        .filter(|name| valid_candidate(name))
        .ok_or_else(invalid)?;
    let temporary = attempt.open_verified_directory(OsStr::new("tmp"))?;
    let held = temporary.open_verified_directory(OsStr::new(candidate))?;
    if held.current_path()? != scratch {
        return Err(invalid());
    }
    let objects = held.open_verified_directory(OsStr::new("git-objects"))?;
    let refs = held.open_verified_directory(OsStr::new("git-common"))?;
    crate::artifacts::seal(
        attempt,
        "writer-objects.json",
        &crate::db::bytes(&ObjectBinding {
            schema_version: 2,
            candidate: candidate.into(),
            identities: Some(ViewIdentities::read(&held, &refs, &objects)?),
        })?,
        crate::RECORD_LIMIT,
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ObjectBinding {
    schema_version: u64,
    candidate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identities: Option<ViewIdentities>,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

impl DirectoryIdentity {
    fn read(directory: &Directory) -> Result<Self> {
        let metadata = directory.metadata()?;
        Ok(Self {
            device: metadata.device,
            inode: metadata.inode,
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ViewIdentities {
    scratch: DirectoryIdentity,
    refs: DirectoryIdentity,
    objects: DirectoryIdentity,
}

impl ViewIdentities {
    fn read(scratch: &Directory, refs: &Directory, objects: &Directory) -> Result<Self> {
        Ok(Self {
            scratch: DirectoryIdentity::read(scratch)?,
            refs: DirectoryIdentity::read(refs)?,
            objects: DirectoryIdentity::read(objects)?,
        })
    }
}

fn valid_candidate(name: &str) -> bool {
    name.strip_prefix("candidate-")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

pub(crate) fn prepare_bound(
    git: &Path,
    repo: &VerifiedRepo,
    attempt: &Directory,
    scratch: &Path,
    branch: &str,
    base: GitOid,
) -> Result<()> {
    let candidate = scratch
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| valid_candidate(name))
        .ok_or_else(invalid)?;
    let temporary = attempt.open_verified_directory(OsStr::new("tmp"))?;
    let held = temporary.open_verified_directory(OsStr::new(candidate))?;
    if held.current_path()? != scratch {
        return Err(invalid());
    }
    // Custom provider launches may only edit files and omit the adapter's Git
    // setup. Their empty object store still needs a complete settlement path.
    let objects = directory(&held, OsStr::new("git-objects"))?;
    let info = directory(&objects, OsStr::new("info"))?;
    info.atomic_write(
        OsStr::new("alternates"),
        ".objects-",
        format!(
            "{}\n",
            quote_alternate(&repo.common_git_dir().join("objects"))
        )
        .as_bytes(),
        PublishMode::NoReplace,
    )?
    .require_durable()?;
    prepare_refs(git, repo, &held, branch, base)?;
    bind(attempt, scratch)
}

pub(super) fn object_binding(attempt: &Directory) -> Result<Option<ObjectBinding>> {
    let Some(bytes) =
        crate::artifacts::read_sealed(attempt, "writer-objects.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    // Keep the records crate's bounded, duplicate-key-refusing decoder while
    // admitting both persisted versions at this owning boundary.
    let binding: ObjectBinding = provenant_records::decode(
        bytes.as_slice(),
        provenant_records::Limits {
            max_bytes: crate::RECORD_LIMIT,
            ..Default::default()
        },
    )?;
    if !valid_candidate(&binding.candidate)
        || !matches!(
            (binding.schema_version, binding.identities.is_some()),
            (1, false) | (2, true)
        )
    {
        return Err(invalid());
    }
    Ok(Some(binding))
}

pub(crate) fn bound_source(attempt: &Directory) -> Result<Option<ObjectSource>> {
    let Some(binding) = object_binding(attempt)? else {
        return Ok(None);
    };
    let candidate = binding.candidate;
    // Open every lane-controlled component without following links.
    let temporary = attempt.open_verified_directory(OsStr::new("tmp"))?;
    let scratch = temporary.open_verified_directory(OsStr::new(&candidate))?;
    let objects = scratch.open_verified_directory(OsStr::new("git-objects"))?;
    let refs = scratch.open_verified_directory(OsStr::new("git-common"))?;
    // Version 1 runs predate identity sealing. Preserve their no-follow recovery
    // path; every new launch uses version 2 and refuses replaced directories.
    if let Some(expected) = binding.identities
        && expected != ViewIdentities::read(&scratch, &refs, &objects)?
    {
        return Err(RunError::Validation("private Git view identity changed"));
    }
    Ok(Some(ObjectSource { objects, refs }))
}

pub(crate) fn restore_index(source: &ObjectSource, repo: &VerifiedRepo) -> Result<()> {
    // An unmerged index needs its merge metadata and objects as well as the
    // index itself. Keep the retained private view intact until settlement can
    // restore those together; copying it first leaves an unusable host index.
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let unmerged = execute(
        &git,
        &source.refs.current_path()?,
        repo.root_path(),
        &args(&["ls-files", "--unmerged", "-z"]),
        None,
    )?;
    if !unmerged.is_empty() {
        return Err(RunError::Validation(
            "stopped merge requires retained import recovery",
        ));
    }
    match source
        .refs
        .read_stable(OsStr::new("index"), 64 * 1024 * 1024)
    {
        Ok(bytes) => repo
            .private_dir()
            .atomic_write(
                OsStr::new("index"),
                ".lane-index-",
                &bytes,
                PublishMode::Replace,
            )?
            .require_durable()?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let paths = execute(
        &git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&["ls-files", "-z"]),
        None,
    )?;
    // Git applies these modes separately. Combining them leaves skip-worktree
    // set on entries which also carry assume-unchanged.
    for flag in ["--no-skip-worktree", "--no-assume-unchanged"] {
        execute_bounded(
            &git,
            repo.private_git_dir(),
            repo.root_path(),
            &args(&["update-index", flag, "-z", "--stdin"]),
            None,
            Execution {
                input: Some(&paths),
                ..Execution::default()
            },
        )?;
    }
    Ok(())
}
