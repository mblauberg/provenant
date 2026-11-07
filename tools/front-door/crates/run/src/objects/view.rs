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
    prepare_refs_from(
        git,
        repo.common_dir(),
        repo.private_dir(),
        repo.common_git_dir(),
        repo.root_path(),
        scratch,
        &format!("ref: {branch}\n"),
        Some((branch, base)),
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_refs_from(
    git: &Path,
    source_common: &Directory,
    source_private: &Directory,
    common_path: &Path,
    root_path: &Path,
    scratch: &Directory,
    head: &str,
    branch: Option<(&str, GitOid)>,
) -> Result<()> {
    let common = directory(scratch, OsStr::new("git-common"))?;
    let write = |directory: &Directory, name: &str, bytes: &[u8]| -> Result<()> {
        directory
            .atomic_write(OsStr::new(name), ".objects-", bytes, PublishMode::NoReplace)?
            .require_durable()?;
        Ok(())
    };
    // Keep the repository's remotes and branch upstream in the lane copy.
    // The host never reads this mutable copy back during settlement.
    write(
        &common,
        "config",
        &source_common.read_stable(OsStr::new("config"), 1024 * 1024)?,
    )?;
    match source_common.read_stable(OsStr::new("shallow"), 1024 * 1024) {
        Ok(bytes) => write(&common, "shallow", &bytes)?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    write(&common, "HEAD", head.as_bytes())?;
    directory(&common, OsStr::new("objects"))?;
    directory(&common, OsStr::new("refs"))?;
    match source_private.read_stable(OsStr::new("index"), 64 * 1024 * 1024) {
        Ok(bytes) => {
            write(&common, "index", &bytes)?;
            // Ask Git to parse the copied index, then read only the named
            // shared index through the held source directory (without links).
            let index = common.current_path()?.join("index");
            let shared = execute_bounded(
                git,
                &source_private.current_path()?,
                root_path,
                &args(&["rev-parse", "--shared-index-path"]),
                None,
                Execution {
                    index_file: Some(&index),
                    ..Execution::default()
                },
            )?;
            let shared = std::str::from_utf8(&shared).map_err(|_| invalid())?.trim();
            if !shared.is_empty() {
                let name = Path::new(shared)
                    .file_name()
                    .and_then(OsStr::to_str)
                    .ok_or_else(invalid)?;
                let hash = name.strip_prefix("sharedindex.").ok_or_else(invalid)?;
                if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(invalid());
                }
                write(
                    &common,
                    name,
                    &source_private.read_stable(OsStr::new(name), 64 * 1024 * 1024)?,
                )?;
                execute(
                    git,
                    &common.current_path()?,
                    root_path,
                    &args(&["update-index", "--no-split-index"]),
                    None,
                )?;
            }
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    // Preserve per-worktree settings and merge metadata alongside the index.
    for name in ["config.worktree", "MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"] {
        match source_private.read_stable(OsStr::new(name), 1024 * 1024) {
            Ok(bytes) => write(&common, name, &bytes)?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    let refs = command(
        git,
        common_path,
        root_path,
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
    if let Some((branch, base)) = branch {
        let (parent, leaf) = branch.rsplit_once('/').ok_or_else(invalid)?;
        let mut parent_dir = common.try_clone()?;
        for component in parent.split('/') {
            parent_dir = directory(&parent_dir, OsStr::new(component))?;
        }
        write(&parent_dir, leaf, format!("{base}\n").as_bytes())?;
    }
    if let Ok(info) = source_common.open_verified_directory(OsStr::new("info")) {
        match info.read_stable(OsStr::new("exclude"), 1024 * 1024) {
            Ok(bytes) => write(&directory(&common, OsStr::new("info"))?, "exclude", &bytes)?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    common.sync()?;
    Ok(())
}

/// Prepare a disposable reader repository using the same snapshot as writers.
pub(crate) fn prepare_read_only(
    git: &Path,
    repo: &provenant_confinement::ReadOnlyRepo,
    scratch: &Path,
    view_git: Option<&Path>,
    head: Option<GitOid>,
) -> Result<()> {
    repo.revalidate().map_err(|_| invalid())?;
    let held = Directory::open_anchor(scratch)?;
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
    let view = view_git.map(Directory::open_anchor).transpose()?;
    let private = view.as_ref().unwrap_or_else(|| repo.private_dir());
    let head = match head {
        Some(oid) => format!("{oid}\n"),
        None => String::from_utf8(private.read_stable(OsStr::new("HEAD"), 1024)?)
            .map_err(|_| invalid())?,
    };
    prepare_refs_from(
        git,
        repo.common_dir(),
        private,
        repo.common_git_dir(),
        repo.root_path(),
        &held,
        &head,
        None,
    )?;
    repo.revalidate().map_err(|_| invalid())?;
    Ok(())
}

pub(crate) struct ObjectSource {
    pub objects: Directory,
    pub refs: Directory,
}

pub(crate) fn merge_head(source: &ObjectSource) -> Result<Vec<u8>> {
    Ok(source
        .refs
        .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)?)
}

/// Objects needed by an in-progress merge, including staged blobs that may
/// never have appeared in a commit. Git parses the index; paths stay opaque.
struct MergeState {
    parents: Vec<GitOid>,
    blobs: Vec<GitOid>,
    stage_zero_paths: Vec<u8>,
}

fn merge_state(source: &ObjectSource, repo: &VerifiedRepo) -> Result<Option<MergeState>> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let directory = source.refs.current_path()?;
    let unmerged = execute(
        &git,
        &directory,
        repo.root_path(),
        &args(&["ls-files", "--unmerged", "-z"]),
        None,
    )?;
    let raw = match source
        .refs
        .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)
    {
        Ok(raw) => raw,
        Err(error) if error.is_not_found() && unmerged.is_empty() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut parents = Vec::new();
    for line in std::str::from_utf8(&raw).map_err(|_| invalid())?.lines() {
        if line.bytes().all(|byte| byte == b'0') {
            return Err(invalid());
        }
        parents.push(GitOid::parse(line).map_err(|_| invalid())?);
    }
    if parents.is_empty() {
        return Err(invalid());
    }
    let mut blobs = Vec::new();
    let mut stage_zero_paths = Vec::new();
    let stages = execute(
        &git,
        &directory,
        repo.root_path(),
        &args(&["ls-files", "--stage", "-z"]),
        None,
    )?;
    for entry in stages
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(invalid)?;
        let header = &entry[..tab];
        let header = std::str::from_utf8(header).map_err(|_| invalid())?;
        let mut fields = header.split(' ');
        let mode = fields.next().ok_or_else(invalid)?;
        let oid = fields.next().ok_or_else(invalid)?;
        if oid.bytes().all(|byte| byte == b'0') {
            return Err(invalid());
        }
        let stage = fields.next().ok_or_else(invalid)?;
        if fields.next().is_some() || !matches!(stage, "0" | "1" | "2" | "3") {
            return Err(invalid());
        }
        match mode {
            "100644" | "100755" | "120000" => {
                blobs.push(GitOid::parse(oid).map_err(|_| invalid())?)
            }
            "160000" => {} // Submodule objects belong to the submodule.
            _ => return Err(invalid()),
        }
        if stage == "0" {
            stage_zero_paths.extend_from_slice(&entry[tab + 1..]);
            stage_zero_paths.push(0);
        }
    }
    Ok(Some(MergeState {
        parents,
        blobs,
        stage_zero_paths,
    }))
}

pub(crate) fn merge_objects(source: &ObjectSource, repo: &VerifiedRepo) -> Result<Vec<GitOid>> {
    let Some(state) = merge_state(source, repo)? else {
        return Ok(Vec::new());
    };
    let mut objects = state.parents;
    objects.extend(state.blobs);
    objects.sort_by_key(ToString::to_string);
    objects.dedup();
    Ok(objects)
}

pub(crate) fn set_private_tip(source: &ObjectSource, branch: &str, tip: GitOid) -> Result<()> {
    let (parent, leaf) = branch.rsplit_once('/').ok_or_else(invalid)?;
    let mut held = source.refs.try_clone()?;
    for component in parent.split('/') {
        held = directory(&held, OsStr::new(component))?;
    }
    held.atomic_write(
        OsStr::new(leaf),
        ".budget-",
        format!("{tip}\n").as_bytes(),
        PublishMode::Replace,
    )?
    .require_durable()?;
    Ok(())
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
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let merge = merge_state(source, repo)?;
    if let Some(state) = &merge {
        let input = state
            .parents
            .iter()
            .chain(&state.blobs)
            .map(|oid| format!("{oid}\n"))
            .collect::<String>();
        let types = execute_bounded(
            &git,
            repo.common_git_dir(),
            repo.root_path(),
            &args(&["cat-file", "--batch-check=%(objecttype)"]),
            None,
            Execution {
                input: Some(input.as_bytes()),
                ..Execution::default()
            },
        )?;
        let mut types = types.split(|byte| *byte == b'\n');
        for expected in std::iter::repeat_n(b"commit".as_slice(), state.parents.len())
            .chain(std::iter::repeat_n(b"blob".as_slice(), state.blobs.len()))
        {
            if types.next() != Some(expected) {
                return Err(RunError::Validation(
                    "merge index object is unavailable or has the wrong type",
                ));
            }
        }
        let merge_head = source
            .refs
            .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)?;
        match repo
            .private_dir()
            .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)
        {
            Ok(existing) if existing != merge_head => {
                return Err(RunError::Validation(
                    "host merge state changed before import",
                ));
            }
            Ok(_) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        for name in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"] {
            match source.refs.read_stable(OsStr::new(name), 1024 * 1024) {
                Ok(bytes) => repo
                    .private_dir()
                    .atomic_write(
                        OsStr::new(name),
                        ".lane-merge-",
                        &bytes,
                        PublishMode::Replace,
                    )?
                    .require_durable()?,
                Err(error) if error.is_not_found() && name != "MERGE_HEAD" => {}
                Err(error) => return Err(error.into()),
            }
        }
    } else {
        // A continuation may have committed the merge in its private view.
        // Clear host merge state only when the published branch contains every
        // recorded parent; an abort or unrelated host change stays fenced.
        match repo
            .private_dir()
            .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)
        {
            Ok(raw) => {
                let text = std::str::from_utf8(&raw).map_err(|_| invalid())?;
                let mut count = 0;
                for line in text.lines() {
                    let parent = GitOid::parse(line).map_err(|_| invalid())?;
                    execute(
                        &git,
                        repo.private_git_dir(),
                        repo.root_path(),
                        &args(&["merge-base", "--is-ancestor", &parent.to_string(), "HEAD"]),
                        None,
                    )?;
                    count += 1;
                }
                if count == 0 {
                    return Err(invalid());
                }
                for name in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"] {
                    match repo.private_dir().unlink(OsStr::new(name)) {
                        Ok(()) => {}
                        Err(error) if error.is_not_found() => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                repo.private_dir().sync()?;
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
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
    let paths = if let Some(state) = merge {
        state.stage_zero_paths
    } else {
        execute(
            &git,
            repo.private_git_dir(),
            repo.root_path(),
            &args(&["ls-files", "-z"]),
            None,
        )?
    };
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
