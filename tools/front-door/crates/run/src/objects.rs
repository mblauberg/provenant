//! Import lane objects through Git's quarantined receive path.
use crate::{Result, RunError};
use provenant_confinement::repo::VerifiedRepo;
use provenant_core::{CancelToken, Clock, GitOid};
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{Directory, FileKind, PublishMode},
};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    time::Duration,
};

fn invalid() -> RunError {
    RunError::Validation("lane Git import failed")
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(Into::into).collect()
}

pub(crate) fn command(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    _: Option<&Path>,
) -> Result<Vec<u8>> {
    execute(git, git_dir, cwd, arguments, None)
}

fn execute(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    cancel: Option<&CancelToken>,
) -> Result<Vec<u8>> {
    execute_bounded(git, git_dir, cwd, arguments, cancel, Execution::default())
}

struct Execution<'a> {
    timeout: Duration,
    discard: bool,
    input: Option<&'a [u8]>,
    clean_stderr: bool,
}

impl Default for Execution<'_> {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            discard: false,
            input: None,
            clean_stderr: false,
        }
    }
}

fn execute_bounded(
    git: &Path,
    git_dir: &Path,
    cwd: &Path,
    arguments: &[OsString],
    cancel: Option<&CancelToken>,
    settings: Execution<'_>,
) -> Result<Vec<u8>> {
    let env: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin:/bin"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", "/dev/null"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", ""),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    // argv confines GIT_DIR to the sender; receive-pack opens the destination.
    let argv: Vec<OsString> = std::iter::once(git.into())
        .chain([OsString::from("--git-dir"), git_dir.into()])
        .chain(args(&[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ]))
        .chain(arguments.iter().cloned())
        .collect();
    let executor = SystemExecutor::new(1).map_err(|_| invalid())?;
    let mut options = ExecOptions::new(&argv, cwd, settings.timeout);
    options.env = Some(&env);
    options.cancel = cancel;
    options.process_group = true;
    options.stdout = if settings.discard {
        StreamPolicy::Discard
    } else {
        StreamPolicy::Capture {
            limit: 64 * 1024 * 1024,
        }
    };
    options.stdin = settings.input;
    options.stderr = StreamPolicy::Tail { limit: 4096 };
    let output = executor.execute(options).map_err(|_| invalid())?;
    if output.cancelled {
        return Err(RunError::Validation("lane import cancelled"));
    }
    if output.timed_out {
        return Err(RunError::ObjectImportLimit(
            "Git operation exceeded its byte-scaled time budget",
        ));
    }
    if output.termination != Termination::Exit(0)
        || output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.stderr.read_failed
        || settings.clean_stderr && !output.stderr.bytes.is_empty()
    {
        if String::from_utf8_lossy(&output.stderr.bytes)
            .contains("pack exceeds maximum allowed size")
        {
            return Err(RunError::ObjectImportLimit(
                "incoming pack exceeds the lane object byte limit",
            ));
        }
        return Err(RunError::OperationIo {
            operation: "import lane Git objects",
            path: Some(git_dir.into()),
            source: std::io::Error::other(
                String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
            ),
        });
    }
    Ok(output.stdout.bytes)
}

fn quote_alternate(path: &Path) -> String {
    let mut value = String::from("\"");
    for byte in path.as_os_str().as_encoded_bytes() {
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

// Shared objects are host-owned. receive-pack checks newly introduced content
// and connectivity; delivery never hashes unrelated shared history.
pub(crate) fn require_commit(
    git: &Path,
    common: &Path,
    cwd: &Path,
    head: GitOid,
    base: Option<GitOid>,
    cancel: Option<&CancelToken>,
) -> Result<()> {
    execute(
        git,
        common,
        cwd,
        &args(&["cat-file", "-e", &format!("{head}^{{commit}}")]),
        cancel,
    )?;
    let mut arguments = args(&[
        "rev-list",
        "--objects",
        "--missing=error",
        "--no-object-names",
        &head.to_string(),
    ]);
    if let Some(base) = base {
        arguments.extend(args(&["--not", &base.to_string()]));
    }
    let objects = execute(git, common, cwd, &arguments, cancel)?;
    // rev-list checks links but does not inflate blob payloads. Read only this
    // delta so corrupt reachable shared blobs still fail before delivery.
    execute_bounded(
        git,
        common,
        cwd,
        &args(&["cat-file", "--batch", "--buffer"]),
        cancel,
        Execution {
            discard: true,
            input: Some(&objects),
            clean_stderr: true,
            ..Execution::default()
        },
    )
    .map(drop)
}

#[cfg(test)]
pub(crate) fn verify_graph(git: &Path, common: &Path, cwd: &Path, head: GitOid) -> Result<()> {
    require_commit(git, common, cwd, head, None, None)
}

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
    let descends = |ancestor: GitOid| {
        execute(
            git,
            repo.common_git_dir(),
            repo.root_path(),
            &args(&[
                "merge-base",
                "--is-ancestor",
                &ancestor.to_string(),
                &head.to_string(),
            ]),
            None,
        )
        .is_ok()
    };
    if !descends(base) && !resolve_ref(git, repo, "refs/remotes/origin/HEAD").is_ok_and(descends) {
        return Err(RunError::Validation(
            "lane tip must descend from its launch base or origin default branch",
        ));
    }
    // receive-pack has checked the graph. Git's CAS protects concurrent host
    // updates, including when a lane rebases or merges its private branch.
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
    command(
        git,
        repo.common_git_dir(),
        repo.root_path(),
        &args(&["update-ref", "-d", &format!("refs/provenant/import/{head}")]),
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

// Validate every entry before Git can traverse it. The provider is dead, but
// descriptor-relative opens still protect against links in persisted input.
fn object_bytes(directory: &Directory, depth: usize) -> Result<u64> {
    if depth > 128 {
        return Err(RunError::Validation(
            "lane object store nesting exceeds limit",
        ));
    }
    let mut bytes = 0_u64;
    for entry in directory.entries()? {
        let entry = entry?;
        let metadata = directory.metadata_at(&entry, false)?;
        let size = match metadata.kind {
            FileKind::Regular => metadata.size,
            FileKind::Directory => {
                object_bytes(&directory.open_verified_directory(&entry)?, depth + 1)?
            }
            _ => {
                return Err(RunError::Validation(
                    "lane object store contains a symlink or special file",
                ));
            }
        };
        bytes = bytes.checked_add(size).ok_or(RunError::ObjectImportLimit(
            "lane object byte count overflow",
        ))?;
    }
    Ok(bytes)
}

fn sanitise_source(source: &Directory, repo: &VerifiedRepo) -> Result<u64> {
    object_bytes(source, 0)?;
    if let Ok(info) = source.open_verified_directory(OsStr::new("info")) {
        match info.read_stable(OsStr::new("alternates"), 1024 * 1024) {
            Ok(bytes) => {
                let shared = repo.common_git_dir().join("objects");
                // Older launches used an unquoted path. Only the host's own
                // alternate is accepted; a lane-supplied foreign path fails.
                if bytes != format!("{}\n", quote_alternate(&shared)).as_bytes()
                    && bytes != format!("{}\n", shared.display()).as_bytes()
                    && !bytes.is_empty()
                {
                    return Err(RunError::Validation("lane object alternates are untrusted"));
                }
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        for name in [
            "alternates",
            "http-alternates",
            "commit-graph",
            "commit-graphs",
        ] {
            crate::artifacts::remove_entry_tree(&info, OsStr::new(name))?;
        }
    }
    for name in ["commit-graph", "multi-pack-index"] {
        crate::artifacts::remove_entry_tree(source, OsStr::new(name))?;
    }
    if let Ok(pack) = source.open_verified_directory(OsStr::new("pack")) {
        for entry in pack.entries()? {
            let entry = entry?;
            if entry.as_encoded_bytes().starts_with(b"multi-pack-index") {
                crate::artifacts::remove_entry_tree(&pack, &entry)?;
            }
        }
    }
    object_bytes(source, 0)
}

fn import_limits(bytes: u64) -> Result<(u64, Duration)> {
    // Repacking can expand a delta and protocol overhead is non-zero, even
    // for an empty private store. Keep the cap tied to persisted lane bytes.
    let cap = bytes
        .checked_mul(4)
        .and_then(|n| n.checked_add(16 * 1024 * 1024))
        .ok_or(RunError::ObjectImportLimit(
            "lane object byte limit overflow",
        ))?;
    let seconds = 120_u64.saturating_add(cap.div_ceil(1024 * 1024));
    Ok((cap, Duration::from_secs(seconds)))
}

fn receiver_args(cap: u64) -> Vec<OsString> {
    args(&[
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "receive.fsckObjects=true",
        "-c",
        "transfer.fsckObjects=true",
        "-c",
        "receive.unpackLimit=1",
        "-c",
        "transfer.unpackLimit=1",
        "-c",
        &format!("receive.maxInputSize={cap}"),
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "receive-pack",
    ])
}

const STALE_IMPORT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn stale_tree(directory: &Directory, cutoff: i128, depth: usize) -> Result<bool> {
    if depth > 128 || directory.metadata()?.modified_ns >= cutoff {
        return Ok(false);
    }
    for entry in directory.entries()? {
        let entry = entry?;
        let metadata = directory.metadata_at(&entry, false)?;
        if metadata.modified_ns >= cutoff {
            return Ok(false);
        }
        if metadata.kind == FileKind::Directory
            && !stale_tree(
                &directory.open_verified_directory(&entry)?,
                cutoff,
                depth + 1,
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn sweep_stale(repo: &VerifiedRepo) -> Result<()> {
    let now = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map_err(|_| RunError::HostClock)?
        .as_micros();
    let cutoff = i128::from(now) * 1000
        - i128::try_from(STALE_IMPORT_AGE.as_nanos()).map_err(|_| invalid())?;
    let objects = repo
        .common_dir()
        .open_verified_directory(OsStr::new("objects"))?;
    for (parent, prefix) in [
        (repo.common_dir(), b"tmp-provenant-import-".as_slice()),
        (repo.common_dir(), b".provenant-staging.".as_slice()),
        (&objects, b"incoming-".as_slice()),
    ] {
        for entry in parent.entries()? {
            let entry = entry?;
            if entry.as_encoded_bytes().starts_with(prefix)
                && parent.metadata_at(&entry, false)?.kind == FileKind::Directory
                && stale_tree(&parent.open_verified_directory(&entry)?, cutoff, 0)?
            {
                crate::artifacts::remove_entry_tree(parent, &entry)?;
            }
        }
    }
    Ok(())
}

/// Use a clean host-owned sender view, never mutable lane configuration.
/// receive-pack quarantines objects until integrity and connectivity pass.
#[cfg(test)]
pub(crate) fn settle(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
) -> Result<()> {
    settle_cancel(git, repo, source, head, None, None)
}

pub(crate) fn settle_cancel(
    git: &Path,
    repo: &VerifiedRepo,
    source: Option<&Directory>,
    head: GitOid,
    cancel: Option<&CancelToken>,
    custody: Option<&provenant_supervisor::custody::Recovery>,
) -> Result<()> {
    let source = source.ok_or(RunError::Validation("writer has no private object directory; recover its commits on the host and start a fresh run"))?;
    repo.revalidate().map_err(|_| invalid())?;
    let bytes = sanitise_source(source, repo)?;
    let (cap, timeout) = import_limits(bytes)?;
    sweep_stale(repo)?;
    let name = OsString::from(format!(
        "tmp-provenant-import-{}",
        provenant_os::fs::temporary_name()?.to_string_lossy()
    ));
    repo.common_dir().mkdir(&name, 0o700)?;
    let view = repo.common_dir().open_verified_directory(&name)?;
    let path = repo.common_git_dir().join(&name);
    let result: Result<()> = (|| {
        execute(
            git,
            &path,
            repo.root_path(),
            &args(&[
                "init",
                "--bare",
                "--quiet",
                "--template=",
                &format!(
                    "--object-format={}",
                    if head.to_string().len() == 64 {
                        "sha256"
                    } else {
                        "sha1"
                    }
                ),
                path.to_str().ok_or_else(invalid)?,
            ]),
            cancel,
        )?;
        let objects = view.open_verified_directory(OsStr::new("objects"))?;
        let info = objects.open_verified_directory(OsStr::new("info"))?;
        info.atomic_write(
            OsStr::new("alternates"),
            ".import-",
            format!(
                "{}\n{}\n",
                quote_alternate(&source.current_path()?),
                quote_alternate(&repo.common_git_dir().join("objects"))
            )
            .as_bytes(),
            PublishMode::Replace,
        )?
        .require_durable()?;
        match repo
            .common_dir()
            .read_stable(OsStr::new("shallow"), 1024 * 1024)
        {
            Ok(bytes) => view
                .atomic_write(
                    OsStr::new("shallow"),
                    ".import-",
                    &bytes,
                    PublishMode::NoReplace,
                )?
                .require_durable()?,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let receiver = std::iter::once(git.as_os_str().to_owned())
            .chain(receiver_args(cap))
            .map(|arg| format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let stage = format!("refs/provenant/import/{head}");
        let mut push = args(&[
            "-c",
            "protocol.file.allow=always",
            "push",
            "--no-verify",
            "--porcelain",
        ]);
        push.push(format!("--receive-pack={receiver}").into());
        push.push(repo.common_git_dir().into());
        push.push(format!("{head}:{stage}").into());
        // Git resolves the alternate path again. Recheck custody immediately
        // before letting the sender access any lane-controlled path.
        if custody.is_some_and(|custody| {
            custody.observe() != provenant_supervisor::custody::RecoveryState::Absent
        }) {
            return Err(RunError::Validation(
                "writer object import requires absent custody",
            ));
        }
        execute_bounded(
            git,
            &path,
            repo.root_path(),
            &push,
            cancel,
            Execution {
                timeout,
                ..Execution::default()
            },
        )?;
        Ok(())
    })();
    let cleanup = crate::artifacts::remove_entry_tree(repo.common_dir(), &name);
    result?;
    cleanup?;
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

pub(crate) fn restore_index(source: &ObjectSource, repo: &VerifiedRepo) -> Result<()> {
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
    let git = provenant_os::git::git_executable().ok_or_else(invalid)?;
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
