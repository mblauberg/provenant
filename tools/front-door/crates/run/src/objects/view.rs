//! Lane Git state. Shared writer paths stay under host-issued worktree grants;
//! only host-sealed directory identities may be reopened for settlement.
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
        None,
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
    minimal_config: Option<&[u8]>,
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
    match minimal_config {
        Some(config) => common
            .atomic_write_with_permissions(
                OsStr::new("config"),
                ".objects-",
                config,
                0o444,
                PublishMode::NoReplace,
            )?
            .require_durable()?,
        None => write(
            &common,
            "config",
            &source_common.read_stable(OsStr::new("config"), 1024 * 1024)?,
        )?,
    }
    match source_common.read_stable(OsStr::new("shallow"), 1024 * 1024) {
        Ok(bytes) => write(&common, "shallow", &bytes)?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    write(&common, "HEAD", head.as_bytes())?;
    directory(&common, OsStr::new("objects"))?;
    directory(&common, OsStr::new("refs"))?;
    copy_index(git, source_private, &common, root_path)?;
    // Preserve per-worktree settings and merge metadata alongside the index.
    let private_files: &[&str] = if minimal_config.is_some() {
        &["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"]
    } else {
        &["config.worktree", "MERGE_HEAD", "MERGE_MODE", "MERGE_MSG"]
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

pub(crate) fn update_private_tip(
    git: &Path,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    branch: &str,
    tip: GitOid,
    expected_old: GitOid,
) -> Result<()> {
    let refs_path = source.refs.current_path()?;
    let objects_path = source.objects.current_path()?;
    let alternate = repo.common_git_dir().join("objects");
    execute_bounded(
        git,
        &refs_path,
        repo.root_path(),
        &args(&[
            "update-ref",
            branch,
            &tip.to_string(),
            &expected_old.to_string(),
        ]),
        None,
        Execution {
            object_directory: Some(&objects_path),
            alternate_directory: Some(&alternate),
            common_directory: Some(&refs_path),
            ..Execution::default()
        },
    )
    .map_err(|_| RunError::Validation("lane Git CAS ref update failed"))?;
    Ok(())
}

fn shared_is_ancestor(
    git: &Path,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    ancestor: GitOid,
    head: GitOid,
) -> Result<bool> {
    let refs_path = source.refs.current_path()?;
    let objects_path = source.objects.current_path()?;
    let alternate = repo.common_git_dir().join("objects");
    // Unlike merge-base --is-ancestor, rev-list succeeds for both answers,
    // allowing command failures to propagate instead of implying divergence.
    let uncontained = execute_bounded(
        git,
        &refs_path,
        repo.root_path(),
        &args(&[
            "rev-list",
            "--max-count=1",
            &ancestor.to_string(),
            "--not",
            &head.to_string(),
        ]),
        None,
        Execution {
            object_directory: Some(&objects_path),
            alternate_directory: Some(&alternate),
            common_directory: Some(&refs_path),
            ..Execution::default()
        },
    )?;
    Ok(uncontained.is_empty())
}

const SHARED_INDEX_RESET: &str = "pending-index-reset.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SharedIndexReset {
    schema_version: u64,
    target: GitOid,
    recovered: Option<GitOid>,
}

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_SHARED_INDEX_RESET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn finish_shared_reset(root: &Directory) -> Result<()> {
    root.unlink(OsStr::new(SHARED_INDEX_RESET))?;
    root.sync()?;
    Ok(())
}

fn resume_shared_reset(
    git: &Path,
    root: &Directory,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    current: GitOid,
) -> Result<Option<GitOid>> {
    let raw = match root.read_stable(OsStr::new(SHARED_INDEX_RESET), 1024) {
        Ok(raw) => raw,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pending: SharedIndexReset = crate::db::decode(&raw)?;
    if pending.schema_version != 1 {
        return Err(invalid());
    }
    // A later sibling ref update supersedes this reset. Never put its index
    // back at the older target, even if that target is still an ancestor.
    if current == pending.target {
        reset_shared_index(git, source, repo, pending.target)?;
    }
    finish_shared_reset(root)?;
    Ok(pending.recovered)
}

fn reconcile_shared_tip(
    git: &Path,
    root: &Directory,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    branch: &str,
    change: SharedIndexReset,
    old: GitOid,
) -> Result<()> {
    root.atomic_write(
        OsStr::new(SHARED_INDEX_RESET),
        ".index-reset-",
        &crate::db::bytes(&change)?,
        PublishMode::NoReplace,
    )?
    .require_durable()?;
    update_private_tip(git, source, repo, branch, change.target, old)?;
    reset_shared_index(git, source, repo, change.target)?;
    finish_shared_reset(root)
}

fn reset_shared_index(
    git: &Path,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    base: GitOid,
) -> Result<()> {
    #[cfg(test)]
    if FAIL_SHARED_INDEX_RESET.with(|fault| fault.replace(false)) {
        return Err(RunError::Validation("injected shared index reset failure"));
    }
    let refs_path = source.refs.current_path()?;
    let objects_path = source.objects.current_path()?;
    let alternate = repo.common_git_dir().join("objects");
    let index_path = refs_path.join("index");
    execute_bounded(
        git,
        &refs_path,
        repo.root_path(),
        &args(&["read-tree", &base.to_string()]),
        None,
        Execution {
            object_directory: Some(&objects_path),
            alternate_directory: Some(&alternate),
            common_directory: Some(&refs_path),
            index_file: Some(&index_path),
            ..Execution::default()
        },
    )
    .map_err(|_| RunError::Validation("shared view index reset failed"))?;
    Ok(())
}

fn save_recovery_ref(
    git: &Path,
    source: &ObjectSource,
    repo: &VerifiedRepo,
    tip: GitOid,
) -> Result<()> {
    let refs_path = source.refs.current_path()?;
    let objects_path = source.objects.current_path()?;
    let alternate = repo.common_git_dir().join("objects");
    let recovery = format!("refs/provenant/recovery/{tip}");
    let expected = match private_ref(source, &recovery) {
        Ok(Some(existing)) if existing == tip => tip.to_string(),
        Ok(Some(_)) => {
            return Err(RunError::Validation(
                "shared recovery ref conflicts with retained tip",
            ));
        }
        Ok(None) => "0".repeat(tip.to_string().len()),
        Err(error) => return Err(error),
    };
    execute_bounded(
        git,
        &refs_path,
        repo.root_path(),
        &args(&[
            "update-ref",
            "--no-deref",
            &recovery,
            &tip.to_string(),
            &expected,
        ]),
        None,
        Execution {
            object_directory: Some(&objects_path),
            alternate_directory: Some(&alternate),
            common_directory: Some(&refs_path),
            ..Execution::default()
        },
    )
    .map_err(|_| RunError::Validation("shared view recovery ref creation failed"))?;
    Ok(())
}

pub(crate) fn private_tip(source: &ObjectSource, branch: &str) -> Result<GitOid> {
    let head = source.refs.read_stable(OsStr::new("HEAD"), 1024)?;
    if std::str::from_utf8(&head).map(str::trim).ok() != Some(format!("ref: {branch}").as_str()) {
        return Err(invalid());
    }
    private_ref(source, branch)?.ok_or_else(invalid)
}

fn private_ref(source: &ObjectSource, branch: &str) -> Result<Option<GitOid>> {
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
            let packed = match source
                .refs
                .read_stable(OsStr::new("packed-refs"), 1024 * 1024)
            {
                Ok(packed) => packed,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
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
            let Some(found) = found else {
                return Ok(None);
            };
            found
        }
        Err(error) => return Err(error),
    };
    GitOid::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?.trim())
        .map(Some)
        .map_err(|_| invalid())
}

#[cfg(any(test, feature = "fixture"))]
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
            shared: None,
            extra: Default::default(),
        })?,
        crate::RECORD_LIMIT,
    )
}

fn bind_shared(
    attempt: &Directory,
    held: &Directory,
    private_refs: &Directory,
    private_objects: &Directory,
    shared: SharedViewBinding,
) -> Result<()> {
    let held_path = held.current_path()?;
    let candidate = held_path
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| valid_candidate(name))
        .ok_or_else(invalid)?
        .to_owned();
    let temporary = attempt.open_verified_directory(OsStr::new("tmp"))?;
    let reopened = temporary.open_verified_directory(OsStr::new(&candidate))?;
    if !reopened.metadata()?.same_identity(&held.metadata()?) {
        return Err(invalid());
    }
    crate::artifacts::seal(
        attempt,
        "writer-objects.json",
        &crate::db::bytes(&ObjectBinding {
            schema_version: 3,
            candidate,
            identities: Some(ViewIdentities::read(held, private_refs, private_objects)?),
            shared: Some(shared),
            extra: Default::default(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared: Option<SharedViewBinding>,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SharedViewBinding {
    receipt: SharedViewReceipt,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SharedViewReceipt {
    schema_version: u64,
    root: std::path::PathBuf,
    subject: provenant_records::custody::Subject,
    branch: String,
    object_format: String,
    alternate_objects: std::path::PathBuf,
    identities: SharedViewIdentities,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SharedViewIdentities {
    root: DirectoryIdentity,
    refs: DirectoryIdentity,
    objects: DirectoryIdentity,
    #[serde(default, flatten)]
    extra: provenant_records::ExtraFields,
}

impl PartialEq for SharedViewIdentities {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.refs == other.refs && self.objects == other.objects
    }
}

impl Eq for SharedViewIdentities {}

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

impl SharedViewIdentities {
    fn read(root: &Directory, refs: &Directory, objects: &Directory) -> Result<Self> {
        Ok(Self {
            root: DirectoryIdentity::read(root)?,
            refs: DirectoryIdentity::read(refs)?,
            objects: DirectoryIdentity::read(objects)?,
            extra: Default::default(),
        })
    }
}

const SHARED_VIEW_RECEIPT: &str = "owner.json";
const SHARED_VIEW_INIT_LOCK: &str = "init.lock";

fn object_format(base: GitOid) -> &'static str {
    if base.to_string().len() == 64 {
        "sha256"
    } else {
        "sha1"
    }
}

fn minimal_git_config(object_format: &str) -> &'static [u8] {
    if object_format == "sha256" {
        b"[core]\n\trepositoryformatversion = 1\n\tbare = false\n\thooksPath = /dev/null\n\tfsmonitor = false\n[extensions]\n\tobjectformat = sha256\n"
    } else {
        b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n\thooksPath = /dev/null\n\tfsmonitor = false\n"
    }
}

fn shared_receipt_matches(left: &SharedViewReceipt, right: &SharedViewReceipt) -> bool {
    left.schema_version == right.schema_version
        && left.root == right.root
        && left.subject == right.subject
        && left.branch == right.branch
        && left.object_format == right.object_format
        && left.alternate_objects == right.alternate_objects
        && left.identities == right.identities
}

fn verify_shared_view(receipt: &SharedViewReceipt) -> Result<(Directory, Directory, Directory)> {
    if receipt.schema_version != 1
        || !matches!(receipt.object_format.as_str(), "sha1" | "sha256")
        || !receipt.root.is_absolute()
        || !receipt.alternate_objects.is_absolute()
        || receipt.subject.worktree_identity().is_none()
    {
        return Err(invalid());
    }
    let root = Directory::open_anchor(&receipt.root)?;
    let root_metadata = root.metadata()?;
    if root.current_path()? != receipt.root
        || root_metadata.kind != FileKind::Directory
        || root_metadata.uid != provenant_os::fs::effective_uid()
        || root_metadata.mode & 0o077 != 0
        || DirectoryIdentity::read(&root)? != receipt.identities.root
    {
        return Err(invalid());
    }
    let receipt_metadata = root.metadata_at(OsStr::new(SHARED_VIEW_RECEIPT), false)?;
    if receipt_metadata.kind != FileKind::Regular
        || receipt_metadata.uid != provenant_os::fs::effective_uid()
        || receipt_metadata.links != 1
        || receipt_metadata.mode & 0o077 != 0
    {
        return Err(invalid());
    }
    let raw = root.read_stable(OsStr::new(SHARED_VIEW_RECEIPT), 16 * 1024)?;
    let stored: SharedViewReceipt = provenant_records::decode(
        raw.as_slice(),
        provenant_records::Limits {
            max_bytes: 16 * 1024,
            ..Default::default()
        },
    )?;
    if !shared_receipt_matches(&stored, receipt) {
        return Err(invalid());
    }
    let refs = root.open_verified_directory(OsStr::new("git-common"))?;
    let objects = root.open_verified_directory(OsStr::new("git-objects"))?;
    if DirectoryIdentity::read(&refs)? != receipt.identities.refs
        || DirectoryIdentity::read(&objects)? != receipt.identities.objects
    {
        return Err(invalid());
    }
    let config = refs.read_stable(OsStr::new("config"), 1024)?;
    if config != minimal_git_config(&receipt.object_format)
        || refs.metadata_at(OsStr::new("config"), false)?.mode & 0o222 != 0
    {
        return Err(invalid());
    }
    for denied in [
        "commondir",
        "commondir.lock",
        "gitdir",
        "gitdir.lock",
        "config.worktree",
        "config.worktree.lock",
    ] {
        match refs.metadata_at(OsStr::new(denied), false) {
            Err(error) if error.is_not_found() => {}
            _ => return Err(invalid()),
        }
    }
    let info = objects.open_verified_directory(OsStr::new("info"))?;
    match info.metadata_at(OsStr::new("http-alternates"), false) {
        Err(error) if error.is_not_found() => {}
        Ok(_) | Err(_) => return Err(invalid()),
    }
    let alternates_name = OsStr::new("alternates");
    let alternates_metadata = info.metadata_at(alternates_name, false)?;
    let expected_alternates = format!("{}\n", quote_alternate(&receipt.alternate_objects));
    if alternates_metadata.kind != FileKind::Regular
        || alternates_metadata.uid != provenant_os::fs::effective_uid()
        || alternates_metadata.links != 1
        || alternates_metadata.mode & 0o222 != 0
        || info.read_stable(alternates_name, 16 * 1024)?.as_slice()
            != expected_alternates.as_bytes()
    {
        return Err(invalid());
    }
    let alternate_objects = Directory::open_anchor(&receipt.alternate_objects)?;
    if alternate_objects.metadata()?.kind != FileKind::Directory
        || alternate_objects.metadata()?.uid != provenant_os::fs::effective_uid()
    {
        return Err(invalid());
    }
    Ok((root, refs, objects))
}

pub(crate) struct SharedPreparation {
    pub base: GitOid,
    pub recovered: Option<GitOid>,
}

fn open_or_create_shared_view(
    git: &Path,
    repo: &VerifiedRepo,
    shared_root: &Path,
    branch: &str,
    base: GitOid,
    custody_slot: Option<&Directory>,
    current_attempt: Option<&str>,
) -> Result<(SharedViewBinding, SharedPreparation)> {
    repo.revalidate().map_err(|_| invalid())?;
    if !shared_root.is_absolute() {
        return Err(invalid());
    }
    let root = Directory::open_anchor(shared_root)?;
    let root_path = root.current_path()?;
    let root_metadata = root.metadata()?;
    if root_metadata.kind != FileKind::Directory
        || root_metadata.uid != provenant_os::fs::effective_uid()
        || root_metadata.mode & 0o077 != 0
    {
        return Err(invalid());
    }
    let clock = provenant_os::clock::SystemClock::new();
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5).map_err(|_| invalid())?;
    let _initialise = root.lock(
        OsStr::new(SHARED_VIEW_INIT_LOCK),
        provenant_os::fs::LockMode::Exclusive,
        budget,
        &CancelToken::new(),
        &clock,
    )?;
    let subject =
        provenant_confinement::worktrees::worktree_subject(repo).map_err(|_| invalid())?;
    let format = object_format(base);
    let alternate_objects = repo.common_git_dir().join("objects");
    if !subject
        .worktree_identity()
        .is_some_and(|identity| Path::new(&identity.path) == repo.root_path())
    {
        return Err(invalid());
    }
    match root.read_stable(OsStr::new(SHARED_VIEW_RECEIPT), 16 * 1024) {
        Ok(raw) => {
            let receipt: SharedViewReceipt = provenant_records::decode(
                raw.as_slice(),
                provenant_records::Limits {
                    max_bytes: 16 * 1024,
                    ..Default::default()
                },
            )?;
            if receipt.root != root_path
                || receipt.subject != subject
                || receipt.branch != branch
                || receipt.object_format != format
                || receipt.alternate_objects != alternate_objects
            {
                return Err(invalid());
            }
            let (_, refs, objects) = verify_shared_view(&receipt)?;
            let source = ObjectSource { objects, refs };
            let current_tip = private_tip(&source, branch)?;
            let recovered = resume_shared_reset(git, &root, &source, repo, current_tip)?;
            if current_tip == base {
                return Ok((
                    SharedViewBinding { receipt },
                    SharedPreparation { base, recovered },
                ));
            }
            if shared_is_ancestor(git, &source, repo, current_tip, base)? {
                reconcile_shared_tip(
                    git,
                    &root,
                    &source,
                    repo,
                    branch,
                    SharedIndexReset {
                        schema_version: 1,
                        target: base,
                        recovered,
                    },
                    current_tip,
                )?;
                return Ok((
                    SharedViewBinding { receipt },
                    SharedPreparation { base, recovered },
                ));
            }
            let live = custody_slot
                .map(|slot| {
                    provenant_supervisor::custody::other_holders_live(slot, current_attempt)
                })
                .transpose()?
                .unwrap_or(false);
            if live {
                if shared_is_ancestor(git, &source, repo, base, current_tip)? {
                    return Ok((
                        SharedViewBinding { receipt },
                        SharedPreparation {
                            base: current_tip,
                            recovered,
                        },
                    ));
                }
                return Err(RunError::SharedViewBaseMismatch);
            }
            save_recovery_ref(git, &source, repo, current_tip)?;
            reconcile_shared_tip(
                git,
                &root,
                &source,
                repo,
                branch,
                SharedIndexReset {
                    schema_version: 1,
                    target: base,
                    recovered: Some(current_tip),
                },
                current_tip,
            )?;
            return Ok((
                SharedViewBinding { receipt },
                SharedPreparation {
                    base,
                    recovered: Some(current_tip),
                },
            ));
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    for name in ["git-common", "git-objects"] {
        let child = match root.open_verified_directory(OsStr::new(name)) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => directory(&root, OsStr::new(name))?,
            Err(error) => return Err(error.into()),
        };
        let metadata = child.metadata()?;
        let entries = child
            .entries()?
            .collect::<provenant_os::fs::Result<Vec<_>>>()?;
        if metadata.uid != provenant_os::fs::effective_uid()
            || metadata.mode & 0o077 != 0
            || !entries.is_empty()
        {
            return Err(invalid());
        }
    }
    let minimal = minimal_git_config(format);
    prepare_refs_from(
        git,
        repo.common_dir(),
        repo.private_dir(),
        repo.common_git_dir(),
        repo.root_path(),
        &root,
        &format!("ref: {branch}\n"),
        Some((branch, base)),
        Some(minimal),
    )?;
    let objects = directory(&root, OsStr::new("git-objects"))?;
    let info = directory(&objects, OsStr::new("info"))?;
    let alternate_contents = format!("{}\n", quote_alternate(&alternate_objects));
    info.atomic_write_with_permissions(
        OsStr::new("alternates"),
        ".objects-",
        alternate_contents.as_bytes(),
        0o444,
        PublishMode::NoReplace,
    )?
    .require_durable()?;
    let refs = root.open_verified_directory(OsStr::new("git-common"))?;
    refs.sync()?;
    objects.sync()?;
    let receipt = SharedViewReceipt {
        schema_version: 1,
        root: root_path,
        subject,
        branch: branch.to_owned(),
        object_format: format.to_owned(),
        alternate_objects,
        identities: SharedViewIdentities::read(&root, &refs, &objects)?,
        extra: Default::default(),
    };
    root.atomic_write(
        OsStr::new(SHARED_VIEW_RECEIPT),
        ".worktree-git-",
        &crate::db::bytes(&receipt)?,
        PublishMode::NoReplace,
    )?
    .require_durable()?;
    root.sync()?;
    verify_shared_view(&receipt)?;
    repo.revalidate().map_err(|_| invalid())?;
    Ok((
        SharedViewBinding { receipt },
        SharedPreparation {
            base,
            recovered: None,
        },
    ))
}

fn valid_candidate(name: &str) -> bool {
    name.strip_prefix("candidate-")
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(any(test, feature = "fixture"))]
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

pub(crate) struct SharedViewLaunch<'a> {
    pub root: &'a Path,
    pub custody_slot: Option<&'a Directory>,
    pub current_attempt: Option<&'a str>,
}

pub(crate) fn prepare_shared_bound(
    git: &Path,
    repo: &VerifiedRepo,
    attempt: &Directory,
    scratch: &Path,
    branch: &str,
    base: GitOid,
    launch: SharedViewLaunch<'_>,
) -> Result<SharedPreparation> {
    repo.revalidate().map_err(|_| invalid())?;
    let (shared, effective_base) = open_or_create_shared_view(
        git,
        repo,
        launch.root,
        branch,
        base,
        launch.custody_slot,
        launch.current_attempt,
    )?;
    let (held, private_refs, private_objects) =
        prepare_candidate(git, repo, attempt, scratch, branch, effective_base.base)?;
    bind_shared(attempt, &held, &private_refs, &private_objects, shared)?;
    Ok(effective_base)
}

pub(super) fn object_binding(attempt: &Directory) -> Result<Option<ObjectBinding>> {
    let Some(bytes) =
        crate::artifacts::read_sealed(attempt, "writer-objects.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    // Keep the records crate's bounded, duplicate-key-refusing decoder while
    // admitting the persisted private and shared view versions.
    let binding: ObjectBinding = provenant_records::decode(
        bytes.as_slice(),
        provenant_records::Limits {
            max_bytes: crate::RECORD_LIMIT,
            ..Default::default()
        },
    )?;
    let valid_shape = match binding.schema_version {
        1 => binding.identities.is_none() && binding.shared.is_none(),
        2 => binding.identities.is_some() && binding.shared.is_none(),
        3 => binding.identities.is_some() && binding.shared.is_some(),
        _ => false,
    };
    if !valid_candidate(&binding.candidate) || !valid_shape {
        return Err(invalid());
    }
    Ok(Some(binding))
}

pub(crate) fn bound_source(attempt: &Directory) -> Result<Option<ObjectSource>> {
    let Some(binding) = object_binding(attempt)? else {
        return Ok(None);
    };
    let candidate = binding.candidate;
    let shared = binding.shared;
    // Open every lane-controlled component without following links.
    let temporary = attempt.open_verified_directory(OsStr::new("tmp"))?;
    let scratch = temporary.open_verified_directory(OsStr::new(&candidate))?;
    let objects = scratch.open_verified_directory(OsStr::new("git-objects"))?;
    let refs = scratch.open_verified_directory(OsStr::new("git-common"))?;
    // Version 1 runs predate identity sealing. Preserve their no-follow recovery
    // path; newer private and shared launches seal their identities.
    if let Some(expected) = binding.identities
        && expected != ViewIdentities::read(&scratch, &refs, &objects)?
    {
        return Err(RunError::Validation("private Git view identity changed"));
    }
    if let Some(shared) = shared {
        let (_, refs, objects) = verify_shared_view(&shared.receipt)?;
        return Ok(Some(ObjectSource { objects, refs }));
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

#[cfg(test)]
mod stored_shape_tests {
    use super::*;

    #[test]
    fn object_binding_preserves_additive_fields_and_declared_identity() {
        let payload = br#"{
            "schema_version":2,
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
            binding.identities.as_ref().expect("schema 2 identities"),
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
