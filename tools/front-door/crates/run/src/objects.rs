//! Hash-check untrusted lane objects in a host-owned snapshot before publication.
use crate::{Result, RunError};
use provenant_confinement::repo::VerifiedRepo;
use provenant_core::GitOid;
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{Directory, FileKind, LockGuard, LockMode, PublishMode, entry_names},
};
use std::{
    ffi::{OsStr, OsString},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

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
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(120));
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

fn verify(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    head: GitOid,
    alternate: Option<&Path>,
) -> Result<()> {
    // Callers supply a fresh bare view without lane refs or a lane index.
    // --full hashes loose contents and packed objects, including alternates.
    command(
        git,
        git_dir,
        cwd,
        &args(&[
            "fsck",
            "--full",
            "--no-reflogs",
            "--no-dangling",
            &head.to_string(),
        ]),
        alternate,
    )?;
    let kind = command(
        git,
        git_dir,
        cwd,
        &args(&["cat-file", "-t", &head.to_string()]),
        alternate,
    )?;
    if kind != b"commit\n" {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn verify_graph(git: &Path, git_dir: &Path, cwd: &Path, head: GitOid) -> Result<()> {
    // Linked worktrees keep their objects in the common directory.
    let common = command(
        git,
        git_dir,
        cwd,
        &args(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
        None,
    )?;
    use std::os::unix::ffi::OsStringExt;
    let common = common.strip_suffix(b"\n").ok_or_else(invalid)?;
    let common = OsString::from_vec(common.to_vec());
    let directory = Directory::open_anchor(Path::new(&common))?;
    let _lock = object_lock(&directory, LockMode::Shared)?;
    verify_shared(git, &directory, Path::new(&common), cwd, head)
}

fn object_lock(common: &Directory, mode: LockMode) -> Result<LockGuard> {
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
    Ok(common.lock_existing(
        name,
        mode,
        WaitBudget::cli(clock.mono_now(), 600).map_err(|_| invalid())?,
        &CancelToken::new(),
        &clock,
    )?)
}

pub(crate) fn resolve_ref(git: &Path, repo: &VerifiedRepo, reference: &str) -> Result<GitOid> {
    let bytes = command(
        git,
        repo.private_git_dir(),
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

fn copy(source: &Directory, destination: &Directory, name: &OsStr, budget: &mut u64) -> Result<()> {
    let mut input = source.open_read(name)?;
    let before = input.metadata()?;
    if before.kind != FileKind::Regular || before.links != 1 || before.size > *budget {
        return Err(invalid());
    }
    *budget -= before.size;
    let mut output = destination.create_new(name)?;
    let count = std::io::copy(&mut (&mut input).take(before.size + 1), &mut output)?;
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
    command(git, staging_path, cwd, &init, None)?;
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
) -> Result<()> {
    let name = provenant_os::fs::temporary_name()?;
    common.mkdir(&name, 0o700)?;
    let view = common.open_verified_directory(&name)?;
    let path = common_path.join(&name);
    let result = initialise_view(git, common, cwd, &view, &path, head)
        .and_then(|()| verify(git, &path, cwd, head, Some(&common_path.join("objects"))));
    let cleanup = crate::artifacts::remove_entry_tree(common, &name);
    result?;
    cleanup?;
    Ok(())
}

/// `source` belongs to an absent lane; `staging` is fresh, outside every lane's
/// grants. Never follow its alternates, trust its indices or publish its inodes.
fn import_objects(
    git: &Path,
    repo: &VerifiedRepo,
    source: &Directory,
    staging: &Directory,
    staging_path: &Path,
    head: GitOid,
) -> Result<()> {
    repo.revalidate().map_err(|_| invalid())?;
    // A verified loose graph is published as several atomic creations. Keep
    // concurrent verifiers from observing an incompletely published graph.
    let _lock = object_lock(repo.common_dir(), LockMode::Exclusive)?;
    initialise_view(
        git,
        repo.common_dir(),
        repo.root_path(),
        staging,
        staging_path,
        head,
    )?;
    let objects = staging.open_verified_directory(OsStr::new("objects"))?;
    let shared = repo.common_git_dir().join("objects");
    let mut bytes = MAX_BYTES;
    let mut files = 0_usize;
    for fanout in entry_names(source, MAX_FILES)? {
        files += 1;
        if files > MAX_FILES {
            return Err(invalid());
        }
        if !hex(&fanout, 2) && fanout != "pack" {
            continue;
        }
        let from = source.open_verified_directory(&fanout)?;
        let to = directory(&objects, &fanout)?;
        for name in entry_names(&from, MAX_FILES)? {
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
            copy(&from, &to, &name, &mut bytes)?;
            if pack {
                let mut index = args(&["index-pack", "--strict", "--threads=1"]);
                index.push(
                    staging_path
                        .join("objects")
                        .join(&fanout)
                        .join(&name)
                        .into(),
                );
                let digest = command(git, staging_path, repo.root_path(), &index, Some(&shared))?;
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
        }
        to.sync()?;
    }
    verify(git, staging_path, repo.root_path(), head, Some(&shared))?;
    let shared_dir = repo
        .common_dir()
        .open_verified_directory(OsStr::new("objects"))?;
    // Snapshot is verified in full before the first publication. Existing
    // shared objects always win; verify the resulting store before consuming it.
    for fanout in entry_names(&objects, 258)? {
        if !hex(&fanout, 2) && fanout != "pack" {
            continue;
        }
        let from = objects.open_verified_directory(&fanout)?;
        let to = directory(&shared_dir, &fanout)?;
        let mut names = entry_names(&from, MAX_FILES)?;
        // A pack becomes discoverable with its index. Publish the complete
        // pack first so concurrent host readers never see an index alone.
        names.sort_by_key(|name| Path::new(name).extension() == Some(OsStr::new("idx")));
        for name in names {
            match from.rename_noreplace(&name, &to, &name) {
                Ok(()) => {}
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
        }
        to.sync()?;
    }
    shared_dir.sync()?;
    verify_shared(
        git,
        repo.common_dir(),
        repo.common_git_dir(),
        repo.root_path(),
        head,
    )
}

/// Only the host's sealed launch binding chooses the source, never lane output.
pub(crate) fn settle(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
) -> Result<()> {
    if let Some(source) = source {
        let name = provenant_os::fs::temporary_name()?;
        repo.common_dir().mkdir(&name, 0o700)?;
        let staging = repo.common_dir().open_verified_directory(&name)?;
        let staging_path = repo.common_git_dir().join(&name);
        let result = import_objects(git, repo, source, &staging, &staging_path, head);
        let cleanup = crate::artifacts::remove_entry_tree(repo.common_dir(), &name);
        result?;
        cleanup?;
    } else {
        verify_graph(git, repo.private_git_dir(), repo.root_path(), head)?;
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

pub(crate) fn bound_source(attempt: &Directory) -> Result<Option<ObjectSource>> {
    let bytes = match attempt.read_stable(OsStr::new("writer-objects.json"), crate::RECORD_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let binding: ObjectBinding = crate::db::decode(&bytes)?;
    let candidate = binding.candidate;
    if !valid_candidate(&candidate) {
        return Err(invalid());
    }
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
