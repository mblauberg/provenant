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
    // Git's view of the lane is a registered linked worktree: the private
    // gitdir sits below the common dir, carries its backlink to the checkout
    // and keeps the same HEAD. The host prepares it before the lane starts;
    // refs, objects and the index remain in the common dir and are unchanged.
    let registry = directory(&common, OsStr::new(provenant_core::LANE_GIT_WORKTREES_DIR))?;
    let registration = directory(&registry, OsStr::new(provenant_core::LANE_GIT_REGISTRATION))?;
    write(&registration, "HEAD", head.as_bytes())?;
    write(&registration, "commondir", b"../..\n")?;
    let mut gitdir = root_path
        .join(".git")
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    gitdir.push(b'\n');
    write(&registration, "gitdir", &gitdir)?;
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
            // Per-worktree files belong in the private registration as well:
            // Git reads them there once the lane uses the linked layout.
            Ok(bytes) => {
                write(&common, name, &bytes)?;
                write(&registration, name, &bytes)?;
            }
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

/// Objects needed by an in-progress merge, including staged blobs that may
/// never have appeared in a commit. Git parses the index; paths stay opaque.
struct MergeState {
    parents: Vec<GitOid>,
    blobs: Vec<GitOid>,
}

/// Merge metadata written by a lane whose private gitdir is a linked
/// worktree registration. An absent registration is not an error: some launches
/// select the common dir directly and keep their per-worktree files there.
fn registered_merge_head(source: &ObjectSource) -> Result<Option<Vec<u8>>> {
    let registry = match source
        .refs
        .open_verified_directory(OsStr::new(provenant_core::LANE_GIT_WORKTREES_DIR))
    {
        Ok(registry) => registry,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let registration =
        match registry.open_verified_directory(OsStr::new(provenant_core::LANE_GIT_REGISTRATION)) {
            Ok(registration) => registration,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
    match registration.read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
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
    // A shim-bound lane writes per-worktree merge state into its linked
    // registration. A direct Git call that selects the common dir writes it
    // there instead; both are legitimate producers of a private merge.
    let raw = match registered_merge_head(source)? {
        Some(raw) => raw,
        None => match source
            .refs
            .read_stable(OsStr::new("MERGE_HEAD"), 1024 * 1024)
        {
            Ok(raw) => raw,
            Err(error) if error.is_not_found() && unmerged.is_empty() => return Ok(None),
            Err(error) => return Err(error.into()),
        },
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
    let temporary = crate::plan::temporary_dir(attempt)?;
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
    seed_native_index(repo, base)?;
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
    let temporary = crate::plan::temporary_dir(attempt)?;
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
    let temporary = crate::plan::temporary_dir(attempt)?;
    let scratch = temporary.open_verified_directory(OsStr::new(&candidate))?;
    let objects = scratch.open_verified_directory(OsStr::new("git-objects"))?;
    let refs = scratch.open_verified_directory(OsStr::new("git-common"))?;
    if binding.identities != ViewIdentities::read(&scratch, &refs, &objects)? {
        return Err(RunError::Validation("private Git view identity changed"));
    }
    Ok(Some(ObjectSource { objects, refs }))
}

const NATIVE_INDEX_RECORD: &str = "provenant-native-index.json";
/// Serialises native-index advances for one worktree. It lives beside the
/// record in the worktree's private Git directory, so it never contends with
/// the custody slot's admission gate that launch, removal and landing probe.
const NATIVE_INDEX_LOCK: &str = "provenant-native-index.lock";
const NATIVE_INDEX_LIMIT: u64 = 64 * 1024 * 1024;
const NATIVE_INDEX_WAIT_SECONDS: u64 = 10;

/// One command that repairs a native index that stopped following the branch:
/// it resets the index to HEAD and drops the record, so the next settlement
/// records the repaired index instead of judging it against the stale one.
pub(crate) const NATIVE_INDEX_REMEDY: &str =
    "git reset -q && rm -f -- \"$(git rev-parse --git-path provenant-native-index.json)\"";

#[derive(serde::Serialize, serde::Deserialize)]
struct NativeIndexRecord {
    schema_version: u64,
    followed: String,
    /// Digest of the index entries Provenant last left in place. An index that
    /// no longer matches was committed, reset or restaged by someone else since,
    /// so merging from `followed` could silently stage a reversal.
    index_digest: String,
}

fn native_index_record(repo: &VerifiedRepo) -> Result<Option<NativeIndexRecord>> {
    let bytes = match repo
        .private_dir()
        .read_stable(OsStr::new(NATIVE_INDEX_RECORD), 4096)
    {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let record: NativeIndexRecord = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if record.schema_version != 2 {
        return Err(invalid());
    }
    GitOid::parse(&record.followed).map_err(|_| invalid())?;
    Ok(Some(record))
}

/// The commit the host worktree's native index last followed, if recorded.
/// Publications advance the index from this commit, never from whichever tip a
/// given publication happened to replace: siblings may publish between two
/// advances, and a crash may leave the index behind the branch.
#[cfg(test)]
pub(crate) fn followed_native_index(repo: &VerifiedRepo) -> Result<Option<GitOid>> {
    native_index_record(repo)?
        .map(|record| GitOid::parse(&record.followed).map_err(|_| invalid()))
        .transpose()
}

fn record_native_index(
    repo: &VerifiedRepo,
    followed: GitOid,
    index_digest: &str,
    mode: PublishMode,
) -> Result<()> {
    let bytes = serde_json::to_vec(&NativeIndexRecord {
        schema_version: 2,
        followed: followed.to_string(),
        index_digest: index_digest.to_owned(),
    })
    .map_err(|_| invalid())?;
    match repo
        .private_dir()
        .atomic_write(OsStr::new(NATIVE_INDEX_RECORD), ".run-", &bytes, mode)
    {
        Ok(publication) => publication.require_durable()?,
        Err(error) if mode == PublishMode::NoReplace && error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Digest of the index's entries (mode, object, stage and path), read without
/// taking index.lock or rewriting the index. A missing or oversized index is
/// unreadable here rather than an error for the caller's settlement.
fn index_digest(git: &Path, repo: &VerifiedRepo) -> Result<String> {
    use sha2::{Digest, Sha256};
    match repo.private_dir().metadata_at(OsStr::new("index"), false) {
        Ok(metadata)
            if metadata.kind == FileKind::Regular && metadata.size <= NATIVE_INDEX_LIMIT => {}
        Ok(_) => {
            return Err(RunError::Validation(
                "native index is not a bounded regular file",
            ));
        }
        Err(error) if error.is_not_found() => {
            return Err(RunError::Validation("native index is missing"));
        }
        Err(error) => return Err(error.into()),
    }
    let entries = execute(
        git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&["ls-files", "-s", "-z"]),
        None,
    )?;
    Ok(Sha256::digest(&entries)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn paths_from(git: &Path, repo: &VerifiedRepo, arguments: &[&str]) -> Result<Vec<String>> {
    let bytes = execute(
        git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(arguments),
        None,
    )?;
    let mut paths = bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Record the commit the native index starts from when a writer first attaches.
/// An existing record is kept: it is never newer than the index, which is the
/// safe direction for the two-tree merge. An index that cannot be digested
/// leaves no record; the first sync records one.
pub(crate) fn seed_native_index(repo: &VerifiedRepo, base: GitOid) -> Result<()> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let Ok(digest) = index_digest(&git, repo) else {
        return Ok(());
    };
    record_native_index(repo, base, &digest, PublishMode::NoReplace)
}

/// Why the native index could not follow the branch tip. The index is left as
/// it was. Commits are absent when the inspection failed before resolving them,
/// and the paths are the ones that differ from the commit the index followed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IndexOutOfSync {
    pub followed: Option<GitOid>,
    pub target: Option<GitOid>,
    pub paths: Vec<String>,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IndexSync {
    /// The index follows the branch tip, whether it advanced now or already did.
    Synced,
    /// A native merge, rebase or similar operation owns the index, or another
    /// settlement is advancing it; nothing moved and the record is unchanged, so
    /// the next sync advances from the same commit.
    Deferred,
    /// The index was left unchanged and the record was not advanced: Git refused
    /// the two-tree merge, the index changed since Provenant last left it, or it
    /// could not be inspected. This is advisory and never fails settlement.
    OutOfSync(IndexOutOfSync),
}

#[derive(Default)]
struct Resolved {
    followed: Option<GitOid>,
    target: Option<GitOid>,
}

/// Move only committed host state into the native index. Private staging and
/// merge metadata never leave their attempt. Git holds index.lock while its
/// index-only two-tree merge resolves the branch tip and preserves unrelated
/// native staging. Working files may contain live lane edits and are never
/// checked out.
///
/// The merge runs from the recorded commit the index last followed to the
/// branch tip, so it is idempotent: repeating it after a crash, or after a
/// sibling already advanced the index, is a no-op. It runs only while the index
/// still matches what the record says Provenant left there; an index someone
/// else committed, reset or restaged since is left alone. `fallback` is used
/// only when nothing is recorded. Every inspection or record failure, including
/// a corrupt record, becomes an unknown result that leaves the index and the
/// record as they were. One lock per worktree serialises concurrent syncs.
pub(crate) fn sync_native_index(repo: &VerifiedRepo, fallback: GitOid) -> IndexSync {
    let mut resolved = Resolved::default();
    let unknown = |resolved: &Resolved, reason: String| {
        IndexSync::OutOfSync(IndexOutOfSync {
            followed: resolved.followed,
            target: resolved.target,
            paths: Vec::new(),
            reason,
        })
    };
    let clock = provenant_os::clock::SystemClock::new();
    let Ok(budget) = provenant_core::WaitBudget::cli(clock.mono_now(), NATIVE_INDEX_WAIT_SECONDS)
    else {
        return unknown(&resolved, "native index wait budget".to_owned());
    };
    let _lock = match repo.private_dir().lock(
        OsStr::new(NATIVE_INDEX_LOCK),
        provenant_os::fs::LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    ) {
        Ok(lock) => lock,
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => return IndexSync::Deferred,
        Err(error) => return unknown(&resolved, format!("native index lock: {error}")),
    };
    match advance_native_index(repo, fallback, &mut resolved) {
        Ok(sync) => sync,
        Err(error) => unknown(&resolved, error.to_string()),
    }
}

fn advance_native_index(
    repo: &VerifiedRepo,
    fallback: GitOid,
    resolved: &mut Resolved,
) -> Result<IndexSync> {
    let git = provenant_os::git::git_executable().ok_or(RunError::GitUnavailable)?;
    let recorded = native_index_record(repo)?;
    let followed = match &recorded {
        Some(record) => GitOid::parse(&record.followed).map_err(|_| invalid())?,
        None => fallback,
    };
    resolved.followed = Some(followed);
    let target = GitOid::parse(
        std::str::from_utf8(&execute(
            &git,
            repo.private_git_dir(),
            repo.root_path(),
            &args(&["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"]),
            None,
        )?)
        .map_err(|_| invalid())?
        .trim(),
    )
    .map_err(|_| invalid())?;
    resolved.target = Some(target);
    if followed == target {
        // Nothing to merge. Staging added since the last sync belongs to the
        // person: it sits on the tip the index already follows, so it becomes
        // the baseline the next advance is judged against.
        if let Ok(live) = index_digest(&git, repo)
            && recorded
                .as_ref()
                .is_none_or(|record| record.index_digest != live)
        {
            record_native_index(repo, target, &live, PublishMode::Replace)?;
        }
        return Ok(IndexSync::Synced);
    }
    // The native index is the checkout's own state. Any unfinished operation
    // there defers the advance rather than converging over its resolution.
    if provenant_confinement::worktrees::incomplete_git_operation(repo.private_dir(), None)?
        .is_some()
    {
        return Ok(IndexSync::Deferred);
    }
    let live = index_digest(&git, repo)?;
    if let Some(record) = &recorded
        && record.index_digest != live
    {
        let target_name = target.to_string();
        let followed_name = followed.to_string();
        let diff = |tree: &str| {
            paths_from(
                &git,
                repo,
                &[
                    "diff-index",
                    "--cached",
                    "--no-renames",
                    "--name-only",
                    "-z",
                    tree,
                ],
            )
        };
        if diff(&target_name)?.is_empty() {
            // The index already holds exactly the tip: an advance that died
            // before recording, or a reset to it. Nothing to merge.
            record_native_index(repo, target, &live, PublishMode::Replace)?;
            return Ok(IndexSync::Synced);
        }
        return Ok(IndexSync::OutOfSync(IndexOutOfSync {
            followed: Some(followed),
            target: Some(target),
            paths: diff(&followed_name).unwrap_or_default(),
            reason: "the index changed since Provenant last advanced it, so a merge from the recorded commit could stage a reversal".to_owned(),
        }));
    }
    if let Err(error) = execute(
        &git,
        repo.private_git_dir(),
        repo.root_path(),
        &args(&[
            "read-tree",
            "-i",
            "-m",
            &followed.to_string(),
            &target.to_string(),
        ]),
        None,
    ) {
        let paths = paths_from(
            &git,
            repo,
            &[
                "diff-tree",
                "-r",
                "--name-only",
                "--no-renames",
                "-z",
                &followed.to_string(),
                &target.to_string(),
            ],
        )
        .unwrap_or_default();
        return Ok(IndexSync::OutOfSync(IndexOutOfSync {
            followed: Some(followed),
            target: Some(target),
            paths,
            reason: error.to_string(),
        }));
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
    record_native_index(
        repo,
        target,
        &index_digest(&git, repo)?,
        PublishMode::Replace,
    )?;
    Ok(IndexSync::Synced)
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
