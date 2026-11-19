//! Attempt-private Git state. Only host-sealed directory identities may be
//! reopened for settlement; launch never reads or resets a sibling index.
use super::*;

/// Copy a stable index, expanding split indexes before a clean host view uses it.
pub(crate) fn copy_index(
    git: &Path,
    source_private: &Directory,
    common: &Directory,
    root_path: &Path,
) -> Result<()> {
    let write = |directory: &Directory, name: &str, bytes: &[u8]| -> Result<()> {
        directory
            .atomic_write(OsStr::new(name), ".index-", bytes, PublishMode::Replace)?
            .require_durable()?;
        Ok(())
    };
    match source_private.read_stable(OsStr::new("index"), 64 * 1024 * 1024) {
        Ok(bytes) => {
            write(common, "index", &bytes)?;
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
                    common,
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
    Ok(())
}

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
    // Reader snapshots preserve the source index. Writer indexes start at
    // their own launch tip and never inherit host or sibling staging.
    if branch.is_none() {
        copy_index(git, source_private, &common, root_path)?;
    }
    let private_files: &[&str] = if branch.is_none() {
        &["config.worktree", "MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"]
    } else {
        &["config.worktree"]
    };
    for name in private_files {
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
    if let Some((_, base)) = branch {
        let objects = scratch.current_path()?.join("git-objects");
        let alternate = common_path.join("objects");
        execute_bounded(
            git,
            &common.current_path()?,
            root_path,
            &args(&["read-tree", &base.to_string()]),
            None,
            Execution {
                object_directory: Some(&objects),
                alternate_directory: Some(&alternate),
                ..Execution::default()
            },
        )?;
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
}

fn merge_state(source: &ObjectSource, repo: &VerifiedRepo) -> Result<Option<MergeState>> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    source
        .refs
        .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
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
    }
    Ok(Some(MergeState { parents, blobs }))
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

pub(crate) fn update_private_tip(
    source: &ObjectSource,
    branch: &str,
    tip: GitOid,
    expected_old: GitOid,
) -> Result<()> {
    use std::io::Write;
    // Ref publication never parses the lane's mutable config.
    let reference = provenant_confinement::repo::GitRefName::new(branch).map_err(|_| invalid())?;
    let (parent, leaf) = reference.as_str().rsplit_once('/').ok_or_else(invalid)?;
    let mut directory = source.refs.try_clone()?;
    for component in parent.split('/') {
        directory = directory.open_verified_directory(OsStr::new(component))?;
    }
    let leaf = OsStr::new(leaf);
    let mut lock_name = leaf.to_os_string();
    lock_name.push(".lock");
    let mut lock = directory.create_new(&lock_name)?;
    let result = (|| -> Result<()> {
        if private_tip(source, branch)? != expected_old {
            return Err(RunError::Validation("lane Git CAS ref update failed"));
        }
        writeln!(&mut lock, "{tip}")?;
        lock.sync()?;
        directory.publish_git_ref_lock(leaf, &lock)?;
        directory.sync()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = directory.discard_git_ref_lock(leaf, &lock);
    }
    result
}

pub(crate) fn private_tip(source: &ObjectSource, branch: &str) -> Result<GitOid> {
    provenant_os::git::private_branch_tip(&source.refs, branch).map_err(|_| invalid())
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
            schema_version: 4,
            candidate: candidate.into(),
            identities: ViewIdentities::read(&held, &refs, &objects)?,
            extra: Default::default(),
        })?,
        crate::RECORD_LIMIT,
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ObjectBinding {
    schema_version: u64,
    candidate: String,
    identities: ViewIdentities,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

impl PartialEq for DirectoryIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

impl Eq for DirectoryIdentity {}

impl DirectoryIdentity {
    fn read(directory: &Directory) -> Result<Self> {
        let metadata = directory.metadata()?;
        Ok(Self {
            device: metadata.device,
            inode: metadata.inode,
            extra: Default::default(),
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ViewIdentities {
    scratch: DirectoryIdentity,
    refs: DirectoryIdentity,
    objects: DirectoryIdentity,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

impl PartialEq for ViewIdentities {
    fn eq(&self, other: &Self) -> bool {
        self.scratch == other.scratch && self.refs == other.refs && self.objects == other.objects
    }
}

impl Eq for ViewIdentities {}

impl ViewIdentities {
    fn read(scratch: &Directory, refs: &Directory, objects: &Directory) -> Result<Self> {
        Ok(Self {
            scratch: DirectoryIdentity::read(scratch)?,
            refs: DirectoryIdentity::read(refs)?,
            objects: DirectoryIdentity::read(objects)?,
            extra: Default::default(),
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
    prepare_candidate(git, repo, attempt, scratch, branch, base)?;
    bind(attempt, scratch)
}

fn prepare_candidate(
    git: &Path,
    repo: &VerifiedRepo,
    attempt: &Directory,
    scratch: &Path,
    branch: &str,
    base: GitOid,
) -> Result<(Directory, Directory, Directory)> {
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
    let objects = held.open_verified_directory(OsStr::new("git-objects"))?;
    let refs = held.open_verified_directory(OsStr::new("git-common"))?;
    Ok((held, refs, objects))
}

pub(super) fn object_binding(attempt: &Directory) -> Result<Option<ObjectBinding>> {
    let Some(bytes) =
        crate::artifacts::read_sealed(attempt, "writer-objects.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    let binding: ObjectBinding = provenant_records::decode(
        bytes.as_slice(),
        provenant_records::Limits {
            max_bytes: crate::RECORD_LIMIT,
            ..Default::default()
        },
    )?;
    if !valid_candidate(&binding.candidate) || binding.schema_version != 4 {
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
    if binding.identities != ViewIdentities::read(&scratch, &refs, &objects)? {
        return Err(RunError::Validation("private Git view identity changed"));
    }
    Ok(Some(ObjectSource { objects, refs }))
}

/// Move only committed host state into the native index. Private staging and
/// merge metadata never leave their attempt. Git holds index.lock while its
/// index-only two-tree merge resolves HEAD and preserves unrelated native
/// staging. Working files may contain live lane edits and are never checked out.
pub(crate) fn advance_native_index(repo: &VerifiedRepo, base: GitOid) -> Result<bool> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_START",
    ] {
        match repo.private_dir().metadata_at(OsStr::new(marker), false) {
            Ok(_) => return Ok(false),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    repo.private_dir()
        .read_stable(OsStr::new("index"), 64 * 1024 * 1024)?;
    if let Err(error) = execute(
        &git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&["read-tree", "-i", "-m", &base.to_string(), "HEAD"]),
        None,
    ) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: native index left intact after publication: {error}"
        );
        return Ok(false);
    }
    let paths = execute(
        &git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&["ls-files", "-z"]),
        None,
    )?;
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
    Ok(true)
}

#[cfg(test)]
mod stored_shape_tests {
    use super::*;

    #[test]
    fn object_binding_preserves_additive_fields_and_declared_identity() {
        let payload = br#"{
            "schema_version":4,
            "candidate":"candidate-1",
            "future_binding":{"enabled":true},
            "identities":{
                "future_identities":7,
                "scratch":{"device":1,"inode":2,"future_identity":"scratch"},
                "refs":{"device":3,"inode":4,"future_identity":"refs"},
                "objects":{"device":5,"inode":6,"future_identity":"objects"}
            }
        }"#;
        let limits = provenant_records::Limits {
            max_bytes: crate::RECORD_LIMIT,
            ..Default::default()
        };
        let binding: ObjectBinding = provenant_records::decode(payload.as_slice(), limits)
            .expect("bounded decoder accepts additive fields");

        let encoded = provenant_records::encode(&binding).expect("record encodes");
        let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid JSON");
        assert_eq!(value["future_binding"]["enabled"], true);
        assert_eq!(value["identities"]["future_identities"], 7);
        for name in ["scratch", "refs", "objects"] {
            assert!(value["identities"][name]["future_identity"].is_string());
        }

        assert_eq!(
            &binding.identities,
            &ViewIdentities {
                scratch: DirectoryIdentity {
                    device: 1,
                    inode: 2,
                    extra: Default::default(),
                },
                refs: DirectoryIdentity {
                    device: 3,
                    inode: 4,
                    extra: Default::default(),
                },
                objects: DirectoryIdentity {
                    device: 5,
                    inode: 6,
                    extra: Default::default(),
                },
                extra: Default::default(),
            }
        );

        let redecoded: ObjectBinding = provenant_records::decode(encoded.as_slice(), limits)
            .expect("bounded decoder reads its encoded record");
        assert_eq!(binding.candidate, redecoded.candidate);
        assert_eq!(binding.identities, redecoded.identities);
    }
}
