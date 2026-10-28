//! Hash-check untrusted lane objects in a host-owned snapshot before publication.
use crate::{Result, RunError};
use provenant_confinement::repo::VerifiedRepo;
use provenant_core::GitOid;
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{Directory, FileKind, LockGuard, LockMode, Metadata, PublishMode, entry_names},
};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_PACKS: usize = 64;
const IMPORT_BUDGET: Duration = Duration::from_secs(120);
const LOCK_BUDGET: Duration = Duration::from_secs(120);
const GIB: u64 = 1024 * 1024 * 1024;
const SMALL_FILE_HASH_LIMIT: u64 = 1024 * 1024;
pub(crate) const STAGED_BYTES_LIMIT_REASON: &str =
    "staged object hashing exceeded verification budget";
pub(crate) const VERIFICATION_DEADLINE_REASON: &str =
    "verification deadline exceeded; retry settlement";
const STAGING_PREFIX: &str = ".provenant-object-import-";
const STALE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STALE_SWEEP: usize = 32;
const MAX_FILES: usize = 100_000;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn invalid() -> RunError {
    RunError::Validation("lane object verification failed")
}

/// No inherited object paths, replacements, grafts, hooks, lazy fetching or
/// configuration helpers. A fixed object directory also bypasses lane HEAD/index.
fn command(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    args: &[OsString],
    alternate: Option<&Path>,
) -> Result<Vec<u8>> {
    command_before(git, git_dir, cwd, args, alternate, None)
}

// Import deadlines are a host real-time boundary.
#[allow(clippy::disallowed_methods)]
fn remaining(deadline: Option<Deadline>) -> Result<Duration> {
    match deadline {
        Some(deadline) => deadline
            .at
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| deadline.expired()),
        None => Ok(Duration::from_secs(120)),
    }
}

#[derive(Clone, Copy)]
struct Deadline {
    at: Instant,
    import: bool,
}

impl Deadline {
    fn expired(self) -> RunError {
        if self.import {
            RunError::ObjectImportLimit("copy/index-pack 120 s wall-clock budget exceeded")
        } else {
            RunError::ObjectVerificationRetry(VERIFICATION_DEADLINE_REASON)
        }
    }
}

/// Each verification gets 120 s plus 60 s per started GiB, capped at one hour.
fn verification_budget(bytes: u64) -> Duration {
    Duration::from_secs(
        120_u64
            .saturating_add(bytes.div_ceil(GIB).saturating_mul(60))
            .min(3600),
    )
}

#[allow(clippy::disallowed_methods)]
fn verification_deadline(generation: &[(PathBuf, Metadata)], extra: u64) -> Deadline {
    let bytes = generation
        .iter()
        .filter(|(_, m)| m.kind == FileKind::Regular)
        .fold(extra, |total, (_, m)| total.saturating_add(m.size));
    #[cfg(test)]
    let budget = TEST_VERIFICATION_BUDGET
        .get()
        .unwrap_or_else(|| verification_budget(bytes));
    #[cfg(not(test))]
    let budget = verification_budget(bytes);
    Deadline {
        at: Instant::now() + budget,
        import: false,
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_VERIFICATION_BUDGET: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// The unlocked full check hashes every object. The locked pass only walks
/// connectivity: 30 seconds plus 30 per started GiB, capped at five minutes.
#[allow(clippy::disallowed_methods)]
fn connectivity_deadline(generation: &[(PathBuf, Metadata)], extra: u64) -> Deadline {
    let bytes = generation
        .iter()
        .filter(|(_, m)| m.kind == FileKind::Regular)
        .fold(extra, |total, (_, m)| total.saturating_add(m.size));
    let seconds = 30_u64
        .saturating_add(bytes.div_ceil(GIB).saturating_mul(30))
        .min(300);
    Deadline {
        at: Instant::now() + Duration::from_secs(seconds),
        import: false,
    }
}

fn command_before(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    args: &[OsString],
    alternate: Option<&Path>,
    deadline: Option<Deadline>,
) -> Result<Vec<u8>> {
    let mut env: Vec<(OsString, OsString)> = [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", "/dev/null"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_ALLOW_PROTOCOL", ""),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    env.push(("GIT_DIR".into(), git_dir.into()));
    if args.first().is_none_or(|arg| arg != "rev-parse") {
        env.push((
            "GIT_OBJECT_DIRECTORY".into(),
            git_dir.join("objects").into(),
        ));
    }
    if let Some(alternate) = alternate {
        env.push((
            "GIT_ALTERNATE_OBJECT_DIRECTORIES".into(),
            quote_alternate(alternate).into(),
        ));
    }
    let argv: Vec<OsString> = std::iter::once(git.into())
        .chain(
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "fsck.skipList=/dev/null",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
            ]
            .into_iter()
            .map(Into::into),
        )
        .chain(args.iter().cloned())
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| invalid())?;
    let mut options = ExecOptions::new(&argv, cwd, remaining(deadline)?);
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture {
        limit: if args.first().is_some_and(|arg| arg == "for-each-ref") {
            1024 * 1024
        } else {
            4096
        },
    };
    options.stderr = StreamPolicy::Tail { limit: 4096 };
    options.stop_on_output_limit = true;
    let output = executor.execute(options).map_err(|_| invalid())?;
    if output.timed_out
        && let Some(deadline) = deadline
    {
        return Err(deadline.expired());
    }
    remaining(deadline)?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.cancelled
        || output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.stderr.read_failed
    {
        return Err(RunError::OperationIo {
            operation: "verify lane objects",
            path: Some(git_dir.into()),
            source: std::io::Error::other(format!(
                "Git {:?}: {:?}: {}",
                args.first(),
                output.termination,
                String::from_utf8_lossy(&output.stderr.bytes)
            )),
        });
    }
    Ok(output.stdout.bytes)
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(Into::into).collect()
}

fn quote_alternate(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut value = String::from("\"");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'"' | b'\\' => {
                value.push('\\');
                value.push(char::from(*byte));
            }
            0x20..=0x7e => value.push(char::from(*byte)),
            _ => value.push_str(&format!("\\{byte:03o}")),
        }
    }
    value.push('"');
    value
}

#[derive(Clone, Copy)]
struct Verification {
    deadline: Deadline,
    mode: &'static str,
}

fn verify(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    head: GitOid,
    alternate: Option<&Path>,
    check: Verification,
) -> Result<()> {
    // Callers supply a fresh bare view without lane refs or a lane index.
    // The full mode hashes loose contents and packed objects, including
    // alternates. Publication only needs connectivity after the snapshot hash.
    command_before(
        git,
        git_dir,
        cwd,
        &args(&[
            "fsck",
            check.mode,
            "--strict",
            "--no-reflogs",
            "--no-dangling",
            &head.to_string(),
        ]),
        alternate,
        Some(check.deadline),
    )?;
    let kind = command_before(
        git,
        git_dir,
        cwd,
        &args(&["cat-file", "-t", &head.to_string()]),
        alternate,
        Some(check.deadline),
    )?;
    if kind != b"commit\n" {
        return Err(invalid());
    }
    Ok(())
}

/// The caller supplies the host's common directory, never a worktree admin
/// directory whose lane-writable commondir could redirect verification.
pub(crate) fn verify_graph(git: &Path, common_path: &Path, cwd: &Path, head: GitOid) -> Result<()> {
    VerificationCache::default().verify_graph(git, common_path, cwd, head)
}

/// Results live only for one delivery. Inspect metadata under the store lock
/// before every lookup, including loose files for legacy/external host writers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VerificationCache {
    generation: Option<(PathBuf, Vec<(PathBuf, Metadata)>)>,
    heads: Vec<GitOid>,
}

impl VerificationCache {
    pub(crate) fn verify_graph(
        &mut self,
        git: &Path,
        common_path: &Path,
        cwd: &Path,
        head: GitOid,
    ) -> Result<()> {
        let common = Directory::open_anchor(common_path)?;
        let _lock = object_lock(&common, LockMode::Shared)?;
        for retry in 0..2 {
            let generation = (common_path.to_path_buf(), store_generation(&common)?);
            if self.generation.as_ref() != Some(&generation) {
                self.heads.clear();
                self.generation = None;
            } else if self.heads.contains(&head) && !shared_alternates(&common)? {
                return Ok(());
            }
            let result = verify_shared(
                git,
                &common,
                common_path,
                cwd,
                head,
                Verification {
                    deadline: verification_deadline(&generation.1, 0),
                    mode: "--full",
                },
            );
            if store_generation(&common)? != generation.1 {
                self.heads.clear();
                self.generation = None;
                if retry == 0 {
                    continue;
                }
                return Err(RunError::ObjectVerificationRetry(
                    "shared object store changed during verification; retry settlement",
                ));
            }
            result?;
            self.generation = Some(generation);
            self.heads.push(head);
            return Ok(());
        }
        Err(invalid())
    }
}

fn shared_alternates(common: &Directory) -> Result<bool> {
    let objects = common.open_verified_directory(OsStr::new("objects"))?;
    let info = match objects.open_verified_directory(OsStr::new("info")) {
        Ok(info) => info,
        Err(error) if error.is_not_found() => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match info.metadata_at(OsStr::new("alternates"), false) {
        Ok(metadata) => Ok(metadata.size != 0),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn store_generation(common: &Directory) -> Result<Vec<(PathBuf, Metadata)>> {
    fn collect(
        directory: &Directory,
        prefix: &Path,
        result: &mut Vec<(PathBuf, Metadata)>,
        depth: usize,
    ) -> Result<()> {
        if depth > 4 {
            return Err(invalid());
        }
        result.push((prefix.to_path_buf(), directory.metadata()?));
        for name in entry_names(directory, MAX_FILES)? {
            let metadata = directory.metadata_at(&name, false)?;
            let path = prefix.join(&name);
            if metadata.kind == FileKind::Directory {
                collect(
                    &directory.open_verified_directory(&name)?,
                    &path,
                    result,
                    depth + 1,
                )?;
            } else if metadata.kind == FileKind::Regular {
                result.push((path, metadata));
            } else {
                return Err(invalid());
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    // Creating a verification view changes the common directory's metadata,
    // but its identity still belongs in the generation.
    let mut identity = common.metadata()?;
    identity.modified_ns = 0;
    identity.changed_ns = 0;
    identity.size = 0;
    identity.allocated = 0;
    identity.links = 0;
    result.push((PathBuf::new(), identity));
    collect(
        &common.open_verified_directory(OsStr::new("objects"))?,
        Path::new("objects"),
        &mut result,
        0,
    )?;
    match common.metadata_at(OsStr::new("shallow"), false) {
        Ok(metadata) if metadata.kind == FileKind::Regular => {
            result.push(("shallow".into(), metadata))
        }
        Ok(_) => return Err(invalid()),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

fn object_lock(common: &Directory, mode: LockMode) -> Result<LockGuard> {
    #[cfg(test)]
    if let Some(budget) = TEST_LOCK_BUDGET.get() {
        #[allow(clippy::disallowed_methods)]
        return object_lock_before(common, mode, Some(Instant::now() + budget));
    }
    object_lock_before(common, mode, None)
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_LOCK_BUDGET: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

#[allow(clippy::disallowed_methods)]
fn object_lock_before(
    common: &Directory,
    mode: LockMode,
    deadline: Option<Instant>,
) -> Result<LockGuard> {
    use provenant_core::{CancelToken, Clock, WaitBudget};
    let clock = provenant_os::clock::SystemClock::new();
    let name = OsStr::new("provenant-objects.lock");
    match common.create_new(name) {
        Ok(file) => {
            file.sync()?;
            common.sync()?;
        }
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let duration = deadline.map_or(Ok(LOCK_BUDGET), |at| {
        at.checked_duration_since(Instant::now())
            .ok_or(RunError::ObjectStoreBusy)
    })?;
    let seconds = duration.as_secs().max(1);
    let lock = common.lock_existing(
        name,
        mode,
        WaitBudget::cli(clock.mono_now(), seconds).map_err(|_| invalid())?,
        &CancelToken::new(),
        &clock,
    );
    match lock {
        Ok(lock) => Ok(lock),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => Err(RunError::ObjectStoreBusy),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn resolve_ref(git: &Path, repo: &VerifiedRepo, reference: &str) -> Result<GitOid> {
    let bytes = command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["rev-parse", "--verify", "--end-of-options", reference]),
        None,
    )?;
    GitOid::parse(std::str::from_utf8(&bytes).map_err(|_| invalid())?.trim()).map_err(|_| invalid())
}

/// Keep private object tips out of the shared ref namespace: Git fetch's
/// connectivity walk otherwise fails on another live lane's missing objects.
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

/// Read only the selected raw ref, through held directories. Running Git with
/// the lane's copied configuration would grant its callbacks host authority.
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

pub(crate) fn publish_branch(
    git: &Path,
    repo: &VerifiedRepo,
    branch: &str,
    base: GitOid,
    head: GitOid,
) -> Result<()> {
    let current = resolve_ref(git, repo, branch)?;
    if current != base && current != head {
        return Err(invalid());
    }
    // Content is verified before publication. Ancestry is certified after
    // quarantine so a lane rewind can archive evidence and fail terminally.
    command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&[
            "update-ref",
            "--no-deref",
            "-m",
            "provenant: verified lane objects",
            branch,
            &head.to_string(),
            &current.to_string(),
        ]),
        None,
    )?;
    Ok(())
}

fn directory(parent: &Directory, name: &OsStr) -> Result<Directory> {
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(parent.open_verified_directory(name)?)
}

fn hex(name: &OsStr, length: usize) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes.len() == length
        && bytes
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

fn copy(
    source: &Directory,
    destination: &Directory,
    name: &OsStr,
    budget: &mut u64,
    deadline: Option<Deadline>,
) -> Result<()> {
    let mut input = source.open_read(name)?;
    let before = input.metadata()?;
    if before.kind != FileKind::Regular || before.links != 1 || before.size > *budget {
        return Err(invalid());
    }
    *budget -= before.size;
    let mut output = destination.create_new(name)?;
    let mut count = 0;
    let mut buffer = [0_u8; 64 * 1024];
    let mut reader = (&mut input).take(before.size + 1);
    loop {
        remaining(deadline)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        count += read as u64;
    }
    if count != before.size || !before.unchanged(&input.metadata()?) {
        return Err(invalid());
    }
    output.flush()?;
    output.sync()?;
    Ok(())
}

fn initialise_view(
    git: &Path,
    common: &Directory,
    cwd: &Path,
    staging: &Directory,
    staging_path: &Path,
    head: GitOid,
    deadline: Option<Deadline>,
) -> Result<()> {
    let format = if head.to_string().len() == 64 {
        "sha256"
    } else {
        "sha1"
    };
    let mut init = args(&[
        "init",
        "--bare",
        "--quiet",
        "--template=",
        &format!("--object-format={format}"),
    ]);
    init.push(staging_path.to_path_buf().into());
    command_before(git, staging_path, cwd, &init, None, deadline)?;
    // Preserve only the host's trusted shallow boundary. A lane cannot turn a
    // missing parent into an accepted boundary through its private metadata.
    match common.read_stable(OsStr::new("shallow"), 1024 * 1024) {
        Ok(bytes) => staging
            .atomic_write(
                OsStr::new("shallow"),
                ".objects-",
                &bytes,
                PublishMode::NoReplace,
            )?
            .require_durable()?,
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Hash the shared store through an empty bare view. Git's reference database
/// checks must not inspect another worktree's unfinished operation markers.
/// The empty primary store also prevents private duplicates from shadowing
/// corrupt shared objects. The caller holds the shared object-store lock.
fn verify_shared(
    git: &Path,
    common: &Directory,
    common_path: &Path,
    cwd: &Path,
    head: GitOid,
    check: Verification,
) -> Result<()> {
    let name = staging_name()?;
    common.mkdir(&name, 0o700)?;
    let view = common.open_verified_directory(&name)?;
    let path = common_path.join(&name);
    let result = initialise_view(git, common, cwd, &view, &path, head, Some(check.deadline))
        .and_then(|()| {
            verify(
                git,
                &path,
                cwd,
                head,
                Some(&common_path.join("objects")),
                check,
            )
        });
    let cleanup = crate::artifacts::remove_entry_tree(common, &name);
    result?;
    cleanup?;
    Ok(())
}

/// `source` belongs to an absent lane; `staging` is fresh, outside every lane's
/// grants. Never follow its alternates, trust its indices or publish its inodes.
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
fn import_objects(
    git: &Path,
    repo: &VerifiedRepo,
    source: &Directory,
    staging: &Directory,
    staging_path: &Path,
    head: GitOid,
) -> Result<()> {
    import_before(
        git,
        repo,
        source,
        staging,
        staging_path,
        head,
        Instant::now() + IMPORT_BUDGET,
    )
}

fn import_before(
    git: &Path,
    repo: &VerifiedRepo,
    source: &Directory,
    staging: &Directory,
    staging_path: &Path,
    head: GitOid,
    deadline: Instant,
) -> Result<()> {
    let deadline = Some(Deadline {
        at: deadline,
        import: true,
    });
    remaining(deadline)?;
    repo.revalidate().map_err(|_| invalid())?;
    // Count all packs before copying or invoking Git.
    let mut packs = 0;
    let mut files = 0;
    let mut plan = Vec::new();
    for fanout in entry_names(source, MAX_FILES)? {
        remaining(deadline)?;
        files += 1;
        if !hex(&fanout, 2) && fanout != "pack" {
            continue;
        }
        let from = source.open_verified_directory(&fanout)?;
        for name in entry_names(&from, MAX_FILES)? {
            remaining(deadline)?;
            files += 1;
            if files > MAX_FILES {
                return Err(invalid());
            }
            let pack = fanout == "pack";
            let valid = if pack {
                name.to_str()
                    .and_then(|s| s.strip_prefix("pack-"))
                    .and_then(|s| s.strip_suffix(".pack"))
                    .is_some_and(|s| hex(OsStr::new(s), head.to_string().len()))
            } else {
                hex(&name, head.to_string().len() - 2)
            };
            if !valid {
                continue;
            }
            if pack {
                packs += 1;
                if packs > MAX_PACKS {
                    return Err(RunError::ObjectImportLimit(
                        "pack-count cap exceeded (maximum 64)",
                    ));
                }
            }
            plan.push((fanout.clone(), name, pack));
        }
    }
    initialise_view(
        git,
        repo.common_dir(),
        repo.root_path(),
        staging,
        staging_path,
        head,
        deadline,
    )?;
    let objects = staging.open_verified_directory(OsStr::new("objects"))?;
    let shared = repo.common_git_dir().join("objects");
    let mut bytes = MAX_BYTES;
    for (fanout, name, pack) in plan {
        remaining(deadline)?;
        let from = source.open_verified_directory(&fanout)?;
        let to = directory(&objects, &fanout)?;
        copy(&from, &to, &name, &mut bytes, deadline)?;
        if pack {
            let mut index = args(&["index-pack", "--strict", "--threads=1"]);
            index.push(
                staging_path
                    .join("objects")
                    .join(&fanout)
                    .join(&name)
                    .into(),
            );
            let digest = command_before(
                git,
                staging_path,
                repo.root_path(),
                &index,
                Some(&shared),
                deadline,
            )?;
            if digest
                != format!(
                    "{}\n",
                    name.to_string_lossy()
                        .trim_start_matches("pack-")
                        .trim_end_matches(".pack")
                )
                .as_bytes()
            {
                return Err(invalid());
            }
        }
        to.sync()?;
    }
    remaining(deadline)?;
    // Bind the files before Git hashes them. A still-live pre-v11 lane can
    // reach the common directory even though current lane policies cannot.
    let snapshot = snapshot_files(&objects)?;
    let staged_bytes = snapshot.iter().fold(0_u64, |total, file| {
        total.saturating_add(file.stamp.metadata.size)
    });
    let mut retried = false;
    let (_generation, _lock, connectivity) = loop {
        let generation = store_generation(repo.common_dir())?;
        let check = Verification {
            deadline: verification_deadline(&generation, staged_bytes),
            mode: "--full",
        };
        let result = verify(
            git,
            staging_path,
            repo.root_path(),
            head,
            Some(&shared),
            check,
        );
        // Only publication and the final shared check exclude other lanes.
        let lock = object_lock(repo.common_dir(), LockMode::Exclusive)?;
        let connectivity = connectivity_deadline(&generation, staged_bytes);
        for file in &snapshot {
            file.check(&objects, connectivity)?;
        }
        if store_generation(repo.common_dir())? != generation {
            if !retried {
                retried = true;
                continue;
            }
            return Err(RunError::ObjectVerificationRetry(
                "shared object store changed during verification; retry settlement",
            ));
        }
        result?;
        break (generation, lock, connectivity);
    };
    let shared_dir = repo
        .common_dir()
        .open_verified_directory(OsStr::new("objects"))?;
    // Snapshot is verified in full before the first publication. Existing
    // shared objects always win; verify the resulting store before consuming it.
    let mut published = Vec::new();
    let mut fanouts = BTreeSet::new();
    for file in snapshot {
        remaining(Some(connectivity))?;
        let from = objects.open_verified_directory(&file.fanout)?;
        let to = directory(&shared_dir, &file.fanout)?;
        fanouts.insert(file.fanout.clone());
        file.check(&objects, connectivity)?;
        match from.rename_noreplace(&file.name, &to, &file.name) {
            Ok(()) => {
                let stamp = file_stamp(&to, &file.name, true, Some(connectivity.at), false)?;
                // Rename changes ctime. All other read-relevant metadata,
                // identity and the inexpensive content digest must match.
                if !renamed_matches(&file.stamp, &stamp) {
                    return Err(staged_changed());
                }
                published.push(StagedFile { stamp, ..file });
            }
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
    }
    for fanout in fanouts {
        shared_dir.open_verified_directory(&fanout)?.sync()?;
    }
    shared_dir.sync()?;
    for file in &published {
        file.check(&shared_dir, connectivity)?;
    }
    verify_shared(
        git,
        repo.common_dir(),
        repo.common_git_dir(),
        repo.root_path(),
        head,
        Verification {
            deadline: connectivity,
            mode: "--connectivity-only",
        },
    )?;
    for file in &published {
        file.check(&shared_dir, connectivity)?;
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    metadata: Metadata,
    digest: Option<[u8; 32]>,
}

fn renamed_matches(before: &FileStamp, after: &FileStamp) -> bool {
    let mut expected = before.clone();
    expected.metadata.changed_ns = after.metadata.changed_ns;
    expected == *after
}

struct StagedFile {
    fanout: OsString,
    name: OsString,
    stamp: FileStamp,
}

fn staged_changed() -> RunError {
    RunError::Validation("staged object file changed during verification")
}

#[allow(clippy::disallowed_methods)]
fn file_stamp(
    directory: &Directory,
    name: &OsStr,
    hash_large: bool,
    deadline: Option<Instant>,
    staged_budget: bool,
) -> Result<FileStamp> {
    use sha2::{Digest as _, Sha256};
    let metadata = directory.metadata_at(name, false)?;
    if metadata.kind != FileKind::Regular || metadata.links != 1 {
        return Err(staged_changed());
    }
    let digest = if hash_large || metadata.size <= SMALL_FILE_HASH_LIMIT {
        let mut file = directory.open_read(name)?;
        if !metadata.unchanged(&file.metadata()?) {
            return Err(staged_changed());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut count = 0_u64;
        loop {
            if deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(if staged_budget {
                    RunError::ObjectImportLimit(STAGED_BYTES_LIMIT_REASON)
                } else {
                    RunError::ObjectVerificationRetry(VERIFICATION_DEADLINE_REASON)
                });
            }
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count.saturating_add(read as u64);
            if count > metadata.size {
                return Err(staged_changed());
            }
            hash.update(&buffer[..read]);
        }
        if count != metadata.size || !metadata.unchanged(&file.metadata()?) {
            return Err(staged_changed());
        }
        Some(hash.finalize().into())
    } else {
        None
    };
    if !metadata.unchanged(&directory.metadata_at(name, false)?) {
        return Err(staged_changed());
    }
    Ok(FileStamp { metadata, digest })
}

impl StagedFile {
    fn check(&self, objects: &Directory, deadline: Deadline) -> Result<()> {
        remaining(Some(deadline))?;
        let directory = objects.open_verified_directory(&self.fanout)?;
        let current = file_stamp(&directory, &self.name, false, Some(deadline.at), false)?;
        if self.stamp.metadata != current.metadata
            || (current.digest.is_some() && self.stamp.digest != current.digest)
        {
            return Err(staged_changed());
        }
        Ok(())
    }
}

#[allow(clippy::disallowed_methods)]
fn snapshot_files(objects: &Directory) -> Result<Vec<StagedFile>> {
    let mut names = Vec::new();
    let mut bytes = 0_u64;
    for fanout in entry_names(objects, 258)? {
        if !hex(&fanout, 2) && fanout != "pack" {
            continue;
        }
        let directory = objects.open_verified_directory(&fanout)?;
        for name in entry_names(&directory, MAX_FILES)? {
            bytes = bytes.saturating_add(directory.metadata_at(&name, false)?.size);
            names.push((fanout.clone(), name));
        }
    }
    // The staged bytes alone must be hashable within their verification budget.
    let deadline = Instant::now() + verification_budget(bytes);
    let mut files = Vec::new();
    for (fanout, name) in names {
        let directory = objects.open_verified_directory(&fanout)?;
        files.push(StagedFile {
            stamp: file_stamp(&directory, &name, true, Some(deadline), true)?,
            fanout,
            name,
        });
    }
    files.sort_by_key(|file| Path::new(&file.name).extension() == Some(OsStr::new("idx")));
    Ok(files)
}

fn staging_name() -> Result<OsString> {
    let mut name = OsString::from(STAGING_PREFIX);
    name.push(provenant_os::fs::temporary_name()?);
    Ok(name)
}

fn stale_tree_size(
    parent: &Directory,
    name: &OsStr,
    remaining: &mut usize,
    depth: usize,
) -> Result<bool> {
    if depth > 4 || *remaining == 0 {
        return Ok(false);
    }
    *remaining -= 1;
    let metadata = parent.metadata_at(name, false)?;
    if metadata.kind != FileKind::Directory {
        return Ok(true);
    }
    let held = parent.open_verified_directory(name)?;
    for entry in held.entries()? {
        if !stale_tree_size(&held, &entry?, remaining, depth + 1)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Sweep only this host-owned namespace, at most 32 old directories per import.
/// The age exceeds all phase budgets. Cleanup must never prevent settlement.
fn sweep_staging(common: &Directory, now: SystemTime) {
    sweep_staging_with(common, now, |common, name, remaining| {
        if !stale_tree_size(common, name, remaining, 0)? {
            return Ok(false);
        }
        crate::artifacts::remove_entry_tree(common, name)?;
        Ok(true)
    });
}

#[allow(clippy::print_stderr)]
fn sweep_staging_with(
    common: &Directory,
    now: SystemTime,
    mut remove: impl FnMut(&Directory, &OsStr, &mut usize) -> Result<bool>,
) {
    let cutoff = match now.duration_since(UNIX_EPOCH) {
        Ok(time) => time.as_nanos() as i128 - STALE_AGE.as_nanos() as i128,
        Err(error) => {
            eprintln!("warning: object import staging sweep: {error}");
            return;
        }
    };
    let names = match entry_names(common, MAX_FILES) {
        Ok(names) => names,
        Err(error) => {
            eprintln!("warning: object import staging sweep: {error}");
            return;
        }
    };
    let mut attempted = 0;
    let mut entries_left = MAX_FILES;
    for name in names {
        let import_staging = name.to_str().is_some_and(|s| {
            s.strip_prefix(STAGING_PREFIX)
                .and_then(|suffix| suffix.strip_prefix(".provenant-staging."))
                .is_some_and(|suffix| hex(OsStr::new(suffix), 24))
        });
        if !import_staging {
            continue;
        }
        let metadata = match common.metadata_at(&name, false) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => continue,
            Err(error) => {
                eprintln!("warning: object import staging sweep {name:?}: {error}");
                continue;
            }
        };
        if metadata.kind == FileKind::Directory && metadata.modified_ns < cutoff {
            attempted += 1;
            match remove(common, &name, &mut entries_left) {
                Ok(true) => {}
                Ok(false) => break,
                Err(RunError::Filesystem(error)) if error.is_not_found() => {}
                Err(error) => eprintln!("warning: object import staging sweep {name:?}: {error}"),
            }
            if attempted == MAX_STALE_SWEEP || entries_left == 0 {
                break;
            }
        }
    }
}

#[allow(clippy::print_stderr)]
pub(crate) fn legacy_policy_diagnostic(identity: &str) -> String {
    let note = format!(
        "warning: {identity}: legacy pre-v11 Git grants policy retains shared-store write access; running full shared-store verification"
    );
    eprintln!("{note}");
    note
}

/// Only the host's sealed launch binding chooses the source, never lane output.
#[allow(clippy::disallowed_methods)]
pub(crate) fn settle(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
) -> Result<()> {
    if let Some(source) = source {
        sweep_staging(repo.common_dir(), SystemTime::now());
        let name = staging_name()?;
        repo.common_dir().mkdir(&name, 0o700)?;
        let staging = repo.common_dir().open_verified_directory(&name)?;
        let staging_path = repo.common_git_dir().join(&name);
        let result = import_before(
            git,
            repo,
            source,
            &staging,
            &staging_path,
            head,
            Instant::now() + IMPORT_BUDGET,
        );
        let cleanup = crate::artifacts::remove_entry_tree(repo.common_dir(), &name);
        result?;
        cleanup?;
    } else {
        verify_graph(git, repo.common_git_dir(), repo.root_path(), head)?;
    }
    Ok(())
}

pub(crate) fn bind(attempt: &Directory, scratch: &Path) -> Result<()> {
    let candidate = scratch.file_name().ok_or_else(invalid)?;
    let candidate = candidate
        .to_str()
        .filter(|name| valid_candidate(name))
        .ok_or_else(invalid)?;
    crate::artifacts::seal(
        attempt,
        "writer-objects.json",
        &crate::db::bytes(&ObjectBinding {
            schema_version: 1,
            candidate: candidate.into(),
        })?,
        crate::RECORD_LIMIT,
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectBinding {
    schema_version: u64,
    candidate: String,
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
    // Custom provider launches may only edit files and omit the adapter's Git
    // setup. Their empty object store still needs a complete settlement path.
    directory(&held, OsStr::new("git-objects"))?;
    prepare_refs(git, repo, &held, branch, base)?;
    bind(attempt, scratch)
}

pub(crate) fn legacy_policy(attempt: &Directory) -> Result<bool> {
    Ok(object_binding(attempt)?.is_none())
}

fn object_binding(attempt: &Directory) -> Result<Option<ObjectBinding>> {
    let Some(bytes) =
        crate::artifacts::read_sealed(attempt, "writer-objects.json", crate::RECORD_LIMIT)?
    else {
        return Ok(None);
    };
    let binding: ObjectBinding = crate::db::decode(&bytes)?;
    if binding.schema_version != 1 || !valid_candidate(&binding.candidate) {
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
    Ok(Some(ObjectSource { objects, refs }))
}

#[cfg(test)]
#[path = "objects_tests.rs"]
mod tests;
